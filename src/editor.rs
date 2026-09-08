//! The assembled thing: a code editor that keeps a language server told about its buffer,
//! offers what that server would finish a word with, and answers a modifier-click with where
//! the name is defined.
//!
//! Everything below is a matter of wiring the pieces of this crate to each other in the one
//! order they go in - drain what came back, draw, then say what is owed. It is written out
//! here so that a caller with nothing but a repo and a window gets all of it in three lines,
//! and so that a caller with its own worker threads can ignore this file entirely and use
//! [`Served`], [`Completing`] and [`asks_about`](crate::asks_about) directly.

use std::{sync::Arc, time::Instant};

use egui_moon_editor::{Editor, EditorOutput, EditorRequest, EditorStyle, Language, Word};

use crate::{
    asking::{Ask, Asking, Heard, StatusAbout},
    calling::follows_the_caret,
    completing::{AtTheCaret, Completing, CompletingNext, before_the_caret},
    definition::{asks_about, still_starting},
    document::{DocumentAsk, Served},
    source::{LanguageSource, LspLocation, LspPosition, LspStatus},
};

/// What a modifier-click on a name came to.
pub enum Definition {
    /// The server answered. Several places is the several the language really has - a trait
    /// method and the impls of it - and is a choice rather than an answer; empty is a server
    /// that had nothing to say, or one that could not be asked.
    Places {
        /// The name that was clicked.
        word: Word,
        /// Everywhere the server says it is defined.
        places: Vec<LspLocation>,
    },
    /// The server was asked while it was still reading the project and answered with
    /// nothing, which from such a server is the wait rather than an answer. The string is the
    /// sentence to show - see [`still_starting`](crate::still_starting).
    StillStarting(String),
    /// Nothing here serves this file, so there was nobody to ask. The caller says so however
    /// it says such things.
    NoServer(Word),
}

/// A modifier-click whose question is still out.
struct LookingUp {
    /// The name that was clicked. A second click replaces this one, so the first one's answer
    /// is thrown away when it lands - the person has moved on, and landing them on the older
    /// of the two names would be wrong.
    word: Word,
    /// Whether the server had not finished reading the project when it was asked. It was
    /// asked anyway, because the wait is far too long to sit a click out; what has to be
    /// remembered until the answer lands is how an empty one is allowed to read.
    asked_while_starting: bool,
}

/// What drawing a [`CodeEditor`] turned up.
pub struct CodeEditorOutput {
    /// Everything the widget itself reported: the response, the marks it laid out, the row
    /// that was taken from the completion list.
    pub editor: EditorOutput,
    /// Where the file stands with its language server, for a caller that wants to say so.
    pub status: LspStatus,
    /// What a modifier-click came to, on the frame the answer is in hand. Clicking and
    /// landing are different frames: the question is put on a worker thread, and this is
    /// `Some` on whichever later frame it comes back on.
    pub definition: Option<Definition>,
}

/// A code editor that talks to a language server.
///
/// It owns the buffer - through [`egui_moon_editor::Editor`] - and a thread of its own for the
/// questions, which all block. Nothing it does waits on that thread: what has come back is
/// read at the top of a frame, and what is owed is put on the queue at the bottom of one.
///
/// ```no_run
/// # use std::sync::Arc;
/// # use egui_moon_code_ide::{CodeEditor, RegistrySource};
/// # fn build(ctx: &egui::Context) -> CodeEditor {
/// let servers = Arc::new(moon_lsp::LspRegistry::new(std::env::var("PATH").unwrap_or_default()));
/// let source = RegistrySource::for_repo(servers, "/home/dev/repo");
/// let text = std::fs::read_to_string("/home/dev/repo/src/main.rs").unwrap_or_default();
/// CodeEditor::new(ctx, "src/main.rs", text, Arc::new(source))
/// # }
/// ```
pub struct CodeEditor {
    /// The widget. Public through [`CodeEditor::editor`] so a caller can do everything a
    /// plain editor does - set the language, read the text, mark a search's hits.
    editor: Editor,
    /// The file, as the source names it: relative to the repo, which is what a server is
    /// told and what its answers come back as.
    file_path: String,
    asking: Asking,
    /// Whether a server is behind the file, and what it has heard of the buffer.
    served: Served,
    /// What is on offer under the caret, and what has been asked about it.
    completing: Completing,
    /// The characters the server behind this file said open a list on their own, once it has
    /// been asked. Empty until then and empty for a server that named none, which in both
    /// cases means only a word being typed is asked about.
    triggers: Vec<char>,
    /// Whether that question has been put. Once, per file: the answer is the server's and it
    /// does not change while the server runs, so asking again every frame would be a call a
    /// frame for an answer that is already in hand.
    asked_what_opens_a_list: bool,
    /// The modifier-click whose question is out, if one is.
    looking_up: Option<LookingUp>,
    /// An answer that came back this frame, waiting to go out in the output.
    landed: Option<Definition>,
}

impl CodeEditor {
    /// An editor on `text`, asking `source` about `file_path`.
    ///
    /// `file_path` is named the way the source names it, which for a repo is relative to its
    /// root. The context is what the worker thread knocks on when an answer lands: without it
    /// a completion list worked out while nobody is typing would wait for a frame that never
    /// comes.
    pub fn new(
        ctx: &egui::Context,
        file_path: impl Into<String>,
        text: String,
        source: Arc<dyn LanguageSource>,
    ) -> Self {
        let file_path = file_path.into();
        let mut editor = Editor::new(text);
        // What the file is read as. The same path the server is asked about, so the
        // highlighting and the answers are about the same language.
        editor.set_language(Language::of_path(&file_path));
        let wake = ctx.clone();
        Self {
            asking: Asking::new(file_path.clone(), source, move || wake.request_repaint()),
            editor,
            file_path,
            served: Served::default(),
            completing: Completing::default(),
            triggers: Vec::new(),
            asked_what_opens_a_list: false,
            looking_up: None,
            landed: None,
        }
    }

    /// Show `text` as `file_path` instead, which is what following a jump comes to.
    ///
    /// The server is told the old file is closed and the new one is open, so its idea of what
    /// is open stays true; the worker thread carries over rather than a second one being
    /// started, so a window that spends an afternoon jumping around a repo still has the one
    /// thread it opened with; and everything in flight about the old file is thrown away, so
    /// an answer computed about the text that was here cannot land in the text that is here
    /// now.
    ///
    /// The caller reads the text - from disk, from a backend, from wherever the source's
    /// paths mean - because a path is only a name to this crate and the file behind it may
    /// not be one this process can open at all.
    pub fn open(&mut self, file_path: impl Into<String>, text: String) {
        let file_path = file_path.into();
        // The language before the text: `set_text` builds the highlighter for whatever
        // language the editor is on, and a rust file read as the last file's language would
        // be highlighted wrong until something else set it.
        self.editor.set_language(Language::of_path(&file_path));
        self.editor.set_text(text);
        self.asking.asks_about_instead(file_path.clone());
        self.file_path = file_path;
        // A different file is a different question in every one of these: whether anything
        // serves it, what the server has heard of it, what is on offer under the caret, and
        // what a click that is still out was about.
        self.served = Served::default();
        self.completing = Completing::default();
        self.looking_up = None;
        self.landed = None;
        // The file may be another language, and another language is another server with
        // another list of what opens one.
        self.triggers = Vec::new();
        self.asked_what_opens_a_list = false;
    }

    /// The widget, for everything a plain editor does.
    pub fn editor(&self) -> &Editor {
        &self.editor
    }

    /// The widget, to set the language on or put text into.
    pub fn editor_mut(&mut self) -> &mut Editor {
        &mut self.editor
    }

    /// What is in the buffer, which after typing is not what is on disk.
    pub fn text(&self) -> &str {
        self.editor.text()
    }

    /// The file this editor is asking about.
    pub fn file_path(&self) -> &str {
        &self.file_path
    }

    /// Where the file stands with its language server.
    pub fn status(&self) -> LspStatus {
        self.served.status()
    }

    /// Draw a frame: read back what the worker answered, draw the text with whatever is on
    /// offer under the caret, and put what is owed on the queue.
    ///
    /// `request` is the widget's own, passed through untouched but for two fields this editor
    /// owns: `completions`, which is what the server offered, and `navigate_modifier`, which
    /// defaults to command-or-ctrl here because a modifier-click is half of what this crate
    /// is for. Set it yourself to change it, or to `None` to turn navigation off.
    pub fn ui(
        &mut self,
        ui: &mut egui::Ui,
        style: &EditorStyle,
        request: &EditorRequest<'_>,
    ) -> CodeEditorOutput {
        self.read_what_came_back();

        let drawn = EditorRequest {
            completions: self.completing.on_offer(),
            navigate_modifier: request
                .navigate_modifier
                .or(Some(egui::Modifiers::COMMAND)),
            marks: request.marks.clone(),
            line_of_interest: request.line_of_interest,
            focus: request.focus,
        };
        let output = self.editor.ui(ui, style, &drawn);

        let now = Instant::now();
        self.keep_the_document_up(ui.ctx(), now);
        let clicked = output
            .navigated_to
            .clone()
            .and_then(|word| self.look_up(word));
        self.follow_the_caret(ui.ctx(), &output, now);

        CodeEditorOutput {
            status: self.served.status(),
            definition: clicked.or_else(|| self.landed.take()),
            editor: output,
        }
    }

    /// Everything the worker has answered since the last frame.
    fn read_what_came_back(&mut self) {
        for heard in self.asking.heard() {
            match heard {
                Heard::Status {
                    about: StatusAbout::WhetherServed,
                    status,
                } => self.served.served_answered(status),
                Heard::Status {
                    about: StatusAbout::WhetherStillStarting,
                    status,
                } => self.served.starting_answered(status),
                Heard::Told { text, heard: true } => self.served.heard(text),
                Heard::Told { heard: false, .. } => self.served.could_not_be_told(),
                Heard::Definition { word, places } => {
                    // A second click while this one was out. The person has moved on, and
                    // landing them on the older of the two names would be wrong.
                    let Some(asking) = self
                        .looking_up
                        .take_if(|asking| asking.word == word)
                    else {
                        continue;
                    };
                    let places = places.unwrap_or_default();
                    // Nothing from a server that had not read the project yet is the wait
                    // showing through, not an answer about the name.
                    self.landed = Some(match places.is_empty() && asking.asked_while_starting {
                        true => Definition::StillStarting(still_starting(&self.file_path)),
                        false => Definition::Places { word, places },
                    });
                }
                Heard::Triggers(characters) => self.triggers = characters,
                Heard::Completion { asked, rows } => {
                    // What the caret sits in front of, off the buffer as it stands: it is what
                    // keeps a call being completed over from being given a second pair of
                    // parentheses. See [`calling::follows_the_caret`].
                    let follows = follows_the_caret(self.editor.text(), asked.at());
                    self.completing.answered(&asked, rows, follows);
                }
            }
        }
    }

    /// Tell the server what is on screen, when it is owed anything.
    fn keep_the_document_up(&mut self, ctx: &egui::Context, now: Instant) {
        let owed = self.served.owed(self.editor.text(), now);
        if let Some(after) = owed.draw_again_in {
            ctx.request_repaint_after(after);
        }
        let Some(ask) = owed.ask else {
            return;
        };
        self.asking.ask(match ask {
            DocumentAsk::WhetherServed => Ask::Status(StatusAbout::WhetherServed),
            DocumentAsk::WhetherStillStarting => Ask::Status(StatusAbout::WhetherStillStarting),
            DocumentAsk::Send { text, opening } => Ask::Send { text, opening },
        });
    }

    /// A name was modifier-clicked. Ask the server where it is defined, or say there is
    /// nobody to ask.
    ///
    /// A server that has not finished reading the project is asked too - see
    /// [`AsksAbout`](crate::AsksAbout). Whether it had is remembered rather than acted on here, because it only
    /// matters if the answer comes back empty, and that is frames away.
    fn look_up(&mut self, word: Word) -> Option<Definition> {
        let asks = asks_about(self.served.status());
        if !asks.asks() {
            return Some(Definition::NoServer(word));
        }
        let at = LspPosition {
            line: word.at.line,
            column: word.at.column,
        };
        self.looking_up = Some(LookingUp {
            word: word.clone(),
            asked_while_starting: asks.an_empty_answer_is_only_the_wait(),
        });
        self.asking.ask(Ask::Definition { word, at });
        None
    }

    /// Ask what could be typed where the caret is, when it is worth asking.
    fn follow_the_caret(&mut self, ctx: &egui::Context, output: &EditorOutput, now: Instant) {
        // A file nothing serves never asks anything, and this is the whole of what it costs.
        if !self.served.has_a_server() {
            return;
        }
        self.ask_what_opens_a_list();
        let can_answer = self.served.can_answer_about(self.editor.text());
        // What the caret sits behind, off the buffer as it stands: with the server's own
        // list beside it, that is what says whether a `.` just typed is worth a question of
        // its own. See [`AtTheCaret`].
        let at_the_caret = AtTheCaret {
            typed: output
                .caret
                .as_ref()
                .and_then(|caret| {
                    before_the_caret(
                        self.editor.text(),
                        LspPosition {
                            line: caret.line,
                            column: caret.column,
                        },
                    )
                }),
            triggers: &self.triggers,
        };
        match self.completing.follow(output, at_the_caret, can_answer, now) {
            CompletingNext::Nothing => {}
            CompletingNext::Wait => ctx.request_repaint_after(crate::TYPING_SETTLES_IN),
            CompletingNext::Ask(asked) => self.asking.ask(Ask::Completion(asked)),
        }
    }

    /// Ask the server what opens a completion list on its own, once there is a server up to
    /// have said it.
    ///
    /// Waited for rather than asked at once, because the answer comes out of the
    /// `initialize` reply and a server that has not started has not sent one - asking early
    /// would fill this in with the empty list of a server that had simply not spoken yet, and
    /// nothing would ever ask again.
    fn ask_what_opens_a_list(&mut self) {
        if self.asked_what_opens_a_list || self.served.status() != LspStatus::Ready {
            return;
        }
        self.asked_what_opens_a_list = true;
        self.asking.ask(Ask::Triggers);
    }
}

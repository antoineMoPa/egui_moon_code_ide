//! The crate driven end to end without a server, a repo or a window.
//!
//! That this is possible at all is the point of [`LanguageSource`]: a fake that answers out of
//! a table is all it takes to check the parts that are hard to be sure of by reading - that a
//! question the buffer has moved out from under never goes, that the worker takes the newest
//! of several rather than all of them, and that a file that was opened is closed again.

use std::{
    sync::{Arc, Mutex, mpsc},
    thread::ThreadId,
    time::{Duration, Instant},
};

use anyhow::Result;

use crate::{
    Asked, CanAnswer, Definition, DocumentAsk, DocumentOwed, Served,
    asking::{Ask, Asking, Heard, StatusAbout},
    source::{
        LanguageSource, LspCompletion, LspCompletionKind, LspLocation, LspPosition, LspStatus,
    },
};

/// How long a test waits on the worker before giving up. Long enough that a loaded machine
/// does not fail the suite, short enough that a hang is a failure rather than a hang.
const PATIENCE: Duration = Duration::from_secs(5);

/// A language source that answers out of a table and writes down what it was asked.
///
/// `held` is what a call waits on before it answers, so a test can keep the worker busy and
/// see what happens to the questions that pile up behind it.
struct FakeSource {
    asked: Mutex<Vec<String>>,
    held: Mutex<Option<mpsc::Receiver<()>>>,
    /// Which threads the calls came in on, so a test can see that following a jump into
    /// another file went on the thread that was already there rather than a second one.
    threads: Mutex<Vec<ThreadId>>,
    /// Where the file stands with its server, so a test can put a click to one that has not
    /// finished reading the project.
    status: LspStatus,
    /// What `definition` answers with. Empty is a server with nothing to say, which means
    /// completely different things depending on `status` - which is the point of it being
    /// settable.
    places: Vec<LspLocation>,
    /// How many of the next attempts to tell it a document fail, counted down as they do.
    /// A momentary failure is what a review over a network actually sees, and a test that
    /// cannot produce one cannot show that the file comes back from it.
    sends_that_fail: Mutex<usize>,
}

impl FakeSource {
    fn new() -> Arc<Self> {
        Self::answering(
            LspStatus::Ready,
            vec![LspLocation {
                file_path: "src/lib.rs".to_string(),
                line_number: 12,
            }],
        )
    }

    /// A source that says `status` about every file and answers every question about a
    /// definition with `places`.
    fn answering(status: LspStatus, places: Vec<LspLocation>) -> Arc<Self> {
        Arc::new(Self {
            asked: Mutex::new(Vec::new()),
            held: Mutex::new(None),
            threads: Mutex::new(Vec::new()),
            status,
            places,
            sends_that_fail: Mutex::new(0),
        })
    }

    /// Fail the next `count` attempts to tell it a document, as a dropped link does.
    fn fails_the_next_sends(&self, count: usize) {
        *self.sends_that_fail.lock().unwrap() = count;
    }

    /// Whether this attempt to tell it a document is one of the ones that fail.
    fn this_send_fails(&self) -> bool {
        let mut left = self.sends_that_fail.lock().unwrap();
        match *left {
            0 => false,
            _ => {
                *left -= 1;
                true
            }
        }
    }

    /// Hold the first call up until the returned handle is dropped or used.
    fn holds_the_first_call(&self) -> mpsc::Sender<()> {
        let (release, held) = mpsc::channel();
        *self.held.lock().unwrap() = Some(held);
        release
    }

    /// Written down before the call is held up, so a test can see that the worker is inside
    /// this one and pile the next questions up behind it.
    fn note(&self, what: String) {
        let mut threads = self.threads.lock().unwrap();
        let on = std::thread::current().id();
        if !threads.contains(&on) {
            threads.push(on);
        }
        drop(threads);
        self.asked.lock().unwrap().push(what);
        if let Some(held) = self.held.lock().unwrap().take() {
            let _ = held.recv();
        }
    }

    fn asked(&self) -> Vec<String> {
        self.asked.lock().unwrap().clone()
    }

    /// How many threads have put a question to this source.
    fn threads_used(&self) -> usize {
        self.threads.lock().unwrap().len()
    }

    /// Wait until the source has been asked something that reads like `what`.
    fn waits_to_be_asked(&self, what: &str) {
        let deadline = Instant::now() + PATIENCE;
        while Instant::now() < deadline {
            if self.asked().iter().any(|asked| asked.contains(what)) {
                return;
            }
            std::thread::sleep(Duration::from_millis(5));
        }
        panic!("never asked about {what}; asked {:?}", self.asked());
    }
}

impl LanguageSource for FakeSource {
    fn status(&self, file_path: &str) -> LspStatus {
        self.note(format!("status {file_path}"));
        self.status
    }

    fn did_open(&self, file_path: &str, text: &str) -> Result<()> {
        self.note(format!("open {file_path} {text}"));
        if self.this_send_fails() {
            anyhow::bail!("the link dropped on the way to the server");
        }
        Ok(())
    }

    fn did_change(&self, file_path: &str, text: &str) -> Result<()> {
        self.note(format!("change {file_path} {text}"));
        if self.this_send_fails() {
            anyhow::bail!("the link dropped on the way to the server");
        }
        Ok(())
    }

    fn did_close(&self, file_path: &str) -> Result<()> {
        self.note(format!("close {file_path}"));
        Ok(())
    }

    fn definition(&self, file_path: &str, at: LspPosition) -> Result<Vec<LspLocation>> {
        self.note(format!("definition {file_path} {}:{}", at.line, at.column));
        Ok(self.places.clone())
    }

    fn completion(&self, file_path: &str, at: LspPosition) -> Result<Vec<LspCompletion>> {
        self.note(format!("completion {file_path} {}:{}", at.line, at.column));
        Ok(vec![LspCompletion {
            label: "greet".to_string(),
            detail: None,
            insert: "greet".to_string(),
            kind: Some(LspCompletionKind::Function),
        }])
    }
}

/// The next answer off the worker, or a failure rather than a wait forever.
fn next_answer(asking: &Asking) -> Heard {
    let deadline = Instant::now() + PATIENCE;
    while Instant::now() < deadline {
        if let Some(heard) = asking.heard().next() {
            return heard;
        }
        std::thread::sleep(Duration::from_millis(5));
    }
    panic!("the worker never answered");
}

/// The whole round trip, on a thread, with nothing but a fake behind it: a question goes, an
/// answer comes back, and the window is knocked on so the frame that reads it gets drawn.
#[test]
fn a_question_is_answered_on_the_worker_and_the_window_is_woken_when_it_lands() {
    let source = FakeSource::new();
    let (woken, was_woken) = mpsc::channel();
    let asking = Asking::new("src/main.rs", Arc::clone(&source) as Arc<dyn LanguageSource>, move || {
        let _ = woken.send(());
    });

    asking.ask(Ask::Status(StatusAbout::WhetherServed));
    let heard = next_answer(&asking);
    assert!(matches!(
        heard,
        Heard::Status {
            about: StatusAbout::WhetherServed,
            status: LspStatus::Ready
        }
    ));
    assert_eq!(source.asked(), ["status src/main.rs"]);
    was_woken
        .recv_timeout(PATIENCE)
        .expect("an answer that lands while nobody is typing has to ask for a frame");
}

/// A question waiting to go out is replaced by the one that supersedes it rather than queued
/// behind it. Two letters typed while the server is busy are one question, not two: the
/// answer to the first would be a list of names for text that no longer exists.
#[test]
fn a_question_still_waiting_to_go_out_is_replaced_by_the_one_that_supersedes_it() {
    let source = FakeSource::new();
    let release = source.holds_the_first_call();
    let asking = Asking::new("src/main.rs", Arc::clone(&source) as Arc<dyn LanguageSource>, || {});

    // The worker is inside the status call and cannot take anything else.
    asking.ask(Ask::Status(StatusAbout::WhetherServed));
    source.waits_to_be_asked("status");
    asking.ask(Ask::Completion(Asked::about("gre", 3, 3)));
    asking.ask(Ask::Completion(Asked::about("greet", 3, 5)));
    drop(release);

    source.waits_to_be_asked("completion src/main.rs 3:5");
    // The half-typed word was never asked about at all: it was overwritten where it stood.
    assert!(
        !source
            .asked()
            .iter()
            .any(|asked| asked == "completion src/main.rs 3:3"),
        "asked {:?}",
        source.asked()
    );
}

/// A file the server was told about is told when it goes, and a file it never heard of is not
/// - a `didClose` for a document nobody opened is a message about nothing.
#[test]
fn a_file_that_was_opened_is_closed_when_the_editor_on_it_goes_away() {
    let source = FakeSource::new();
    let asking = Asking::new("src/main.rs", Arc::clone(&source) as Arc<dyn LanguageSource>, || {});
    asking.ask(Ask::Send {
        text: "fn one() {}".to_string(),
        opening: true,
    });
    assert!(matches!(next_answer(&asking), Heard::Told { heard: true, .. }));
    drop(asking);
    source.waits_to_be_asked("close src/main.rs");

    let never_opened = FakeSource::new();
    let asking = Asking::new(
        "notes.md",
        Arc::clone(&never_opened) as Arc<dyn LanguageSource>,
        || {},
    );
    asking.ask(Ask::Status(StatusAbout::WhetherServed));
    next_answer(&asking);
    drop(asking);
    std::thread::sleep(Duration::from_millis(50));
    assert_eq!(never_opened.asked(), ["status notes.md"]);
}

/// The document goes before anything else waiting, because every other question is about a
/// place in text the server has to have been told about already.
#[test]
fn the_text_is_sent_before_any_question_that_is_asked_about_a_place_in_it() {
    let source = FakeSource::new();
    let release = source.holds_the_first_call();
    let asking = Asking::new("src/main.rs", Arc::clone(&source) as Arc<dyn LanguageSource>, || {});

    // Something to hold the worker while the other three pile up behind it.
    asking.ask(Ask::Status(StatusAbout::WhetherServed));
    source.waits_to_be_asked("status");
    asking.ask(Ask::Completion(Asked::about("greet", 3, 5)));
    asking.ask(Ask::Definition {
        word: egui_moon_editor::Word {
            text: "greet".to_string(),
            at: egui_moon_editor::TextPoint {
                offset: 0,
                line: 3,
                column: 0,
            },
        },
        at: LspPosition { line: 3, column: 0 },
    });
    asking.ask(Ask::Send {
        text: "fn greet() {}".to_string(),
        opening: true,
    });
    drop(release);

    source.waits_to_be_asked("completion");
    let asked = source.asked();
    let order: Vec<&str> = asked
        .iter()
        .map(|asked| asked.split(' ').next().unwrap_or_default())
        .collect();
    assert_eq!(order, ["status", "open", "definition", "completion"]);
}

/// Following a jump: the file that was open is closed on the server, the file jumped into is
/// opened, and both went to the source on the one thread the editor started with - a thread
/// per click would be a leak an afternoon of jumping around a repo pays for.
#[test]
fn a_jump_to_another_file_closes_the_old_one_and_opens_the_new_one_on_the_same_thread() {
    let source = FakeSource::new();
    let mut asking = Asking::new(
        "src/main.rs",
        Arc::clone(&source) as Arc<dyn LanguageSource>,
        || {},
    );
    asking.ask(Ask::Send {
        text: "fn one() {}".to_string(),
        opening: true,
    });
    assert!(matches!(
        next_answer(&asking),
        Heard::Told { heard: true, .. }
    ));

    asking.asks_about_instead("src/lib.rs");
    asking.ask(Ask::Send {
        text: "fn two() {}".to_string(),
        opening: true,
    });
    assert!(matches!(
        next_answer(&asking),
        Heard::Told { heard: true, .. }
    ));

    assert_eq!(
        source.asked(),
        [
            "open src/main.rs fn one() {}",
            "close src/main.rs",
            "open src/lib.rs fn two() {}"
        ]
    );
    assert_eq!(source.threads_used(), 1);
}

/// A question that was already out when the editor moved on comes back to nobody. The answer
/// is about a place in text that is no longer on screen, and showing it against the file
/// jumped into would land the person somewhere the server never said.
#[test]
fn an_answer_about_the_file_that_was_left_is_thrown_away_rather_than_shown_against_the_new_one() {
    let source = FakeSource::new();
    let mut asking = Asking::new(
        "src/main.rs",
        Arc::clone(&source) as Arc<dyn LanguageSource>,
        || {},
    );
    asking.ask(Ask::Send {
        text: "fn greet() {}".to_string(),
        opening: true,
    });
    assert!(matches!(
        next_answer(&asking),
        Heard::Told { heard: true, .. }
    ));

    let release = source.holds_the_first_call();
    asking.ask(Ask::Definition {
        word: egui_moon_editor::Word {
            text: "greet".to_string(),
            at: egui_moon_editor::TextPoint {
                offset: 0,
                line: 3,
                column: 0,
            },
        },
        at: LspPosition { line: 3, column: 0 },
    });
    // The worker is inside the definition call when the person jumps somewhere else.
    source.waits_to_be_asked("definition src/main.rs");
    asking.asks_about_instead("src/lib.rs");
    drop(release);
    source.waits_to_be_asked("close src/main.rs");

    // The next answer to reach a frame is about the new file: the definition worked out for
    // the old one was dropped on the way in.
    asking.ask(Ask::Status(StatusAbout::WhetherServed));
    assert!(matches!(next_answer(&asking), Heard::Status { .. }));
}

/// The jump as an editor actually does it, on a bare `egui` context with no window: the text
/// on screen becomes the file jumped into, and the line asked for is laid out - which is what
/// the caller waits on before it stops asking for it, since a line asked for every frame
/// drags the view back to it and the file cannot be scrolled away from.
#[test]
fn an_editor_that_opens_another_file_shows_it_and_lays_out_the_line_the_jump_asked_for() {
    let source = FakeSource::new();
    let ctx = egui::Context::default();
    let mut code = crate::CodeEditor::new(
        &ctx,
        "src/main.rs",
        "fn one() {}\n".to_string(),
        Arc::clone(&source) as Arc<dyn LanguageSource>,
    );

    let jumped_into = (1..=9).fold(String::new(), |mut text, line| {
        text.push_str(&format!("// line {line}\n"));
        text
    });
    code.open("src/deep/other.rs", jumped_into.clone());
    assert_eq!(code.text(), jumped_into);
    assert_eq!(code.file_path(), "src/deep/other.rs");

    // Frames until the line is laid out, which for a file this short is the first of them.
    let input = egui::RawInput {
        screen_rect: Some(egui::Rect::from_min_size(
            egui::Pos2::ZERO,
            egui::vec2(600.0, 400.0),
        )),
        ..Default::default()
    };
    let deadline = Instant::now() + PATIENCE;
    let mut laid_out = false;
    while !laid_out && Instant::now() < deadline {
        let mut frame = ctx.run_ui(input.clone(), |ui| {
            let style = egui_moon_editor::EditorStyle::from_visuals(ui.visuals());
            let output = code.ui(
                ui,
                &style,
                &egui_moon_editor::EditorRequest {
                    line_of_interest: Some(7),
                    ..Default::default()
                },
            );
            laid_out = output.editor.line_at.is_some();
        });
        // Nothing paints this frame, and epaint will not let the fonts it rasterized be
        // dropped on the floor.
        frame.textures_delta.clear();
    }
    assert!(laid_out, "the line the jump asked for was never laid out");
}

/// A frame of a bare context, big enough for a file to be laid out in.
fn a_frame_at(pointer: egui::Pos2, clicking: bool) -> egui::RawInput {
    // The modifier is a state the context holds rather than a field of a frame's input, so
    // every frame says it is still down.
    let mut events = vec![
        egui::Event::ModifiersChanged(egui::Modifiers::COMMAND),
        egui::Event::PointerMoved(pointer),
    ];
    if clicking {
        // Both halves of the click in the one frame: a press and a release is what the widget
        // reads as a click, and a test has no reason to spread them over two.
        for pressed in [true, false] {
            events.push(egui::Event::PointerButton {
                pos: pointer,
                button: egui::PointerButton::Primary,
                pressed,
                modifiers: egui::Modifiers::COMMAND,
            });
        }
    }
    egui::RawInput {
        screen_rect: Some(egui::Rect::from_min_size(
            egui::Pos2::ZERO,
            egui::vec2(600.0, 400.0),
        )),
        events,
        ..Default::default()
    }
}

/// Every spot a name might have been laid out at, coarsely: where the widget puts the first
/// line of a file is the widget's business, so the pointer is walked over the top-left of the
/// text until the editor says there is a name under it rather than arithmetic being done on
/// the layout from out here.
fn spots_a_name_could_be_at() -> impl Iterator<Item = egui::Pos2> {
    (0..80)
        .step_by(4)
        .flat_map(|y| (0..300).step_by(6).map(move |x| egui::pos2(x as f32, y as f32)))
}

/// What a modifier-click on the first name of a file comes to, against a source that says
/// `status` about the file and answers every question about a definition with `places`.
///
/// The whole of the crate, driven on a bare context: the file is opened on the source, the
/// click goes through the widget, the question goes out on the worker thread and the answer
/// comes back on a later frame - which is the only place the reading of an empty answer
/// actually happens.
fn what_a_modifier_click_comes_to(status: LspStatus, places: Vec<LspLocation>) -> Definition {
    let source = FakeSource::answering(status, places);
    let ctx = egui::Context::default();
    let mut code = crate::CodeEditor::new(
        &ctx,
        "src/main.rs",
        "fn greet() {}\n".to_string(),
        Arc::clone(&source) as Arc<dyn LanguageSource>,
    );

    let mut spots = spots_a_name_could_be_at();
    let mut over_a_name: Option<egui::Pos2> = None;
    let mut clicked = false;
    let deadline = Instant::now() + PATIENCE;
    while Instant::now() < deadline {
        // Until a name is found, the pointer walks; once one is, it stays on it and clicks.
        let (pointer, clicking) = match over_a_name {
            Some(at) => (at, !std::mem::replace(&mut clicked, true)),
            None => (
                spots.next().expect("a name was never found under the pointer"),
                false,
            ),
        };
        let mut answer = None;
        let mut frame = ctx.run_ui(a_frame_at(pointer, clicking), |ui| {
            let style = egui_moon_editor::EditorStyle::from_visuals(ui.visuals());
            let output = code.ui(ui, &style, &egui_moon_editor::EditorRequest::default());
            if over_a_name.is_none() && output.editor.navigable_word.is_some() {
                over_a_name = Some(pointer);
            }
            answer = output.definition;
        });
        // Nothing paints these frames, and epaint will not let the fonts it rasterized be
        // dropped on the floor.
        frame.textures_delta.clear();
        if let Some(answer) = answer {
            return answer;
        }
    }
    panic!("the click was never answered; the source was asked {:?}", source.asked());
}

/// The distinction the whole crate exists for, end to end: nothing from a server that has not
/// finished reading the project is the wait, and nothing from one that has read it is the
/// answer. They arrive over the wire as the same empty list.
///
/// And the click is put to a server that is still starting rather than held back until it is
/// ready: rust-analyzer takes the better part of a minute over a cold project, and a click
/// that did nothing for that minute is a feature nobody would believe in. So a starting server
/// that does have an answer answers.
#[test]
fn nothing_from_a_server_still_reading_the_project_is_the_wait_and_not_an_answer() {
    let indexing = what_a_modifier_click_comes_to(LspStatus::Starting, Vec::new());
    assert!(
        matches!(&indexing, Definition::StillStarting(said) if said.contains("still indexing")),
        "a starting server with nothing to say is the wait, saw {}",
        match &indexing {
            Definition::Places { places, .. } => format!("{} places", places.len()),
            Definition::StillStarting(said) => said.clone(),
            Definition::NoServer(word) => format!("no server for {}", word.text),
        }
    );

    let ready = what_a_modifier_click_comes_to(LspStatus::Ready, Vec::new());
    assert!(
        matches!(&ready, Definition::Places { places, .. } if places.is_empty()),
        "a ready server with nothing to say means the name is defined nowhere"
    );

    let answered_early = what_a_modifier_click_comes_to(
        LspStatus::Starting,
        vec![LspLocation {
            file_path: "src/lib.rs".to_string(),
            line_number: 12,
        }],
    );
    assert!(
        matches!(&answered_early, Definition::Places { places, .. } if places.len() == 1),
        "a server that is still starting is asked anyway, and its answer is the answer"
    );
}

/// A file nothing serves has nobody to ask, and the click says so rather than going quiet.
#[test]
fn a_modifier_click_in_a_file_nothing_serves_says_there_is_no_server() {
    let answer = what_a_modifier_click_comes_to(LspStatus::Unavailable, Vec::new());
    assert!(matches!(&answer, Definition::NoServer(word) if word.text == "fn" || word.text == "greet"));
}

/// The document half of a frame, over and over without a window: whatever the file owes its
/// server goes to the worker, whatever the worker answered is written back, until `done` says
/// there is nothing left to wait for. It is what [`crate::CodeEditor`] does every frame with
/// the drawing taken out.
fn the_document_is_kept_up_until(
    served: &mut Served,
    asking: &Asking,
    text: &str,
    done: impl Fn(&Served) -> bool,
) -> bool {
    let deadline = Instant::now() + PATIENCE;
    while Instant::now() < deadline {
        if done(served) {
            return true;
        }
        if let Some(ask) = served.owed(text, Instant::now()).ask {
            asking.ask(match ask {
                DocumentAsk::WhetherServed => Ask::Status(StatusAbout::WhetherServed),
                DocumentAsk::WhetherStillStarting => {
                    Ask::Status(StatusAbout::WhetherStillStarting)
                }
                DocumentAsk::Send { text, opening } => Ask::Send { text, opening },
            });
        }
        for heard in asking.heard() {
            match heard {
                Heard::Status {
                    about: StatusAbout::WhetherServed,
                    status,
                } => served.served_answered(status),
                Heard::Status {
                    about: StatusAbout::WhetherStillStarting,
                    status,
                } => served.starting_answered(status),
                Heard::Told { text, heard: true } => served.heard(text),
                Heard::Told { heard: false, .. } => served.could_not_be_told(),
                Heard::Definition { .. }
                | Heard::Completion { .. }
                | Heard::Triggers(_) => {}
            }
        }
        std::thread::sleep(Duration::from_millis(5));
    }
    done(served)
}

/// A `didOpen` that failed does not cost the file its server for the rest of the session. On a
/// review over a network that call is a round trip, and a link that blinks for a second would
/// otherwise leave that tab with no completions and no ⌘-click until it was closed and opened
/// again - so the text is offered once more, and the second time it lands.
#[test]
fn a_file_whose_first_open_failed_is_told_again_later_and_the_server_hears_it() {
    let source = FakeSource::new();
    source.fails_the_next_sends(1);
    let asking = Asking::new(
        "src/main.rs",
        Arc::clone(&source) as Arc<dyn LanguageSource>,
        || {},
    );
    let mut served = Served::default();
    let text = "fn one() {}";

    assert!(
        the_document_is_kept_up_until(&mut served, &asking, text, |served| {
            served.can_answer_about(text) == CanAnswer::Yes
        }),
        "the file never came back from a failed open; the source was asked {:?}",
        source.asked()
    );

    // The open was made twice, the first of them the one that failed, and the file has a
    // server again rather than being written off.
    let opens = source
        .asked()
        .into_iter()
        .filter(|asked| asked.starts_with("open"))
        .count();
    assert_eq!(opens, 2, "asked {:?}", source.asked());
    assert!(served.has_a_server());
    assert!(served.was_opened());
}

/// A file nothing serves is a different thing from a call that failed, and it stays as quiet
/// as it always was: asked about once, and then never spoken of again for as many frames as
/// the file is open. Most of a repo is that file.
#[test]
fn a_file_nothing_serves_settles_quietly_and_stops_asking_about_itself() {
    let source = FakeSource::answering(LspStatus::Unavailable, Vec::new());
    let asking = Asking::new(
        "notes.md",
        Arc::clone(&source) as Arc<dyn LanguageSource>,
        || {},
    );
    let mut served = Served::default();

    assert!(
        the_document_is_kept_up_until(&mut served, &asking, "# notes", Served::nothing_serves_it),
        "the answer that nothing serves the file never landed"
    );

    // Frames go by and it owes nothing at all - no text, no question, and not even a frame
    // asked for on its behalf.
    for _ in 0..200 {
        assert_eq!(served.owed("# notes", Instant::now()), DocumentOwed::default());
    }
    assert_eq!(source.asked(), ["status notes.md"]);
}

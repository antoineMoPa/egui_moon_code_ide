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
    Asked,
    asking::{Ask, Asking, Heard, StatusAbout},
    source::{LanguageSource, LspCompletion, LspLocation, LspPosition, LspStatus},
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
}

impl FakeSource {
    fn new() -> Arc<Self> {
        Arc::new(Self {
            asked: Mutex::new(Vec::new()),
            held: Mutex::new(None),
            threads: Mutex::new(Vec::new()),
        })
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
        LspStatus::Ready
    }

    fn did_open(&self, file_path: &str, text: &str) -> Result<()> {
        self.note(format!("open {file_path} {text}"));
        Ok(())
    }

    fn did_change(&self, file_path: &str, text: &str) -> Result<()> {
        self.note(format!("change {file_path} {text}"));
        Ok(())
    }

    fn did_close(&self, file_path: &str) -> Result<()> {
        self.note(format!("close {file_path}"));
        Ok(())
    }

    fn definition(&self, file_path: &str, at: LspPosition) -> Result<Vec<LspLocation>> {
        self.note(format!("definition {file_path} {}:{}", at.line, at.column));
        Ok(vec![LspLocation {
            file_path: "src/lib.rs".to_string(),
            line_number: 12,
        }])
    }

    fn completion(&self, file_path: &str, at: LspPosition) -> Result<Vec<LspCompletion>> {
        self.note(format!("completion {file_path} {}:{}", at.line, at.column));
        Ok(vec![LspCompletion {
            label: "greet".to_string(),
            detail: None,
            insert: "greet".to_string(),
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

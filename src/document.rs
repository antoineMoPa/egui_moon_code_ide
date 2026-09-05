//! Keeping a language server's copy of an open file up with what is on screen.
//!
//! A server answers questions about a document it has been told about, and the only thing
//! that knows what is in that document is whatever is showing it: the text on disk is not it -
//! the point of asking is to be answered about what has been typed. So the editor is what
//! tells the server, and what it has heard is kept here beside the text itself.
//!
//! Three things are sent, and nothing else: the file is opened when its text lands, changed
//! once the typing has stopped, and closed when the last view of it goes. They only ever go
//! for a file something actually serves, which is asked once and remembered - most of a repo
//! is markdown, configuration and images that no server has ever heard of, and a source
//! reached over a network pays for every question.
//!
//! Nothing here calls anything or blocks on anything. [`Served::owed`] says what the server
//! is owed this frame and the caller does it however it does such things - on this crate's
//! own worker thread, or on whatever the application already has.

use std::time::{Duration, Instant};

use crate::source::LspStatus;

/// How long the typing has to have stopped before the server hears the new text.
///
/// The whole text goes over on every change, and where the source is reached over a network
/// that is the whole file over the wire: a call per keystroke would flood the server and the
/// link both. Four hundred milliseconds is longer than the gap between two keys of ordinary
/// typing, so a sentence typed straight through sends once at the end of it rather than once
/// a letter, and short enough that a click made after even a brief pause is answered against
/// what is on screen rather than against what was there a word ago.
///
/// It is also the pause [`Completing`](crate::Completing) waits before it asks what could
/// finish the word being typed, and deliberately the same number rather than one of its own:
/// that question can only be asked about text the server has already heard, so a shorter
/// pause there would only end up waiting on this one, and a longer one would make the person
/// wait twice over a single lull in the typing.
pub const TYPING_SETTLES_IN: Duration = Duration::from_millis(400);

/// How often a server that is still reading the project is asked whether it has finished.
///
/// It is asked at all because the answer changes on its own: rust-analyzer takes tens of
/// seconds over a cold project, and until it is done it answers every question with nothing,
/// which reads exactly like a real answer of "there is nothing". A question a second while a
/// view waits on a server that is starting costs nothing next to that, and it stops the
/// moment the answer is yes - a server that has finished starting does not un-finish.
const STARTING_IS_ASKED_ABOUT_EVERY: Duration = Duration::from_secs(1);

/// Whether a language server is behind a file, and what it has heard.
#[derive(Default)]
pub enum Served {
    /// Nobody has asked yet. Nothing is asked until the text has arrived: a document is
    /// opened with what is in it, and there is nothing to open it with before then.
    #[default]
    Unknown,
    /// The question is out.
    Asking,
    /// No server serves this file, which is the normal state of most of a repo - markdown,
    /// configuration, images, and every language nobody installed a server for. Nothing more
    /// is ever sent about it and nothing is ever said about it: it is not a fault.
    No,
    /// A server serves it, and this is what it has heard.
    Yes(Document),
}

/// Whether the server behind a file can answer a question about a place in the text on
/// screen, and when it cannot, why not.
///
/// The two reasons are told apart because they end differently. Text the server has not heard
/// is on its way to it already and will be there in a moment; a server that has not finished
/// reading the project is a wait of tens of seconds. Neither is ever mistaken for an answer -
/// that is the whole of what this is for.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum CanAnswer {
    /// Not about this text. Either the server has never heard of the file, or what it was
    /// told is a word or two behind what is on screen - and a caret at line twelve of the
    /// text it heard a word ago is a place in a different file.
    NotThisText,
    /// Not yet at all: it has the text, but it is still reading the project. A server that is
    /// still reading answers every question with nothing, which reads exactly like a real
    /// answer of "there is nothing here" - the mistake [`LspStatus`] exists to prevent.
    StillReadingTheProject,
    /// Yes.
    Yes,
}

/// One open document, as the server last heard it.
pub struct Document {
    /// Whether the open has gone through. Until it has, the server has never heard of the
    /// file and a change would be a change to nothing.
    opened: bool,
    /// Whether a call about this document is in flight. One at a time, so the record of what
    /// the server has heard is only ever written by a call that has come back.
    sending: bool,
    /// The text the server last heard, and so what a change is measured against.
    sent: String,
    /// The text as the last frame saw it, and the moment it became that. Together they are
    /// how long the typing has been stopped for, which is what the debounce waits on.
    seen: String,
    seen_at: Instant,
    /// Whether the server has finished reading the project. Kept apart from whether there is
    /// a server at all, because the two mean completely different things to whoever is about
    /// to ask it something - see [`CanAnswer`].
    ready: bool,
    /// Whether a question about that is out, and when the last one was asked. A server that
    /// is starting finishes on its own, so the only way a view learns of it is by asking
    /// again now and then.
    asking_about_starting: bool,
    asked_about_starting_at: Instant,
}

/// What the server is owed about a document right now.
#[derive(PartialEq, Eq, Debug)]
enum Owed {
    Nothing,
    Open,
    Change,
}

impl Document {
    fn new(ready: bool) -> Self {
        let now = Instant::now();
        Self {
            opened: false,
            sending: false,
            sent: String::new(),
            seen: String::new(),
            seen_at: now,
            ready,
            asking_about_starting: false,
            asked_about_starting_at: now,
        }
    }

    /// Whether it is time to ask again whether the server has finished starting. Never once
    /// the answer is yes, and never twice at once.
    fn wants_the_status_again(&self, now: Instant) -> bool {
        !self.ready
            && !self.asking_about_starting
            && now.duration_since(self.asked_about_starting_at) >= STARTING_IS_ASKED_ABOUT_EVERY
    }

    /// Take note of the text this frame is showing, so the debounce is measured from the
    /// last time it changed rather than from the first time it differed from what was sent.
    fn saw(&mut self, text: &str, now: Instant) {
        if self.seen != text {
            self.seen.clear();
            self.seen.push_str(text);
            self.seen_at = now;
        }
    }

    /// What to send, if anything. Pure, so the debounce is tested without a clock or a
    /// server: the open goes the moment the text is there, and a change waits for the
    /// typing to stop.
    fn owed(&self, text: &str, now: Instant) -> Owed {
        if self.sending {
            return Owed::Nothing;
        }
        if !self.opened {
            return Owed::Open;
        }
        if self.sent == text {
            return Owed::Nothing;
        }
        match now.duration_since(self.seen_at) >= TYPING_SETTLES_IN {
            true => Owed::Change,
            false => Owed::Nothing,
        }
    }
}

/// The one call a view owes its server on the frame it is drawing.
#[derive(PartialEq, Eq, Debug)]
pub enum DocumentAsk {
    /// Nobody has asked yet whether anything serves this file. Answered with
    /// [`Served::served_answered`].
    WhetherServed,
    /// The server is still reading the project, and it is time to ask whether it has
    /// finished. Nothing about the text goes with it - this is only about the waiting.
    /// Answered with [`Served::starting_answered`].
    WhetherStillStarting,
    /// The whole of the text, as an open or as a change. Answered with [`Served::heard`].
    Send {
        /// What the server is to be told the file holds.
        text: String,
        /// Whether this is the first the server hears of the file.
        opening: bool,
    },
}

/// What a view owes its server this frame.
#[derive(PartialEq, Eq, Debug, Default)]
pub struct DocumentOwed {
    /// The call to make, if any. At most one: a question and a call both take frames to come
    /// back, and neither is asked twice.
    pub ask: Option<DocumentAsk>,
    /// How long until the frame this becomes worth acting on, when something is waiting on
    /// the typing to settle. A window nobody is typing in draws no more frames, so a caller
    /// with an egui context passes this straight to
    /// [`request_repaint_after`](egui::Context::request_repaint_after).
    pub draw_again_in: Option<Duration>,
}

impl Served {
    /// Whether the server behind this file can answer a question about a place in the text on
    /// screen right now. A file nothing serves answers about no text at all.
    pub fn can_answer_about(&self, text: &str) -> CanAnswer {
        let Served::Yes(document) = self else {
            return CanAnswer::NotThisText;
        };
        if !document.opened || document.sent != text {
            return CanAnswer::NotThisText;
        }
        match document.ready {
            true => CanAnswer::Yes,
            false => CanAnswer::StillReadingTheProject,
        }
    }

    /// Whether a server is behind this file at all, which is what decides whether it is ever
    /// worth offering to finish a word in it.
    pub fn has_a_server(&self) -> bool {
        matches!(self, Served::Yes(_))
    }

    /// Whether the answer has come back that nothing serves this file, which is the end state
    /// of the language-server side of most of a repo.
    pub fn nothing_serves_it(&self) -> bool {
        matches!(self, Served::No)
    }

    /// Whether there is an open document to close. A file whose open never went through has
    /// nothing to tell the server about, and a `didClose` for it would be about a document
    /// the server has never heard of.
    pub fn was_opened(&self) -> bool {
        matches!(self, Served::Yes(document) if document.opened)
    }

    /// Where the file stands, as the three things a person can be told: no server, a server
    /// still reading the project, or one that will answer.
    ///
    /// Before anything has been asked this reads as [`LspStatus::Unavailable`] - nothing is
    /// known to be behind the file yet, and the honest answer to "is a server going to
    /// answer this click" is no.
    pub fn status(&self) -> LspStatus {
        match self {
            Served::Unknown | Served::Asking | Served::No => LspStatus::Unavailable,
            Served::Yes(document) if document.ready => LspStatus::Ready,
            Served::Yes(_) => LspStatus::Starting,
        }
    }

    /// What the view owes its server on the frame it is drawing, marking down what it is
    /// about to do.
    ///
    /// `text` is what is on screen right now, which after typing is not what is on disk and
    /// not what the server has heard.
    pub fn owed(&mut self, text: &str, now: Instant) -> DocumentOwed {
        match self {
            Served::Asking | Served::No => DocumentOwed::default(),
            Served::Unknown => {
                *self = Served::Asking;
                DocumentOwed {
                    ask: Some(DocumentAsk::WhetherServed),
                    draw_again_in: None,
                }
            }
            Served::Yes(document) => {
                document.saw(text, now);
                match document.owed(text, now) {
                    Owed::Nothing => {
                        // A change waiting on the typing to stop needs a frame to be sent on.
                        let waiting = !document.sending && document.sent != text;
                        // Asked only when there is nothing to send, so a change never waits a
                        // frame behind a question about the waiting.
                        let ask = document.wants_the_status_again(now).then(|| {
                            document.asking_about_starting = true;
                            document.asked_about_starting_at = now;
                            DocumentAsk::WhetherStillStarting
                        });
                        DocumentOwed {
                            ask,
                            draw_again_in: waiting.then_some(TYPING_SETTLES_IN),
                        }
                    }
                    owed => {
                        document.sending = true;
                        DocumentOwed {
                            ask: Some(DocumentAsk::Send {
                                text: text.to_string(),
                                opening: owed == Owed::Open,
                            }),
                            draw_again_in: None,
                        }
                    }
                }
            }
        }
    }

    /// The answer to [`DocumentAsk::WhetherServed`].
    ///
    /// Starting counts as served: what starts the server is this document being opened.
    /// Whether it has finished starting is kept rather than folded in, because a question
    /// asked of a server that is still reading the project comes back empty and reads as an
    /// answer - see [`CanAnswer`].
    pub fn served_answered(&mut self, status: LspStatus) {
        *self = match status {
            LspStatus::Starting => Served::Yes(Document::new(false)),
            LspStatus::Ready => Served::Yes(Document::new(true)),
            LspStatus::Unavailable => Served::No,
        };
    }

    /// The answer to [`DocumentAsk::WhetherStillStarting`].
    ///
    /// Nothing is said about it either way. A click made while a server is starting says so,
    /// because a click is a direct request and deserves an answer; typing is not, and a
    /// message for every word typed in the first ten seconds of a file would be worse than
    /// the wait it was explaining.
    pub fn starting_answered(&mut self, status: LspStatus) {
        let Served::Yes(document) = self else {
            return;
        };
        document.asking_about_starting = false;
        document.ready = status == LspStatus::Ready;
    }

    /// The same question, come back with nothing to say. It is asked again in a moment, and
    /// until then nothing is asked of the server.
    pub fn starting_could_not_be_asked(&mut self) {
        if let Served::Yes(document) = self {
            document.asking_about_starting = false;
        }
    }

    /// The server heard the text that was sent to it.
    pub fn heard(&mut self, sent: String) {
        let Served::Yes(document) = self else {
            return;
        };
        document.sending = false;
        document.opened = true;
        document.sent = sent;
    }

    /// The server could not be told, so this view stops talking to it.
    ///
    /// Trying again every frame would be a call a frame at the exact moment something is
    /// already wrong, and go-to-definition has whatever the caller falls back on either way.
    pub fn could_not_be_told(&mut self) {
        *self = Served::No;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The document is opened the moment there is text to open it with, and nothing more is
    /// sent until it has gone through.
    #[test]
    fn the_document_is_opened_once_and_nothing_is_sent_while_a_call_is_out() {
        let now = Instant::now();
        let mut document = Document::new(true);
        assert_eq!(document.owed("fn one() {}", now), Owed::Open);

        document.sending = true;
        assert_eq!(document.owed("fn one() {}", now), Owed::Nothing);

        // As the open comes back: the server has heard this text, and has heard nothing
        // else until the typing stops again.
        document.sending = false;
        document.opened = true;
        document.sent = "fn one() {}".to_string();
        assert_eq!(document.owed("fn one() {}", now), Owed::Nothing);
    }

    /// The point of the debounce: a burst of typing sends the text once at the end of it
    /// rather than once a keystroke, which over a network is the whole file each time.
    #[test]
    fn typing_sends_the_text_once_it_has_stopped_rather_than_once_a_keystroke() {
        let start = Instant::now();
        let mut document = Document::new(true);
        document.opened = true;
        document.sent = "fn one() {}".to_string();
        document.seen = document.sent.clone();
        document.seen_at = start;

        // A keystroke every fiftieth of a second, none of them worth a call.
        let mut typed = String::new();
        for (key, letter) in "// a comment".chars().enumerate() {
            typed.push(letter);
            let at = start + Duration::from_millis(20 * key as u64);
            document.saw(&typed, at);
            assert_eq!(document.owed(&typed, at), Owed::Nothing, "at key {key}");
        }

        // Still nothing a moment after the last of them, and the whole text once the pause
        // has run its length.
        let last = start + Duration::from_millis(20 * 11);
        assert_eq!(
            document.owed(&typed, last + TYPING_SETTLES_IN / 2),
            Owed::Nothing
        );
        assert_eq!(document.owed(&typed, last + TYPING_SETTLES_IN), Owed::Change);
    }

    /// The whole sequence a file goes through, as the caller sees it: ask what serves it,
    /// open it, and then have nothing to say until the typing stops.
    #[test]
    fn a_served_file_is_asked_about_then_opened_and_then_left_alone() {
        let now = Instant::now();
        let mut served = Served::default();
        assert_eq!(
            served.owed("fn one() {}", now).ask,
            Some(DocumentAsk::WhetherServed)
        );
        // The question is out, so it is not asked again on the next frame.
        assert_eq!(served.owed("fn one() {}", now).ask, None);

        served.served_answered(LspStatus::Ready);
        let owed = served.owed("fn one() {}", now);
        assert_eq!(
            owed.ask,
            Some(DocumentAsk::Send {
                text: "fn one() {}".to_string(),
                opening: true
            })
        );
        served.heard("fn one() {}".to_string());
        assert_eq!(served.can_answer_about("fn one() {}"), CanAnswer::Yes);
        assert_eq!(served.owed("fn one() {}", now).ask, None);
    }

    /// A file nothing serves is asked about once and then never spoken of again, which is
    /// what most of a repo costs.
    #[test]
    fn a_file_nothing_serves_is_asked_about_once_and_then_never_again() {
        let now = Instant::now();
        let mut served = Served::default();
        assert!(served.owed("# notes", now).ask.is_some());
        served.served_answered(LspStatus::Unavailable);

        assert!(served.nothing_serves_it());
        assert!(!served.has_a_server());
        assert_eq!(served.owed("# notes", now), DocumentOwed::default());
        assert_eq!(served.can_answer_about("# notes"), CanAnswer::NotThisText);
    }

    /// A server that is still reading the project is not the same as one with no answer, and
    /// a question is never put to it while it reads.
    #[test]
    fn a_server_that_is_still_reading_the_project_is_not_asked_anything_about_the_text() {
        let now = Instant::now();
        let mut served = Served::default();
        served.owed("fn one() {}", now);
        served.served_answered(LspStatus::Starting);
        served.owed("fn one() {}", now);
        served.heard("fn one() {}".to_string());

        assert_eq!(
            served.can_answer_about("fn one() {}"),
            CanAnswer::StillReadingTheProject
        );
        assert_eq!(served.status(), LspStatus::Starting);

        // Asked again a second later, and once it says yes the file is answerable.
        let later = now + STARTING_IS_ASKED_ABOUT_EVERY * 2;
        assert_eq!(
            served.owed("fn one() {}", later).ask,
            Some(DocumentAsk::WhetherStillStarting)
        );
        served.starting_answered(LspStatus::Ready);
        assert_eq!(served.can_answer_about("fn one() {}"), CanAnswer::Yes);
        assert_eq!(served.status(), LspStatus::Ready);
    }

    /// A window nobody is typing in draws no more frames, so the change waiting on the pause
    /// has to ask for the frame it will be sent on.
    #[test]
    fn a_change_waiting_on_the_pause_asks_for_the_frame_it_will_be_sent_on() {
        let now = Instant::now();
        let mut served = Served::default();
        served.owed("fn one() {}", now);
        served.served_answered(LspStatus::Ready);
        served.owed("fn one() {}", now);
        served.heard("fn one() {}".to_string());

        let owed = served.owed("fn one() {} // typed", now);
        assert_eq!(owed.ask, None);
        assert_eq!(owed.draw_again_in, Some(TYPING_SETTLES_IN));
    }
}

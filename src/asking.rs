//! The thread the questions are actually put on.
//!
//! Every call on a [`LanguageSource`] blocks - milliseconds against a warm server, tens of
//! seconds against a cold one, and a network round trip on top where the repo is somewhere
//! else. An egui application draws its window on one thread and must never wait on any of
//! that, so the questions go somewhere else and the answers are read back on a later frame.
//!
//! One thread per editor, and one question in flight on it at a time. Per editor because a
//! definition that takes four seconds in one view must not hold up the typing in another;
//! one at a time because that is what the state machines above are written against - they
//! each hold exactly one outstanding question and check the answer against it. An editor
//! that follows a jump into another file takes its thread with it - see
//! [`Asking::asks_about_instead`] - because a thread per click is a leak, and the file it
//! left has to be closed on the server rather than merely forgotten here.
//!
//! What is waiting to be asked is a slot per kind of question rather than a queue, so a
//! question replaces the one it supersedes instead of piling up behind it. That is the whole
//! of the cancellation this needs: a completion asked for `gre` that has not been sent yet is
//! simply overwritten by the one for `greet`, and one that *has* been sent comes back and is
//! thrown away by [`Completing::answered`](crate::Completing::answered) because it no longer
//! matches what is being typed.

use std::{
    sync::{Arc, Condvar, Mutex, mpsc},
    thread,
};

use egui_moon_editor::Word;

use crate::{
    completing::Asked,
    source::{LanguageSource, LspCompletion, LspLocation, LspPosition, LspStatus},
};

/// Which of the two things a status question is about, carried out and back so the answer
/// reaches the right half of [`Served`](crate::Served).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum StatusAbout {
    /// Whether anything serves this file at all. Asked once.
    WhetherServed,
    /// Whether the server has finished reading the project. Asked again every second until
    /// the answer is yes.
    WhetherStillStarting,
}

/// A question to put to the source.
pub enum Ask {
    /// Where the file stands.
    Status(StatusAbout),
    /// The whole of the text, as an open or as a change.
    Send {
        /// What to tell the server the file holds.
        text: String,
        /// Whether this is the first the server hears of it.
        opening: bool,
    },
    /// Where the clicked name is defined. The word is carried so the answer can be checked
    /// against the click that is still current when it lands.
    Definition {
        /// The name that was clicked.
        word: Word,
        /// Where in the file it sits.
        at: LspPosition,
    },
    /// What could finish the word being typed.
    Completion(Asked),
    /// What the server behind this file said opens a completion list on its own. Asked once,
    /// as soon as there is a server up to have said it.
    Triggers,
}

/// An answer, read off the worker on a later frame.
pub enum Heard {
    /// Where the file stands, and which question it answers.
    Status {
        /// The question this answers.
        about: StatusAbout,
        /// What the source said.
        status: LspStatus,
    },
    /// What became of a [`Ask::Send`].
    Told {
        /// The text that was sent, so the record of what the server has heard is written
        /// from what actually went rather than from what is on screen now.
        text: String,
        /// Whether the server heard it.
        heard: bool,
    },
    /// Where a name is defined. `None` for a question that could not be put at all, which is
    /// not the same as a server that answered with nowhere.
    Definition {
        /// The name that was clicked.
        word: Word,
        /// Everywhere the server says it is defined.
        places: Option<Vec<LspLocation>>,
    },
    /// What opens a completion list on its own, as the server behind this file named them.
    Triggers(Vec<char>),
    /// What could finish a word. `None` for a server that could not answer.
    Completion {
        /// The question this answers, to be handed back to
        /// [`Completing::answered`](crate::Completing::answered).
        asked: Asked,
        /// What the server offered.
        rows: Option<Vec<LspCompletion>>,
    },
}

/// The kinds of question, which is also how many can be waiting at once.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Kind {
    Document,
    Status,
    Definition,
    Triggers,
    Completion,
}

/// Which waiting question is taken next.
///
/// A table rather than a chain of conditions, because the order is the whole contract with
/// the state machines above. The document goes first: every other question is about a place
/// in text the server has to have heard already, so sending it first is what makes the rest
/// answerable at all. The status is next because it is what says whether asking is worth
/// anything. Then the click, which somebody is waiting on. Then what opens a list on its own,
/// which is asked once per server and is what says whether the last one is worth putting at
/// all. And last the completion, which is an offer nobody asked for out loud.
const IN_ORDER: [Kind; 5] = [
    Kind::Document,
    Kind::Status,
    Kind::Definition,
    Kind::Triggers,
    Kind::Completion,
];

impl Kind {
    fn slot(self) -> usize {
        IN_ORDER
            .iter()
            .position(|kind| *kind == self)
            .expect("every kind is in the order")
    }
}

impl Ask {
    fn kind(&self) -> Kind {
        match self {
            Ask::Status(_) => Kind::Status,
            Ask::Send { .. } => Kind::Document,
            Ask::Definition { .. } => Kind::Definition,
            Ask::Completion(_) => Kind::Completion,
            Ask::Triggers => Kind::Triggers,
        }
    }
}

/// What is waiting to be asked, and whether the file has been closed.
#[derive(Default)]
struct Waiting {
    /// One slot per [`Kind`], so a question replaces the one it supersedes.
    slots: [Option<Ask>; IN_ORDER.len()],
    /// Set as the handle is dropped. The thread tells the server the file is gone and stops.
    closing: bool,
    /// A file the editor has moved to, waiting for the thread to close the old one and start
    /// asking about this one instead. See [`Asking::asks_about_instead`].
    moving_to: Option<String>,
    /// Which file the questions being taken now are about, counted up once per move. It goes
    /// out with every answer so that one worked out about the file before the move can be
    /// told apart from one about the file on screen - see [`Asking::heard`].
    about: u64,
}

impl Waiting {
    /// The next question to put, in the order [`IN_ORDER`] lays down.
    fn take(&mut self) -> Option<Ask> {
        IN_ORDER
            .iter()
            .find_map(|kind| self.slots[kind.slot()].take())
    }
}

/// What the thread does next, decided under the lock and done outside it - the two calls a
/// move makes can each take a server as long as a question does, and holding the queue shut
/// for them would block the frame that put the next one.
enum Next {
    /// The handle is gone: close the file if it was opened, and stop.
    Stop,
    /// The editor is on another file now: close the old one and ask about this one from here.
    MoveTo(String),
    /// A question, and which file it is about.
    Put(Ask, u64),
}

/// A worker thread that puts questions to a [`LanguageSource`], and the answers it has come
/// back with.
///
/// Dropping it tells the server the file is gone - if it was ever opened - and stops the
/// thread. Nothing about that is worth waiting on, so the drop does not block.
///
/// It follows the editor from file to file rather than being one per file for the life of a
/// window: a jump into a definition would otherwise leave a thread behind per click, and the
/// server would be left believing every file ever visited is still open. See
/// [`asks_about_instead`](Self::asks_about_instead).
pub struct Asking {
    queue: Arc<(Mutex<Waiting>, Condvar)>,
    answers: mpsc::Receiver<(u64, Heard)>,
    /// Which file the answers worth reading are about. Bumped as the editor moves, so an
    /// answer the thread was already inside when it moved is dropped rather than shown
    /// against text it was never computed for.
    about: u64,
}

impl Asking {
    /// Start asking about `file_path`, out of `source`.
    ///
    /// `wake` is called every time an answer lands, and is where an egui application calls
    /// [`request_repaint`](egui::Context::request_repaint): an answer that arrives while
    /// nobody is typing would otherwise sit in the channel until something else caused a
    /// frame, which for a completion list is forever.
    pub fn new(
        file_path: impl Into<String>,
        source: Arc<dyn LanguageSource>,
        wake: impl Fn() + Send + 'static,
    ) -> Self {
        let queue = Arc::new((Mutex::new(Waiting::default()), Condvar::new()));
        let (answers, heard) = mpsc::channel();
        let file_path = file_path.into();
        let working = Arc::clone(&queue);
        thread::spawn(move || work(file_path, source.as_ref(), &working, &answers, &wake));
        Self {
            queue,
            answers: heard,
            about: 0,
        }
    }

    /// Put a question, replacing whatever of the same kind was still waiting to go.
    pub fn ask(&self, ask: Ask) {
        let (waiting, ready) = &*self.queue;
        let slot = ask.kind().slot();
        waiting.lock().unwrap().slots[slot] = Some(ask);
        ready.notify_one();
    }

    /// Ask about `file_path` from here on, on this same thread.
    ///
    /// The thread tells the source the old file is closed and then goes on with the new one,
    /// so the server's idea of what is open stays true and a window that jumps around a repo
    /// all afternoon still has the one thread it started with.
    ///
    /// Everything waiting to be asked about the old file is dropped where it stands - it is
    /// about text nobody is looking at - and anything already out comes back to be thrown
    /// away rather than mistaken for an answer about the new file.
    pub fn asks_about_instead(&mut self, file_path: impl Into<String>) {
        let (waiting, ready) = &*self.queue;
        let mut queued = waiting.lock().unwrap();
        queued.slots = Default::default();
        queued.moving_to = Some(file_path.into());
        queued.about += 1;
        self.about = queued.about;
        drop(queued);
        ready.notify_one();
    }

    /// Every answer about the file now open that has come back since the last frame. Never
    /// blocks.
    pub fn heard(&self) -> impl Iterator<Item = Heard> {
        let about = self.about;
        self.answers
            .try_iter()
            .filter_map(move |(answered, heard)| (answered == about).then_some(heard))
    }
}

impl Drop for Asking {
    fn drop(&mut self) {
        let (waiting, ready) = &*self.queue;
        waiting.lock().unwrap().closing = true;
        ready.notify_one();
    }
}

/// The worker: wait for a question, put it, send the answer back, wake the window.
///
/// It ends when the handle is dropped, which is one of the two things it checks before every
/// question - a file being closed matters more than anything still waiting to be asked about
/// it, and so does the editor having moved to another file, whose text is the only text
/// anyone is looking at.
fn work(
    mut file_path: String,
    source: &dyn LanguageSource,
    queue: &(Mutex<Waiting>, Condvar),
    answers: &mpsc::Sender<(u64, Heard)>,
    wake: &dyn Fn(),
) {
    let (waiting, ready) = queue;
    // Whether the server has ever heard of this file, which is what says there is anything
    // to close - at the end, and on the way to the next file.
    let mut opened = false;
    loop {
        let next = {
            let mut queued = waiting.lock().unwrap();
            loop {
                if queued.closing {
                    break Next::Stop;
                }
                if let Some(moving_to) = queued.moving_to.take() {
                    break Next::MoveTo(moving_to);
                }
                if let Some(ask) = queued.take() {
                    break Next::Put(ask, queued.about);
                }
                queued = ready.wait(queued).unwrap();
            }
        };

        let (ask, about) = match next {
            Next::Stop => {
                if opened {
                    let _ = source.did_close(&file_path);
                }
                return;
            }
            Next::MoveTo(moving_to) => {
                if opened {
                    let _ = source.did_close(&file_path);
                }
                opened = false;
                file_path = moving_to;
                continue;
            }
            Next::Put(ask, about) => (ask, about),
        };

        let file_path = file_path.as_str();
        let heard = match ask {
            Ask::Status(about) => Heard::Status {
                about,
                status: source.status(file_path),
            },
            Ask::Send { text, opening } => {
                let told = match opening {
                    true => source.did_open(file_path, &text),
                    false => source.did_change(file_path, &text),
                };
                opened |= told.is_ok();
                Heard::Told {
                    text,
                    heard: told.is_ok(),
                }
            }
            Ask::Definition { word, at } => Heard::Definition {
                word,
                places: source.definition(file_path, at).ok(),
            },
            Ask::Completion(asked) => Heard::Completion {
                rows: source.completion(file_path, asked.at()).ok(),
                asked,
            },
            Ask::Triggers => Heard::Triggers(source.trigger_characters(file_path)),
        };
        // A closed handle has dropped the receiver, and the answer belongs to nobody.
        if answers.send((about, heard)).is_err() {
            return;
        }
        wake();
    }
}

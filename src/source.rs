//! Where the answers about a file come from, as six questions and nothing else.
//!
//! This is the whole of what the rest of the crate needs of a language server, written down
//! so that it can be answered by something other than a server running here.

use anyhow::Result;

pub use moon_lsp::{LspCompletion, LspLocation, LspPosition, LspStatus};

/// Somewhere that answers language questions about the files being edited.
///
/// It is a trait because the answers do not have to come from this process. A window
/// reviewing a repo on another machine has no files to start a server on: the repo, and the
/// servers reading it, are over there, and the window reaches them over HTTP - it sends the
/// same six questions and reads the same answers back. An editor built on this crate cannot
/// tell the difference, and should not be able to: whether `definition` walked a hash map or
/// crossed a continent, it is still "where is this name defined".
///
/// It is deliberately not `'static`: an application whose backend only exists for the length
/// of one call - a `&dyn Backend` handed to a worker closure - implements this on a borrow,
/// and only [`Asking`](crate::Asking), which hands a source to a thread, asks for a lifetime
/// at all.
///
/// The same seam is what makes the crate testable. A fake that answers out of a table needs
/// no server, no repo and no window, which is how the debounce and the abandoning of a stale
/// request are tested here.
///
/// **Every one of these blocks.** A server takes milliseconds on a good day and tens of
/// seconds on a cold project, and over a network there is a round trip on top. Nothing here
/// may be called from a frame - see [`Asking`](crate::Asking), which is the thread that calls
/// them, and [`CodeEditor`](crate::CodeEditor), which owns one.
pub trait LanguageSource: Send + Sync {
    /// Whether a server is behind this file, and whether it has finished starting.
    ///
    /// Not a `Result`: every way of having no answer reads the same to whoever is waiting -
    /// no server in the table, none installed, one that would not start, a machine that could
    /// not be reached - and all of them are [`LspStatus::Unavailable`]. An implementation
    /// that has an error in hand throws it away here on purpose, because the caller's next
    /// move is the same either way.
    fn status(&self, file_path: &str) -> LspStatus;

    /// Tell the server a file is open and what is in it.
    fn did_open(&self, file_path: &str, text: &str) -> Result<()>;

    /// The whole text again, as it stands.
    ///
    /// **Debounced by the caller**, which in this crate is [`Document`](crate::Document): a
    /// round trip per keystroke floods a server, and over a network floods the link too.
    fn did_change(&self, file_path: &str, text: &str) -> Result<()>;

    /// Tell the server this side is done with a file.
    fn did_close(&self, file_path: &str) -> Result<()>;

    /// Where the name at this place is defined. Empty when the server has no answer.
    fn definition(&self, file_path: &str, at: LspPosition) -> Result<Vec<LspLocation>>;

    /// What could be typed at this place. Empty when the server offers nothing.
    fn completion(&self, file_path: &str, at: LspPosition) -> Result<Vec<LspCompletion>>;
}

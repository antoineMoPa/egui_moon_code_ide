//! Where the answers about a file come from, as a handful of questions and nothing else.
//!
//! This is the whole of what the rest of the crate needs of a language server, written down
//! so that it can be answered by something other than a server running here.

use anyhow::Result;

pub use moon_lsp::{
    LspCodeAction, LspDiagnostic, LspFormatting, LspPlaces, LspSeverity, LspSignature,
};

pub use moon_lsp::{
    LspCompletion, LspCompletionKind, LspFileEdit, LspLocation, LspPosition, LspStatus, LspTextEdit,
};

/// Somewhere that answers language questions about the files being edited.
///
/// It is a trait because the answers do not have to come from this process. A window
/// reviewing a repo on another machine has no files to start a server on: the repo, and the
/// servers reading it, are over there, and the window reaches them over HTTP - it sends the
/// same questions and reads the same answers back. An editor built on this crate cannot
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

    /// The places of one kind the server names for the name at this place - where it is
    /// defined, where its type is, where it is implemented, or everywhere it is used. Empty
    /// when the server has no answer.
    fn places(
        &self,
        file_path: &str,
        at: LspPosition,
        which: LspPlaces,
    ) -> Result<Vec<LspLocation>>;

    /// Where the name at this place is defined. Empty when the server has no answer.
    fn definition(&self, file_path: &str, at: LspPosition) -> Result<Vec<LspLocation>> {
        self.places(file_path, at, LspPlaces::Definition)
    }

    /// What could be typed at this place. Empty when the server offers nothing.
    fn completion(&self, file_path: &str, at: LspPosition) -> Result<Vec<LspCompletion>>;

    /// What the name at this place is called, as the server would rename it: the text to
    /// offer to be typed over. `None` when nothing at this place can be renamed - a keyword, a
    /// string, a file nothing serves.
    fn prepare_rename(&self, file_path: &str, at: LspPosition) -> Result<Option<String>>;

    /// Everything calling the name at this place `new_name` would change, one entry per file,
    /// every place in the editor's own units.
    ///
    /// Nothing is written. Which of those files are open in a buffer and which are only on
    /// disk is the caller's to know, and so is what to do about each: an open buffer takes
    /// the edit as typing would, where a file nobody has open can only be written.
    fn rename(&self, file_path: &str, at: LspPosition, new_name: &str) -> Result<Vec<LspFileEdit>>;

    /// The edits that format the whole of this file, indented the way `options` says. Empty
    /// for a file already formatted; an error for one whose server does not format.
    fn format(&self, file_path: &str, options: LspFormatting) -> Result<Vec<LspTextEdit>>;

    /// What the server says about the name at this place - its type, its signature, its docs -
    /// as markdown. `None` for nothing to say.
    fn hover(&self, file_path: &str, at: LspPosition) -> Result<Option<String>>;

    /// What the server last said is wrong with this file, every place counted against the text
    /// it was last sent. A read of what the server already published, not a question.
    fn diagnostics(&self, file_path: &str) -> Result<Vec<LspDiagnostic>>;

    /// Tell the server this file was written to disk, which is what some servers check a
    /// project on.
    fn did_save(&self, file_path: &str) -> Result<()>;

    /// What the server offers to do to the code at this place - fixes for what it found wrong
    /// there, rewrites - each with everything it changes. Empty for nothing on offer.
    fn code_actions(&self, file_path: &str, at: LspPosition) -> Result<Vec<LspCodeAction>>;

    /// The signature of the call around this place, and which parameter it is at. `None` for
    /// no call around it.
    fn signature_help(&self, file_path: &str, at: LspPosition) -> Result<Option<LspSignature>>;

    /// The characters the server behind this file said open a completion list on their own:
    /// the `.` of `thing.`, the `:` of a path, the `(` of a call.
    ///
    /// A question rather than a table because the answer is the server's and differs by
    /// language - rust-analyzer names `.`, `:`, `'` and `(`, and typescript-language-server
    /// names `.`, `"`, `'`, `/`, `@` and `<`. It is asked once a file's server is up, and
    /// the answer holds for as long as it runs.
    ///
    /// Empty for a file nothing serves, for a server that has not started yet, and for one
    /// that named none. All three come to the same thing above: nothing here opens a list on
    /// its own, so only a word being typed is asked about.
    ///
    /// It has an answer of its own rather than being required of every implementation,
    /// because a source that reaches its servers across a network has to carry the question
    /// there before it can answer it, and one that has not been taught to is not broken - it
    /// is a source that completes words and not trigger characters, which is what every
    /// source did before this question existed.
    fn trigger_characters(&self, _file_path: &str) -> Vec<char> {
        Vec::new()
    }
}

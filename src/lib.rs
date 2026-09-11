//! An [egui](https://github.com/emilk/egui) code editor with go-to-definition and
//! autocomplete.
//!
//! There is a widget - [`egui_moon_editor`] - which owns a buffer, draws it, draws a
//! completion list it is *handed*, and reports the word under a modifier-click. And there is a
//! client - [`moon_lsp`] - which starts language servers, carries the JSON-RPC and does the
//! position arithmetic. Neither knows the other exists, and that is right: a widget with a
//! subprocess in it is not a widget.
//!
//! What was missing is everything between them, which turns out to be most of the work.
//! Nobody wants to write it twice, so it is here: when to tell a server the text has changed
//! and when not to, how to ask what could finish a half-typed word without asking once a
//! keystroke, how to throw away an answer computed for text that no longer exists, and how to
//! tell "the server is still indexing" apart from "there is no definition" - which look
//! identical on the wire and could not be more different to the person waiting.
//!
//! ```no_run
//! use std::sync::Arc;
//! use egui_moon_code_ide::{CodeEditor, Definition, RegistrySource};
//!
//! # fn build(ctx: &egui::Context) -> CodeEditor {
//! let servers = Arc::new(moon_lsp::LspRegistry::new(std::env::var("PATH").unwrap_or_default()));
//! let source = RegistrySource::for_repo(servers, "/home/dev/repo");
//! let text = std::fs::read_to_string("/home/dev/repo/src/main.rs").unwrap_or_default();
//! let editor = CodeEditor::new(ctx, "src/main.rs", text, Arc::new(source));
//! # editor
//! # }
//!
//! # fn frame(ui: &mut egui::Ui, editor: &mut CodeEditor) {
//! let style = egui_moon_editor::EditorStyle::from_visuals(ui.visuals());
//! let output = editor.ui(ui, &style, &egui_moon_editor::EditorRequest::default());
//! match output.definition {
//!     Some(Definition::Places { places, .. }) => println!("{} places", places.len()),
//!     Some(Definition::StillStarting(waiting)) => println!("{waiting}"),
//!     Some(Definition::NoServer(word)) => println!("nothing serves {}", word.text),
//!     None => {}
//! }
//! # }
//! ```
//!
//! A whole file open in a window, against a real language server, with ⌘-click opening the
//! file a name is defined in:
//!
//! ```sh
//! cargo run --example edit -- src/lib.rs
//! ```
//!
//! # The seam
//!
//! The answers do not have to come from this process. [`LanguageSource`] is the questions
//! and nothing else, because a window reviewing a repo on another machine reaches its servers
//! over HTTP - the repo is over there, and so is anything that could read it. An editor built
//! on this crate cannot tell the difference, and should not be able to. [`RegistrySource`] is
//! the local answer to those, so that the simple case is two lines rather than homework.
//!
//! Everything above the trait is the caller's. Where a jump lands, what to do when there is
//! no server - a repo search, a tags file, nothing - what a status bar says, and whether two
//! files share a set of servers: none of that is this crate's business, and all of it is a
//! product's.
//!
//! # Threading
//!
//! Every one of them blocks, sometimes for tens of seconds, and an egui application must
//! never wait on a frame. So [`CodeEditor`] owns a worker thread - see [`Asking`] - puts its
//! questions on it and reads the answers back on whatever later frame they land. Three things
//! hold whatever else changes: `did_change` is debounced by [`TYPING_SETTLES_IN`] rather than
//! sent per keystroke, a question the buffer has moved out from under is thrown away instead
//! of shown, and [`LspStatus::Starting`] is never folded into an empty answer.
//!
//! An editor that follows a jump keeps that thread: [`CodeEditor::open`] closes the old
//! document on the server, opens the new one and drops what was in flight about the old, so
//! an afternoon of jumping around a repo costs the one thread it started with.
//!
//! An application that already has worker threads of its own can skip [`CodeEditor`] and
//! [`Asking`] entirely and drive [`Served`], [`Completing`] and [`asks_about`] from them.
//! Those hold no channels, take the clock as an argument, and are where all of the deciding
//! lives.
//!
//! # The parts
//!
//! [`source`] is the trait, [`registry`] the local answer to it, [`document`] what a server
//! is owed about an open file, [`completing`] when a half-typed word is worth a question,
//! [`calling`] why taking a function from the list writes the parentheses of a call,
//! [`definition`] what a click can expect of a server, [`asking`] the thread the questions go
//! on, and [`editor`] the whole of it wired together.

#![forbid(unsafe_code)]
#![warn(missing_docs, clippy::doc_markdown)]

pub mod asking;
pub mod calling;
pub mod completing;
pub mod definition;
pub mod document;
pub mod editor;
pub mod hovering;
pub mod registry;
pub mod signing;
pub mod source;

pub use asking::{Ask, Asking, Heard, StatusAbout};
pub use calling::{follows_the_caret, row_for, takes_parentheses};
pub use completing::{Asked, AtTheCaret, Completing, CompletingNext, before_the_caret};
pub use definition::{AsksAbout, asks_about, still_starting};
pub use document::{CanAnswer, Document, DocumentAsk, DocumentOwed, Served, TYPING_SETTLES_IN};
pub use editor::{CodeEditor, CodeEditorOutput, Definition};
pub use hovering::{Hovering, HoveringNext, POINTER_SETTLES_IN};
pub use registry::RegistrySource;
pub use signing::{SIGNATURE_SETTLES_IN, SignedAt, Signing, SigningNext, inside_a_call};
pub use source::{
    LanguageSource, LspCodeAction, LspCompletion, LspCompletionKind, LspDiagnostic, LspFileEdit,
    LspFormatting, LspLocation, LspPlaces, LspPosition, LspSeverity, LspSignature, LspStatus,
    LspTextEdit,
};

#[cfg(test)]
mod tests;

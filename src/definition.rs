//! Modifier-click a name and land on where it is defined.
//!
//! What is here is the small part of that which is nobody's policy but the protocol's:
//! whether the server behind the file is in a position to be asked at all, and what to say
//! when it is not.
//!
//! The rest is deliberately absent. Where a jump lands, whether several places are offered or
//! one is taken, and what to do when there is no server - a repo search, a tags file, nothing
//! at all - are the caller's, because they are the caller's product. This crate has one
//! answer to give, and it says plainly when it has none.

use crate::source::LspStatus;

/// What a modifier-click does about the server behind the file it was made in.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum AsksAbout {
    /// Ask it where the name is defined. It has read the project and knows which of the forty
    /// `new`s in the repo this one is, which no amount of guessing at lines does.
    Server,
    /// Say it is still coming up, and ask nothing. A server that is indexing answers with
    /// nothing at all, which reads exactly like the name being defined nowhere - see
    /// [`still_starting`] for the sentence to say.
    Wait,
    /// Nothing here can answer, so it is the caller's own fallback or nothing. Not a
    /// consolation prize: it is how markdown, shell, SQL and every language nobody installed
    /// a server for get navigated, which is most of a repo and most machines.
    Elsewhere,
}

/// Which of the three a file's status comes to.
pub fn asks_about(status: LspStatus) -> AsksAbout {
    match status {
        LspStatus::Unavailable => AsksAbout::Elsewhere,
        LspStatus::Starting => AsksAbout::Wait,
        LspStatus::Ready => AsksAbout::Server,
    }
}

/// What to say about a click made while the server behind the file is still coming up.
///
/// rust-analyzer takes tens of seconds over a cold project, and every one of those seconds
/// would otherwise look like a feature that does nothing.
///
/// The name comes from the one table that says which server serves which extension - see
/// [`moon_lsp::languages`] - so that the sentence and the process it is about can never
/// disagree: a language added there is named here without anything being added, and a second
/// copy of that mapping kept in a window would drift the moment one of them was edited.
pub fn still_starting(file_path: &str) -> String {
    match moon_lsp::languages::for_file(file_path) {
        Some(language) => format!(
            "{} is still indexing this project - try again in a moment",
            language.server.name
        ),
        // Nothing in that table serves it, so it can only be named as what it is. A file
        // like this never reads as starting in the first place.
        None => "the language server is still starting - try again in a moment".to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The server is asked where there is one; a file nothing serves falls back to whatever
    /// the caller has; and a server still coming up is neither.
    #[test]
    fn a_server_is_asked_where_there_is_one_and_a_starting_one_is_waited_for() {
        assert_eq!(asks_about(LspStatus::Ready), AsksAbout::Server);
        assert_eq!(asks_about(LspStatus::Unavailable), AsksAbout::Elsewhere);
        assert_eq!(asks_about(LspStatus::Starting), AsksAbout::Wait);
    }

    /// A click made while the server is indexing says so, and says which server: the wait is
    /// tens of seconds on a cold project, and silence would read as a feature that does
    /// nothing.
    #[test]
    fn a_click_while_the_server_is_indexing_names_the_language_that_is_busy() {
        assert!(still_starting("src/lib.rs").starts_with("rust is still indexing"));
        assert!(still_starting("web/app.tsx").starts_with("typescript is still indexing"));
        assert!(still_starting("README.md").starts_with("the language server"));
    }
}

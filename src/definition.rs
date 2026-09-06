//! Modifier-click a name and land on where it is defined.
//!
//! What is here is the small part of that which is nobody's policy but the protocol's:
//! whether the server behind the file is worth a question, and how to read the answer it
//! gives - because a server that has not finished reading the project answers everything with
//! nothing at all, and nothing reads exactly like "this name is defined nowhere".
//!
//! A server that is still starting is asked anyway, and that is the whole of the change from
//! doing the obvious thing. rust-analyzer takes the better part of a minute over a cold
//! project, and a click that did nothing at all for that minute is a feature nobody would
//! believe in. Asking early is safe: the one refusal a server that is still indexing gives -
//! the protocol's `content modified` - is retried inside [`moon_lsp`], so an early question is
//! answered the moment there is an answer to it. What the wait really costs is only how an
//! *empty* answer reads, and keeping hold of that is what [`AsksAbout`] is for.
//!
//! The rest is deliberately absent. Where a jump lands, whether several places are offered or
//! the first is taken, and what a window says about any of it are the caller's, because they
//! are the caller's product. This crate has one answer to give, and it says plainly when it
//! has none.

use crate::source::LspStatus;

/// What a modifier-click does about the server behind the file it was made in.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum AsksAbout {
    /// Nothing here serves this file, so there is no question to put and nothing to wait for.
    /// Not a fault: it is the standing state of markdown, shell, SQL and every language
    /// nobody installed a server for, which is most of a repo and most machines.
    Nobody,
    /// Ask it, and take an empty answer for what it says - that the name is defined nowhere
    /// this server can see.
    Server,
    /// Ask it as well, though it has not finished reading the project: waiting out a cold
    /// index is worse than asking early, and asking early is safe. The one difference is how
    /// emptiness reads - "not yet", never "nowhere" - see [`still_starting`].
    ServerThatIsStillStarting,
}

impl AsksAbout {
    /// Whether there is a server to put the question to at all.
    pub fn asks(self) -> bool {
        self != AsksAbout::Nobody
    }

    /// Whether an empty answer to this question is only the wait showing through rather than
    /// the name being defined nowhere. Telling those two apart is the whole reason
    /// [`LspStatus`] has three states instead of two.
    pub fn an_empty_answer_is_only_the_wait(self) -> bool {
        self == AsksAbout::ServerThatIsStillStarting
    }
}

/// Which of the three a file's status comes to.
pub fn asks_about(status: LspStatus) -> AsksAbout {
    match status {
        LspStatus::Unavailable => AsksAbout::Nobody,
        LspStatus::Starting => AsksAbout::ServerThatIsStillStarting,
        LspStatus::Ready => AsksAbout::Server,
    }
}

/// What to say when a click was answered with nothing by a server that had not finished
/// reading the project.
///
/// rust-analyzer takes tens of seconds over a cold project, and every one of those seconds
/// would otherwise read as the name being defined nowhere - which is a bug the person goes
/// looking for rather than a wait they can sit out.
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

    /// A file nothing serves has nobody to ask. A server is asked whether it has finished
    /// reading the project or not - the wait is far too long to sit a click out - and what the
    /// waiting changes is only how an empty answer is allowed to read.
    #[test]
    fn a_server_is_asked_even_while_it_is_still_reading_the_project() {
        assert_eq!(asks_about(LspStatus::Unavailable), AsksAbout::Nobody);
        assert_eq!(asks_about(LspStatus::Ready), AsksAbout::Server);
        assert_eq!(
            asks_about(LspStatus::Starting),
            AsksAbout::ServerThatIsStillStarting
        );

        assert!(!asks_about(LspStatus::Unavailable).asks());
        assert!(asks_about(LspStatus::Ready).asks());
        assert!(asks_about(LspStatus::Starting).asks());

        // The distinction the whole of this is for: emptiness from a server that has read the
        // project is an answer, and emptiness from one that has not is the wait.
        assert!(
            !asks_about(LspStatus::Ready).an_empty_answer_is_only_the_wait(),
            "a ready server answering with nothing means there is nothing"
        );
        assert!(
            asks_about(LspStatus::Starting).an_empty_answer_is_only_the_wait(),
            "a starting server answering with nothing means it has not read the project yet"
        );
    }

    /// A click answered with nothing while the server is indexing says so, and says which
    /// server: the wait is tens of seconds on a cold project, and "no definition" would send
    /// the person looking for a bug that is really a wait.
    #[test]
    fn a_click_answered_while_the_server_is_indexing_names_the_language_that_is_busy() {
        assert!(still_starting("src/lib.rs").starts_with("rust is still indexing"));
        assert!(still_starting("web/app.tsx").starts_with("typescript is still indexing"));
        assert!(still_starting("README.md").starts_with("the language server"));
    }
}

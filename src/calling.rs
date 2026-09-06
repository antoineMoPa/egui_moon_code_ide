//! Taking a function from the list and getting a call: `greet` goes in as `greet()`, with the
//! caret between the parentheses so an argument can be typed straight away.
//!
//! This is the layer the decision belongs to, and neither of the two either side of it would
//! do. The widget puts what it is handed into the buffer and must never learn what a function
//! is - that is what keeps it a text editor rather than a component of this product. The
//! client carries the kind a server sent and reads nothing into it, because "a function is
//! written with parentheses after its name" is a fact about languages rather than about the
//! protocol. Here, where a server's answer is turned into the widget's rows, is the one place
//! that knows both.
//!
//! Nothing is guessed at. A server that sent no kind, or a kind newer than the client's list,
//! gets a plain insertion: parentheses written after something that is not called are a
//! character to delete, which is worse than the parenthesis nobody typed.

use egui_moon_editor::Completion;

use crate::source::{LspCompletion, LspCompletionKind, LspPosition};

/// Whether each sort of thing a server can offer is written with parentheses after its name.
///
/// A table with a row per kind rather than a match on the three that are called, so that the
/// business of it - which is a language question, and one people will want to argue about - is
/// laid out to be read and changed in one place. A kind missing from it is not called; see
/// [`takes_parentheses`], which is where that is decided rather than here.
///
/// The three that are called are the three a name is followed by an argument list for.
/// Everything else is either not callable at all or, like a class in the languages where
/// naming one calls its constructor, is offered as a constructor of its own when it is: a
/// `Struct` is the type's name, which is written bare far more often than it is called.
const TAKES_PARENTHESES: &[(LspCompletionKind, bool)] = &[
    (LspCompletionKind::Function, true),
    (LspCompletionKind::Method, true),
    (LspCompletionKind::Constructor, true),
    (LspCompletionKind::Text, false),
    (LspCompletionKind::Field, false),
    (LspCompletionKind::Variable, false),
    (LspCompletionKind::Class, false),
    (LspCompletionKind::Interface, false),
    (LspCompletionKind::Module, false),
    (LspCompletionKind::Property, false),
    (LspCompletionKind::Unit, false),
    (LspCompletionKind::Value, false),
    (LspCompletionKind::Enum, false),
    (LspCompletionKind::Keyword, false),
    (LspCompletionKind::Snippet, false),
    (LspCompletionKind::Color, false),
    (LspCompletionKind::File, false),
    (LspCompletionKind::Reference, false),
    (LspCompletionKind::Folder, false),
    (LspCompletionKind::EnumMember, false),
    (LspCompletionKind::Constant, false),
    (LspCompletionKind::Struct, false),
    (LspCompletionKind::Event, false),
    (LspCompletionKind::Operator, false),
    (LspCompletionKind::TypeParameter, false),
];

/// Whether taking this sort of thing should write the parentheses of a call after it.
///
/// `None` - a server that said nothing about what it offered - is not called, and neither is a
/// kind the table has no row for. Both are the same silence, and guessing wrong at it costs a
/// person a character to delete on every completion of whatever the server is vague about.
pub fn takes_parentheses(kind: Option<LspCompletionKind>) -> bool {
    let Some(kind) = kind else {
        return false;
    };
    TAKES_PARENTHESES
        .iter()
        .find(|(named, _)| *named == kind)
        .is_some_and(|(_, takes)| *takes)
}

/// The character the caret is sitting in front of, and `None` at the end of a line or past the
/// end of the text.
///
/// The one thing the decision needs of the buffer: completing over a call that is already
/// there - `gre|(x)` taking `greet` - must not write a second pair, or the line reads
/// `greet()(x)`. Taken against the place that was asked about, which is where the caret is:
/// [`Completing::answered`](crate::Completing::answered) only offers an answer while the word
/// and the place it was asked about are still the word and the place being typed.
pub fn follows_the_caret(text: &str, at: LspPosition) -> Option<char> {
    let line = moon_lsp::protocol::line_of(text, at.line)?;
    line.get(at.column..)?.chars().next()
}

/// One of the server's items as the widget's row, with the parentheses of a call where it is a
/// call and the caret asked for between them.
///
/// Three things each stop the parentheses on their own. A kind that is not called - see
/// [`takes_parentheses`]. An insertion the server already wrote a parenthesis into, which is a
/// server that has written the call itself and knows better than this does. And a caret already
/// sitting in front of one, which is a call being completed over rather than written.
pub fn row_for(item: LspCompletion, follows_the_caret: Option<char>) -> Completion {
    let writes_a_call = takes_parentheses(item.kind)
        && !item.insert.contains('(')
        && follows_the_caret != Some('(');
    match writes_a_call {
        // One byte back is between the parentheses, which is where an argument is typed.
        true => Completion {
            label: item.label,
            detail: item.detail,
            insert: format!("{}()", item.insert),
            caret_back: 1,
        },
        false => Completion {
            label: item.label,
            detail: item.detail,
            insert: item.insert,
            caret_back: 0,
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn offered(label: &str, kind: Option<LspCompletionKind>) -> LspCompletion {
        LspCompletion {
            label: label.to_string(),
            detail: None,
            insert: label.to_string(),
            kind,
        }
    }

    /// The point of the whole thing: a function comes out of the list as a call, with the caret
    /// where the first argument goes.
    #[test]
    fn taking_a_function_writes_the_parentheses_and_leaves_the_caret_between_them() {
        for kind in [
            LspCompletionKind::Function,
            LspCompletionKind::Method,
            LspCompletionKind::Constructor,
        ] {
            let row = row_for(offered("greet", Some(kind)), None);
            assert_eq!(row.insert, "greet()", "{kind:?}");
            assert_eq!(row.caret_back, 1, "{kind:?}");
            // What is read is still the name rather than the call: the parentheses are what
            // taking the row does, not what the row says.
            assert_eq!(row.label, "greet", "{kind:?}");
        }
    }

    /// Everything that is not called goes in exactly as the server sent it.
    #[test]
    fn taking_a_variable_or_a_field_or_a_module_writes_no_parentheses() {
        for kind in [
            LspCompletionKind::Variable,
            LspCompletionKind::Field,
            LspCompletionKind::Module,
            LspCompletionKind::Keyword,
            LspCompletionKind::Struct,
        ] {
            let row = row_for(offered("greeting", Some(kind)), None);
            assert_eq!(row.insert, "greeting", "{kind:?}");
            assert_eq!(row.caret_back, 0, "{kind:?}");
        }
    }

    /// A server that said nothing about what it offered is not read as having said "function".
    /// Guessing wrong here costs a character to delete on every completion.
    #[test]
    fn an_item_with_no_kind_at_all_is_not_taken_for_a_call() {
        let row = row_for(offered("greet", None), None);
        assert_eq!(row.insert, "greet");
        assert_eq!(row.caret_back, 0);
        assert!(!takes_parentheses(None));
    }

    /// Completing over a call that is already there: the parentheses are the ones already in
    /// the line rather than a second pair, which would leave `greet()(x)`.
    #[test]
    fn a_caret_already_in_front_of_a_parenthesis_gets_no_second_pair() {
        let row = row_for(
            offered("greet", Some(LspCompletionKind::Function)),
            Some('('),
        );
        assert_eq!(row.insert, "greet");
        assert_eq!(row.caret_back, 0);

        // And the character that says so is read off the buffer at the place asked about.
        let text = "let x = 1;\nlet y = gre(x);\n";
        let at = LspPosition {
            line: 1,
            column: "let y = gre".len(),
        };
        assert_eq!(follows_the_caret(text, at), Some('('));
    }

    /// A server that wrote the call itself is left to it, and the end of a line is nothing to
    /// read.
    #[test]
    fn a_server_that_wrote_the_call_itself_is_left_alone_and_a_line_ends_in_nothing() {
        let mut item = offered("greet", Some(LspCompletionKind::Function));
        item.insert = "greet(name)".to_string();
        let row = row_for(item, None);
        assert_eq!(row.insert, "greet(name)");
        assert_eq!(row.caret_back, 0);

        let text = "gre\n";
        assert_eq!(
            follows_the_caret(text, LspPosition { line: 0, column: 3 }),
            None
        );
        assert_eq!(
            follows_the_caret(text, LspPosition { line: 9, column: 0 }),
            None
        );
    }
}

//! When the signature of the call being typed is worth asking a server for, and which answer
//! is still the one to show.
//!
//! Asked only while the caret sits inside an unclosed `(` - a server answers with nothing
//! anywhere else, and a question a pause is a question a pause - and once the typing has
//! paused on text the server has heard: a signature asked about text it does not have is the
//! signature of another call. Each place is asked about once, and what was said stays on
//! screen while the typing goes on until the next answer replaces it: a popup that vanished
//! with every keystroke and came back at every pause would be worse than one a word behind.
//!
//! Nothing here calls anything or reads a clock; the caller passes the time in.

use std::time::{Duration, Instant};

use crate::{CanAnswer, LspPosition, LspSignature};

/// How long the caret has to have stayed put, on text the server has heard, before the call
/// it is in is asked about. Short, because the document sync has already waited for the typing
/// to stop before the server heard the text at all - see
/// [`TYPING_SETTLES_IN`](crate::TYPING_SETTLES_IN).
pub const SIGNATURE_SETTLES_IN: Duration = Duration::from_millis(150);

/// How far back from the caret an unclosed `(` is looked for. A call's arguments run to a few
/// lines; a scan of the whole file for every caret move would not be worth what it finds.
const LOOKS_BACK: usize = 4_000;

/// One place a signature was asked about: where the caret was, and how long the text was -
/// which is what tells the same caret in a text typed into since from the one asked about.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct SignedAt {
    /// Where the caret was.
    pub at: LspPosition,
    text_len: usize,
}

/// What to do about the caret this frame.
#[derive(Debug, PartialEq, Eq)]
pub enum SigningNext {
    /// Nothing to ask.
    Nothing,
    /// The caret has not stayed put long enough yet; draw again after this.
    Wait(Duration),
    /// Ask about the call at this place, and hand the answer back with
    /// [`Signing::answered`].
    Ask(SignedAt),
}

/// What has been asked about the call the caret is in, and what to show.
#[derive(Default)]
pub struct Signing {
    seen: Option<(SignedAt, Instant)>,
    asked: Option<SignedAt>,
    answered_at: Option<SignedAt>,
    shown: Option<LspSignature>,
}

impl Signing {
    /// Follow the caret through `text`.
    pub fn follow(
        &mut self,
        caret: Option<LspPosition>,
        text: &str,
        can_answer: CanAnswer,
        now: Instant,
    ) -> SigningNext {
        let Some(at) = caret.filter(|at| inside_a_call(text, at)) else {
            // Out of the call, so there is no call to show.
            self.seen = None;
            self.answered_at = None;
            self.shown = None;
            return SigningNext::Nothing;
        };
        let here = SignedAt {
            at,
            text_len: text.len(),
        };
        let since = match self.seen {
            Some((seen, since)) if seen == here => since,
            _ => {
                self.seen = Some((here, now));
                now
            }
        };
        if self.asked == Some(here)
            || self.answered_at == Some(here)
            || can_answer != CanAnswer::Yes
        {
            return SigningNext::Nothing;
        }
        let stayed = now.duration_since(since);
        if stayed < SIGNATURE_SETTLES_IN {
            return SigningNext::Wait(SIGNATURE_SETTLES_IN - stayed);
        }
        self.asked = Some(here);
        SigningNext::Ask(here)
    }

    /// What the server said about a place. An answer about a place that is no longer the one
    /// asked about is dropped; nothing, or a question that could not be answered, is nothing
    /// to show.
    pub fn answered(&mut self, asked: SignedAt, signature: Option<LspSignature>) {
        if self.asked != Some(asked) {
            return;
        }
        self.asked = None;
        self.answered_at = Some(asked);
        self.shown = signature;
    }

    /// The signature to show, if there is one.
    pub fn showing(&self) -> Option<&LspSignature> {
        self.shown.as_ref()
    }

    /// Put the signature away - Escape - until the caret moves to a place worth asking about.
    pub fn dismiss(&mut self) {
        self.shown = None;
    }
}

/// Whether the caret sits inside an unclosed `(`, looking back from it - the one cheap thing
/// every language here agrees a call looks like. A `;` or a brace at the same depth first is
/// a statement or a block, not the inside of a call.
pub fn inside_a_call(text: &str, at: &LspPosition) -> bool {
    let Ok(offset) = moon_lsp::edits::offset_of(text, at) else {
        return false;
    };
    let mut depth = 0usize;
    for character in text[..offset].chars().rev().take(LOOKS_BACK) {
        match character {
            ')' => depth += 1,
            '(' if depth == 0 => return true,
            '(' => depth -= 1,
            ';' | '{' | '}' if depth == 0 => return false,
            _ => {}
        }
    }
    false
}

#[cfg(test)]
mod tests {
    use super::*;

    fn at(line: usize, column: usize) -> LspPosition {
        LspPosition { line, column }
    }

    fn signature() -> LspSignature {
        LspSignature {
            label: "fn add(a: u32, b: u32) -> u32".to_string(),
            active_parameter: Some(15..21),
            documentation: None,
        }
    }

    #[test]
    fn the_caret_is_inside_a_call_only_behind_an_unclosed_parenthesis() {
        let text = "fn main() { add(1, max(2, 3), ); }";
        assert!(inside_a_call(text, &at(0, "fn main() { add(1, ".len())));
        assert!(inside_a_call(
            text,
            &at(0, "fn main() { add(1, max(2, 3), ".len())
        ));
        assert!(!inside_a_call(text, &at(0, "fn main() { ".len())));
        assert!(!inside_a_call(text, &at(0, "fn main(".len() + 1)));
    }

    /// Asked once the caret has stayed put, once per place, and kept on screen while the
    /// typing goes on until the next answer replaces it.
    #[test]
    fn a_call_is_asked_about_once_the_caret_has_settled_and_the_answer_stays_while_typing() {
        let start = Instant::now();
        let mut signing = Signing::default();
        let text = "add(1, ";
        let caret = Some(at(0, text.len()));

        assert_eq!(
            signing.follow(caret, text, CanAnswer::Yes, start),
            SigningNext::Wait(SIGNATURE_SETTLES_IN)
        );
        let SigningNext::Ask(asked) =
            signing.follow(caret, text, CanAnswer::Yes, start + SIGNATURE_SETTLES_IN)
        else {
            panic!("the caret settled inside a call");
        };
        signing.answered(asked, Some(signature()));
        assert_eq!(signing.showing(), Some(&signature()));
        assert_eq!(
            signing.follow(
                caret,
                text,
                CanAnswer::Yes,
                start + SIGNATURE_SETTLES_IN * 2
            ),
            SigningNext::Nothing
        );

        // Typing on: the text the server has not heard is not asked about, and what was said
        // stays up.
        let typed = "add(1, 2";
        assert_eq!(
            signing.follow(
                Some(at(0, typed.len())),
                typed,
                CanAnswer::NotThisText,
                start + SIGNATURE_SETTLES_IN * 3
            ),
            SigningNext::Nothing
        );
        assert!(signing.showing().is_some());

        // Out of the call, and it goes.
        let closed = "add(1, 2)";
        signing.follow(
            Some(at(0, closed.len())),
            closed,
            CanAnswer::Yes,
            start + SIGNATURE_SETTLES_IN * 4,
        );
        assert_eq!(signing.showing(), None);
    }

    /// An answer about a place the caret has left is not the answer to show.
    #[test]
    fn an_answer_about_a_place_since_left_is_dropped() {
        let start = Instant::now();
        let mut signing = Signing::default();
        let text = "add(1, ";
        signing.follow(Some(at(0, 4)), text, CanAnswer::Yes, start);
        let SigningNext::Ask(first) = signing.follow(
            Some(at(0, 4)),
            text,
            CanAnswer::Yes,
            start + SIGNATURE_SETTLES_IN,
        ) else {
            panic!("the caret settled inside a call");
        };
        signing.follow(
            Some(at(0, 7)),
            text,
            CanAnswer::Yes,
            start + SIGNATURE_SETTLES_IN,
        );
        signing.follow(
            Some(at(0, 7)),
            text,
            CanAnswer::Yes,
            start + SIGNATURE_SETTLES_IN * 2,
        );
        signing.answered(first, Some(signature()));
        assert_eq!(signing.showing(), None);
    }
}

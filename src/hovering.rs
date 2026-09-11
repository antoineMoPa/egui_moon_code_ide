//! What resting the pointer on a name asks a server, and when.
//!
//! A hover is asked about once the pointer has rested on one word for [`POINTER_SETTLES_IN`]:
//! a pointer crossing a page passes over dozens of names, and a question for each would be a
//! queue of answers about words nobody is looking at any more. Only text the server has heard
//! is asked about - a word typed a moment ago sits at a place in a text the server does not
//! have - and each word is asked about once: an answer, even one with nothing in it, stands
//! until the pointer rests on another word.
//!
//! Nothing here calls anything or reads a clock; the caller passes the time in, which is what
//! lets the waiting be tested without either.

use std::time::{Duration, Instant};

use egui_moon_editor::Word;

use crate::CanAnswer;

/// How long the pointer has to rest on a word before it is asked about. Long enough that a
/// pointer on its way somewhere else asks nothing, short enough that one that stopped to read
/// is answered before the reader wonders.
pub const POINTER_SETTLES_IN: Duration = Duration::from_millis(350);

/// Where the pointer has rested, what was asked about it, and what came back.
#[derive(Default)]
pub struct Hovering {
    /// The word the pointer is on, and since when.
    resting: Option<(Word, Instant)>,
    /// The word a question is out about.
    asked: Option<Word>,
    /// The last answer, and the word it is about. `None` inside is a server with nothing to
    /// say about that word, which is kept so it is not asked again.
    answer: Option<(Word, Option<String>)>,
}

/// What to do about the pointer this frame.
#[derive(Debug, PartialEq, Eq)]
pub enum HoveringNext {
    /// Nothing: no word under the pointer, a word already asked about, or text the server
    /// cannot answer about yet.
    Nothing,
    /// The pointer is resting and has not rested long enough. A window nobody is moving the
    /// pointer over draws no more frames, so this is when to draw the next one.
    Wait(Duration),
    /// Ask about this word, and hand the answer back with [`Hovering::answered`].
    Ask(Word),
}

impl Hovering {
    /// Follow the pointer: `pointed` is the word under it this frame, and `can_answer` whether
    /// the server has heard the text it is in.
    pub fn follow(
        &mut self,
        pointed: Option<&Word>,
        can_answer: CanAnswer,
        now: Instant,
    ) -> HoveringNext {
        let Some(word) = pointed else {
            self.resting = None;
            return HoveringNext::Nothing;
        };
        let since = match &self.resting {
            Some((resting, since)) if resting == word => *since,
            _ => {
                self.resting = Some((word.clone(), now));
                now
            }
        };
        let already = self.asked.as_ref() == Some(word)
            || self
                .answer
                .as_ref()
                .is_some_and(|(answered, _)| answered == word);
        if already || can_answer != CanAnswer::Yes {
            return HoveringNext::Nothing;
        }
        let rested = now.duration_since(since);
        if rested < POINTER_SETTLES_IN {
            return HoveringNext::Wait(POINTER_SETTLES_IN - rested);
        }
        self.asked = Some(word.clone());
        HoveringNext::Ask(word.clone())
    }

    /// What the server said about a word. An answer about a word that is no longer the one
    /// asked about - the pointer moved on and something else was asked - is dropped. A
    /// question that could not be answered is handed back as `None`, the same as nothing to
    /// say: either way the word is not asked about again while the pointer stays on it.
    pub fn answered(&mut self, word: &Word, markdown: Option<String>) {
        if self.asked.as_ref() != Some(word) {
            return;
        }
        self.asked = None;
        self.answer = Some((word.clone(), markdown));
    }

    /// What to show about the word under the pointer, if the server said anything about it.
    pub fn showing(&self, pointed: Option<&Word>) -> Option<&str> {
        let (answered, markdown) = self.answer.as_ref()?;
        match pointed == Some(answered) {
            true => markdown.as_deref(),
            false => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use egui_moon_editor::TextPoint;

    fn word(text: &str, offset: usize) -> Word {
        Word {
            text: text.to_string(),
            at: TextPoint {
                offset,
                line: 0,
                column: offset,
            },
        }
    }

    /// The pointer rests, and the word is asked about once it has rested long enough - and
    /// only the once.
    #[test]
    fn a_word_is_asked_about_once_the_pointer_has_rested_on_it() {
        let start = Instant::now();
        let greet = word("greet", 7);
        let mut hovering = Hovering::default();

        assert_eq!(
            hovering.follow(Some(&greet), CanAnswer::Yes, start),
            HoveringNext::Wait(POINTER_SETTLES_IN)
        );
        assert_eq!(
            hovering.follow(Some(&greet), CanAnswer::Yes, start + POINTER_SETTLES_IN),
            HoveringNext::Ask(greet.clone())
        );
        assert_eq!(
            hovering.follow(Some(&greet), CanAnswer::Yes, start + POINTER_SETTLES_IN * 2),
            HoveringNext::Nothing,
            "the question is out"
        );

        hovering.answered(&greet, Some("fn greet()".to_string()));
        assert_eq!(hovering.showing(Some(&greet)), Some("fn greet()"));
        assert_eq!(
            hovering.follow(Some(&greet), CanAnswer::Yes, start + POINTER_SETTLES_IN * 3),
            HoveringNext::Nothing,
            "answered, so not asked again"
        );
    }

    /// A pointer passing over a word asks nothing about it, and the wait starts again on the
    /// next word it rests on.
    #[test]
    fn a_pointer_passing_over_words_asks_about_none_of_them() {
        let start = Instant::now();
        let mut hovering = Hovering::default();
        let half = POINTER_SETTLES_IN / 2;

        hovering.follow(Some(&word("one", 0)), CanAnswer::Yes, start);
        assert_eq!(
            hovering.follow(Some(&word("two", 4)), CanAnswer::Yes, start + half),
            HoveringNext::Wait(POINTER_SETTLES_IN)
        );
        assert_eq!(
            hovering.follow(None, CanAnswer::Yes, start + half * 2),
            HoveringNext::Nothing
        );
    }

    /// Text the server has not heard is not asked about: the word sits at a place in a text
    /// the server does not have.
    #[test]
    fn a_word_in_text_the_server_has_not_heard_is_not_asked_about() {
        let start = Instant::now();
        let greet = word("greet", 7);
        let mut hovering = Hovering::default();
        hovering.follow(Some(&greet), CanAnswer::NotThisText, start);
        assert_eq!(
            hovering.follow(
                Some(&greet),
                CanAnswer::NotThisText,
                start + POINTER_SETTLES_IN
            ),
            HoveringNext::Nothing
        );
        assert_eq!(
            hovering.follow(Some(&greet), CanAnswer::Yes, start + POINTER_SETTLES_IN),
            HoveringNext::Ask(greet)
        );
    }

    /// An answer is shown only while the pointer is on the word it is about, and an answer
    /// about a word that is no longer the one asked about is dropped.
    #[test]
    fn an_answer_is_shown_only_over_its_own_word() {
        let start = Instant::now();
        let greet = word("greet", 7);
        let name = word("name", 13);
        let mut hovering = Hovering::default();
        hovering.follow(Some(&greet), CanAnswer::Yes, start);
        hovering.follow(Some(&greet), CanAnswer::Yes, start + POINTER_SETTLES_IN);
        hovering.answered(&greet, Some("fn greet()".to_string()));
        assert_eq!(hovering.showing(Some(&name)), None);

        // Onto another word, asked about, and the late answer about the first is not taken.
        hovering.follow(Some(&name), CanAnswer::Yes, start + POINTER_SETTLES_IN * 2);
        hovering.follow(Some(&name), CanAnswer::Yes, start + POINTER_SETTLES_IN * 3);
        hovering.answered(&greet, Some("stale".to_string()));
        assert_eq!(hovering.showing(Some(&greet)), Some("fn greet()"));
        hovering.answered(&name, Some("name: &str".to_string()));
        assert_eq!(hovering.showing(Some(&name)), Some("name: &str"));
    }
}

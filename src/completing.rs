//! Finishing what is being typed, out of what the language server behind the file knows.
//!
//! The editor draws the list and puts the chosen row into the text - it owns the buffer, and
//! the keyboard while a list is up. What is here is the other half: deciding when the question
//! is worth asking, and checking that the answer is still an answer when it lands. Nothing
//! here ever touches the text.
//!
//! Two things are worth asking about, and only one of them is a word. A half-typed name is
//! the obvious one. The other is a caret sitting behind a character the server itself said
//! opens a list - the `.` of `thing.`, the `:` of a path, the `(` of a call - where there is
//! no word at all and the whole point is to be shown what could go there. Which characters
//! those are is never guessed at here: the server declares them as it starts and they are
//! carried in, because `.` and `:` are rust-analyzer's answer and `.`, `/` and `@` are
//! typescript's, and a table written here would be one language's punctuation applied to
//! every language.
//!
//! Three things make that a decision rather than a call. A file no server serves - markdown,
//! configuration, and most of a repo - asks nothing, ever, and costs a match on an enum per
//! frame. A question is only worth asking once the typing has stopped, because a call a
//! keystroke floods the server and, over a network, the link. And a question is only
//! answerable about text the server has already been told about, by a server that has
//! finished reading the project - which is why this sits on top of [`Served`] rather than
//! beside it. Asking about a caret in text the server has never heard of gets a confident
//! answer about the wrong file, and asking a server that is still indexing gets an empty one,
//! which reads exactly like a real answer of "there is nothing to finish this with". Both are
//! waited out rather than answered, and waiting leaves the word askable: the word half typed
//! while a server was starting is offered its rows the moment it is ready, without another
//! letter being typed to unstick it.
//!
//! The last of the three is what most of the state below is for. Typing does not stop while a
//! request is out, so an answer routinely lands for a word that is no longer being typed, and
//! a list of names for a word the person has already finished is worse than no list at all.
//! Every request remembers what it was about and every answer is checked against what is being
//! typed when it lands.

use std::time::Instant;

use egui_moon_editor::{Completion, EditorOutput, TextPoint};

use crate::{
    calling,
    document::{CanAnswer, TYPING_SETTLES_IN},
    source::{LspCompletion, LspPosition},
};

/// The most rows offered at once, however many the server named.
///
/// A server answers a bare prefix with everything in scope, which for rust-analyzer is
/// thousands of items. Only a handful are ever on screen, and the rest are a list nobody
/// scrolls, walked over in the editor's request every frame.
const MOST_ROWS: usize = 50;

/// A question about one place: what to ask, and what to check the answer against when it
/// lands.
///
/// Handed out by [`Completing::follow`] and handed back to [`Completing::answered`]. It is
/// opaque on purpose - the only thing a caller does with it is put it to a
/// [`LanguageSource`](crate::LanguageSource) and give it back.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Asked {
    /// What has been typed of the name so far, which the answer is filtered against - and
    /// `None` for a question asked at a place where nothing has been typed towards anything,
    /// which is what a trigger character asks. Those two are really different questions: one
    /// is "which of these finishes `gre`" and the other is "what can go here at all", and an
    /// empty string for the second would filter every row in and read as the first.
    ///
    /// Two questions that read the same in two places are still two questions, so where it
    /// is counts as much as what it says.
    prefix: Option<String>,
    /// Where the caret sits - the end of the word, which is the place a server is asked what
    /// could finish it rather than what could stand in front of it.
    ///
    /// Straight from the editor: the line counts from zero and the column is bytes into that
    /// line, which is exactly what [`LspPosition`] is. What the server counts in is settled
    /// inside `moon_lsp`, and there is nothing to convert here.
    line: usize,
    column: usize,
}

impl Asked {
    /// Where to ask.
    pub fn at(&self) -> LspPosition {
        LspPosition {
            line: self.line,
            column: self.column,
        }
    }

    /// What has been typed of the name so far, for a caller that wants to say what it is
    /// asking about. `None` where the question is what could go here at all.
    pub fn prefix(&self) -> Option<&str> {
        self.prefix.as_deref()
    }

    /// A question about a half-typed word that the editor never handed out, for the tests
    /// that drive the worker without a window to read a caret out of.
    #[cfg(test)]
    pub(crate) fn about(word: &str, line: usize, column: usize) -> Self {
        Self {
            prefix: Some(word.to_string()),
            line,
            column,
        }
    }
}

/// What is on offer under the caret, and the question it answers.
struct Offering {
    answers: Asked,
    rows: Vec<Completion>,
}

/// What the caret is sitting behind this frame, which is the half of the decision the editor
/// widget does not report on its own.
///
/// Two things, because on their own neither says anything: the character just typed, read off
/// the buffer at the caret, and the characters the server behind this file said open a list.
/// A `.` is a trigger in every language anybody uses and a `,` is a trigger in none of them,
/// and the only place that difference is written down is the server's own `initialize` reply -
/// see [`LanguageSource::trigger_characters`](crate::LanguageSource::trigger_characters).
///
/// [`Default`] is a caret behind nothing with no server to have said anything, which asks
/// about words and nothing else - what this crate did before a server was asked what opens a
/// list.
#[derive(Default, Clone, Copy)]
pub struct AtTheCaret<'a> {
    /// The character the caret sits behind - see
    /// [`before_the_caret`](crate::before_the_caret). `None` at the start of a line.
    pub typed: Option<char>,
    /// What the server said opens a list on its own. Empty for a server that named none, and
    /// for a file whose server has not started yet.
    pub triggers: &'a [char],
}

impl AtTheCaret<'_> {
    /// Whether what the caret sits behind is one of the server's own triggers, which is the
    /// whole of "should a list open here with nothing typed towards it".
    fn opens_a_list(&self) -> bool {
        self.typed
            .is_some_and(|typed| self.triggers.contains(&typed))
    }
}

/// What one view is doing about finishing what is being typed in it.
///
/// One per open file rather than one per window: two files each have their own caret, their
/// own word and their own question out about it.
#[derive(Default)]
pub struct Completing {
    /// The question the caret is on - a word being typed, or a place behind a trigger
    /// character - and the moment it became that question. Together they are how long the
    /// typing has been stopped for, which is what the pause is measured against.
    ///
    /// The same pause as a word's, deliberately: a trigger is not asked about any sooner
    /// than a word is, because the answer would be about text the server has not been sent
    /// yet - the document goes out on the same lull - and because `::` and `->` are two
    /// keystrokes, where firing on the first would put a list up that the second one has to
    /// take down again.
    typing: Option<Asked>,
    typing_since: Option<Instant>,
    /// The question that is out, if there is one. One at a time per view, so what comes back
    /// is always the answer to this.
    asked: Option<Asked>,
    /// What the editor is being offered this frame.
    offering: Option<Offering>,
    /// The question there is nothing more to offer for: Escape was pressed on it, a row was
    /// taken on it, or it was asked and the answer was nothing at all. It is never asked
    /// again - Escape means stop offering, and a list that pops straight back up is the most
    /// annoying possible outcome.
    ///
    /// One question rather than one word, which is what keeps a dismissal from putting the
    /// list out for the rest of the file: it is the prefix *and* the place, so another
    /// letter, another caret, and the same `.` typed anywhere else are each a different
    /// question and a fair one again.
    nothing_more_for: Option<Asked>,
}

/// What the view does about asking, on the frame it has just drawn.
#[derive(PartialEq, Eq, Debug)]
pub enum CompletingNext {
    /// Nothing to ask about under the caret, the answer already in hand, a question already
    /// out, or one already put away.
    Nothing,
    /// There is something worth asking about, but not yet: the typing has not stopped for long
    /// enough, or the server cannot answer about this text yet. Either way the frame it
    /// becomes worth asking on has to be drawn, and a window nobody is typing in draws no
    /// more of them - so the caller asks for one.
    ///
    /// Waiting is deliberately not answering: nothing is asked, so nothing comes back empty,
    /// so the word is not written off as one with nothing to offer.
    Wait,
    /// Ask what could be typed at the place the caret is on, and give the answer back to
    /// [`Completing::answered`] along with this.
    Ask(Asked),
}

impl Completing {
    /// What the editor is offered this frame. Empty offers nothing, which is the usual state.
    pub fn on_offer(&self) -> &[Completion] {
        match &self.offering {
            Some(offering) => &offering.rows,
            None => &[],
        }
    }

    /// Take in what the editor reported, put the list away if it has stopped being an answer,
    /// and say whether the place the caret is on is worth a question.
    ///
    /// Called with the editor's output in hand, because that output is where the word being
    /// typed, the caret and the fate of the last list all come from. `at_the_caret` is the
    /// one thing the widget cannot report on its own - what the caret sits behind, and what
    /// the server said about that character; see [`AtTheCaret`].
    pub fn follow(
        &mut self,
        output: &EditorOutput,
        at_the_caret: AtTheCaret<'_>,
        can_answer: CanAnswer,
        now: Instant,
    ) -> CompletingNext {
        let asking = asked_at(
            output.word_being_typed.as_ref().map(|word| word.text.as_str()),
            output.caret.as_ref(),
            at_the_caret,
        );
        self.put_away(output, asking.as_ref(), output.response.has_focus());
        self.saw(asking.as_ref(), now);
        match self.next(can_answer, now) {
            Next::Nothing => CompletingNext::Nothing,
            Next::Wait => CompletingNext::Wait,
            Next::Ask => {
                let asked = self
                    .typing
                    .clone()
                    .expect("a place to ask about is what makes this an ask");
                self.asked = Some(asked.clone());
                CompletingNext::Ask(asked)
            }
        }
    }

    /// An answer has come back. It is only offered if it is still an answer to the word being
    /// typed: the caret has kept moving the whole time the question was out.
    ///
    /// `answered` is `None` for a server that could not answer, which offers nothing and says
    /// nothing about it: a completion list is an offer, and an offer that did not come is not
    /// a fault.
    ///
    /// `follows_the_caret` is the character the caret sits in front of - see
    /// [`calling::follows_the_caret`], which is what reads it off the buffer. It is what stops
    /// a call being completed over from being given a second pair of parentheses, and it is
    /// taken here rather than at the frame the row is taken because this is where the rows are
    /// made, and a row is only ever offered while the place it was asked about is still the
    /// place being typed.
    pub fn answered(
        &mut self,
        asked: &Asked,
        answered: Option<Vec<LspCompletion>>,
        follows_the_caret: Option<char>,
    ) {
        if self.asked.as_ref() == Some(asked) {
            self.asked = None;
        }
        // The word has moved on, or has been put away since. Either way this is a list of
        // names for text that is no longer there, which is worse than no list.
        if self.typing.as_ref() != Some(asked) {
            return;
        }
        let rows = answered.map_or_else(Vec::new, |rows| {
            rows_for(asked.prefix(), rows, follows_the_caret)
        });
        if rows.is_empty() {
            // Asked and answered with nothing. Nothing is gained by asking the same question
            // again every time the typing stops.
            self.nothing_more_for = Some(asked.clone());
            return;
        }
        self.offering = Some(Offering {
            answers: asked.clone(),
            rows,
        });
    }

    /// Take note of the question the caret is on, so the pause is measured from the last time
    /// it changed rather than from the first time it was worth asking about.
    fn saw(&mut self, asking: Option<&Asked>, now: Instant) {
        if self.typing.as_ref() != asking {
            self.typing = asking.cloned();
            self.typing_since = Some(now);
        }
    }

    /// Put the list away once it has stopped being an answer to what is being typed.
    ///
    /// Four things end a list, and they are all here rather than spread over the frame: a row
    /// taken, Escape, the caret leaving the word the list finishes, and the editor losing the
    /// keyboard - a popup left hanging over a view that has moved on is the thing to avoid.
    fn put_away(&mut self, output: &EditorOutput, asking: Option<&Asked>, focused: bool) {
        if output.completion_taken.is_some() || output.completion_dismissed {
            // The question under the caret *now*, which after a taken row is the word the
            // take just put there: that is the one nothing more is offered for.
            self.nothing_more_for = asking.cloned();
            self.offering = None;
            return;
        }
        let answers_something_else = self
            .offering
            .as_ref()
            .is_some_and(|offering| asking != Some(&offering.answers));
        if !focused || answers_something_else {
            self.offering = None;
        }
        if self.nothing_more_for.as_ref() != asking {
            self.nothing_more_for = None;
        }
    }

    /// Whether to ask. Pure, so the pause and every reason not to ask are tested without a
    /// clock, a server or a window.
    fn next(&self, can_answer: CanAnswer, now: Instant) -> Next {
        let (Some(typing), Some(since)) = (&self.typing, self.typing_since) else {
            return Next::Nothing;
        };
        if self.nothing_more_for.as_ref() == Some(typing) {
            return Next::Nothing;
        }
        if self.asked.is_some() {
            return Next::Nothing;
        }
        if self
            .offering
            .as_ref()
            .is_some_and(|offering| &offering.answers == typing)
        {
            return Next::Nothing;
        }
        if now.duration_since(since) < TYPING_SETTLES_IN {
            return Next::Wait;
        }
        match can_answer {
            CanAnswer::Yes => Next::Ask,
            // Both of the other two are waits rather than answers. The server's copy being a
            // word behind is a wait of a moment - the document sync is already bringing it
            // up. A server still reading the project is a wait of tens of seconds. Neither is
            // asked, so neither can come back empty and write the word off.
            CanAnswer::NotThisText | CanAnswer::StillReadingTheProject => Next::Wait,
        }
    }
}

/// The same three answers as [`CompletingNext`], before the word to ask about has been taken
/// out of the state. Kept apart so [`Completing::next`] stays a pure reading of the state
/// rather than something that also hands a value out of it.
#[derive(PartialEq, Eq, Debug)]
enum Next {
    Nothing,
    Wait,
    Ask,
}

/// The rows a server's answer comes to, as the editor takes them.
///
/// Two decisions, and both of them are here because this is where a server's answer stops being
/// the protocol's and becomes something a person reads. Which rows survive: the protocol leaves
/// the filtering to whoever asked, and a server handed a position answers with everything that
/// could stand there, most of which does not begin with what has been typed. Offering those
/// would put a row nobody typed towards at the top of the list, where Enter takes it.
///
/// With nothing typed towards anything there is nothing to filter on, and the rows are offered
/// in the order the server sent them. That is not a shortcut: a server orders its own answer,
/// and what it puts first after a `.` is the field of the thing to the left rather than the
/// name that happens to sort first. Rearranging it here would be this crate second-guessing
/// the one thing the server knows better.
///
/// And what each row puts in, which for a function is a call - see [`calling::row_for`].
fn rows_for(
    prefix: Option<&str>,
    answered: Vec<LspCompletion>,
    follows_the_caret: Option<char>,
) -> Vec<Completion> {
    answered
        .into_iter()
        .filter(|row| prefix.is_none_or(|prefix| starts_the_same(&row.label, prefix)))
        .take(MOST_ROWS)
        .map(|row| calling::row_for(row, follows_the_caret))
        .collect()
}

/// The question the caret is on this frame, if it is on one.
///
/// Two ways to have one, and they are asked in the order they are written. A word being typed
/// is the first: the editor reports it, and what is asked is which names finish it. A caret
/// behind one of the server's own trigger characters is the second, and it is asked with no
/// prefix at all - `thing.` has no word to finish, and filtering the answer against anything
/// would throw away the very rows the `.` was typed to see. Anywhere else there is nothing to
/// ask.
///
/// It is the character under the caret rather than a keystroke that decides, because a
/// keystroke is not something this can see: the editor reports where the caret is, not how it
/// got there. Clicking to the right of a `.` therefore offers what could go there too, which
/// is the same question and the same right answer.
fn asked_at(
    word_being_typed: Option<&str>,
    caret: Option<&TextPoint>,
    at_the_caret: AtTheCaret<'_>,
) -> Option<Asked> {
    let caret = caret?;
    let asked = |prefix| Asked {
        prefix,
        line: caret.line,
        column: caret.column,
    };
    match word_being_typed {
        Some(word) => Some(asked(Some(word.to_string()))),
        None => at_the_caret.opens_a_list().then(|| asked(None)),
    }
}

/// The character the caret sits behind, and `None` at the start of a line or past the end of
/// the text.
///
/// The other half of [`AtTheCaret`], and the caller's to read because the caller is what owns
/// the buffer. The mirror of [`calling::follows_the_caret`], which reads the character on the
/// other side of the same caret for an entirely different reason.
pub fn before_the_caret(text: &str, at: LspPosition) -> Option<char> {
    moon_lsp::protocol::character_before(text, &at)
}

/// Whether a row reads as a way of finishing the word, ignoring case the way a list of names
/// should - someone typing `str` means `String` too.
fn starts_the_same(label: &str, word: &str) -> bool {
    let mut label = label.chars();
    word.chars()
        .all(|typed| label.next().is_some_and(|row| row.eq_ignore_ascii_case(&typed)))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    /// A question about a half-typed word, at the end of it.
    fn asked(word: &str) -> Asked {
        Asked {
            prefix: Some(word.to_string()),
            line: 3,
            column: word.len(),
        }
    }

    /// A question about a place with nothing typed towards anything, which is what a trigger
    /// character asks.
    fn asked_with_no_prefix(column: usize) -> Asked {
        Asked {
            prefix: None,
            line: 3,
            column,
        }
    }

    fn offered(labels: &[&str]) -> Vec<LspCompletion> {
        labels
            .iter()
            .map(|label| LspCompletion {
                label: label.to_string(),
                detail: None,
                insert: label.to_string(),
                kind: None,
            })
            .collect()
    }

    /// The point of the pause: a burst of typing asks once at the end of it rather than once a
    /// keystroke, which over a network is a round trip a letter.
    #[test]
    fn a_word_is_asked_about_once_the_typing_has_stopped_rather_than_once_a_keystroke() {
        let start = Instant::now();
        let mut completing = Completing::default();

        let mut typed = String::new();
        for (key, letter) in "greet".chars().enumerate() {
            typed.push(letter);
            let at = start + Duration::from_millis(20 * key as u64);
            completing.saw(Some(&asked(&typed)), at);
            assert_eq!(completing.next(CanAnswer::Yes, at), Next::Wait, "at key {key}");
        }

        let last = start + Duration::from_millis(20 * 4);
        assert_eq!(
            completing.next(CanAnswer::Yes, last + TYPING_SETTLES_IN / 2),
            Next::Wait
        );
        assert_eq!(
            completing.next(CanAnswer::Yes, last + TYPING_SETTLES_IN),
            Next::Ask
        );
    }

    /// Nothing is asked about a caret sitting on a space, and nothing is asked against a
    /// server that has not yet been told the text the caret is in.
    #[test]
    fn there_is_nothing_to_ask_with_no_word_and_nothing_to_ask_against_a_stale_document() {
        let now = Instant::now();
        let mut completing = Completing::default();
        completing.saw(None, now);
        assert_eq!(completing.next(CanAnswer::Yes, now), Next::Nothing);

        completing.saw(Some(&asked("greet")), now - TYPING_SETTLES_IN);
        assert_eq!(completing.next(CanAnswer::NotThisText, now), Next::Wait);
        assert_eq!(completing.next(CanAnswer::Yes, now), Next::Ask);
    }

    /// A server that has not finished reading the project answers every question with
    /// nothing, which reads exactly like a real answer of "there is nothing to finish this
    /// with". Nothing is asked while it is starting - and, the part that matters, the word
    /// half typed while it was starting is asked about the moment it is ready, rather than
    /// having been written off on the strength of a reply that never really answered.
    #[test]
    fn a_word_typed_while_the_server_is_starting_is_asked_about_once_it_is_ready() {
        let start = Instant::now();
        let long_enough = start + TYPING_SETTLES_IN;
        let mut completing = Completing::default();
        completing.saw(Some(&asked("greet")), start);

        // Tens of seconds of this, at a frame apiece, and not one question out of any of them.
        assert_eq!(
            completing.next(CanAnswer::StillReadingTheProject, long_enough),
            Next::Wait
        );

        // Nothing was asked, so nothing came back empty, so nothing was written off: the same
        // word, still askable, without another letter being typed to unstick it.
        assert!(completing.nothing_more_for.is_none());
        assert_eq!(completing.next(CanAnswer::Yes, long_enough), Next::Ask);
    }

    /// An answer that arrives for a word the person has already finished typing offers names
    /// for text that is no longer there, so it is dropped rather than shown.
    #[test]
    fn an_answer_for_a_word_that_has_moved_on_is_dropped() {
        let now = Instant::now();
        let mut completing = Completing::default();
        completing.saw(Some(&asked("gre")), now);
        completing.asked = Some(asked("gre"));

        // Two more letters went in while the question was out.
        completing.saw(Some(&asked("greet")), now);
        completing.answered(&asked("gre"), Some(offered(&["greet", "greeting"])), None);
        assert!(completing.on_offer().is_empty());
        // And the view is free to ask about the word that is really being typed.
        assert!(completing.asked.is_none());
        assert_eq!(
            completing.next(CanAnswer::Yes, now + TYPING_SETTLES_IN),
            Next::Ask
        );

        // The answer to that one lands while it is still the word being typed, and is shown.
        completing.asked = Some(asked("greet"));
        completing.answered(&asked("greet"), Some(offered(&["greet", "greeting"])), None);
        assert_eq!(completing.on_offer().len(), 2);
    }

    /// Escape means stop offering. Nothing is asked about that word again, so the list cannot
    /// pop straight back up over it; another letter is another word and a fair question.
    #[test]
    fn a_dismissed_word_is_never_asked_about_again_and_the_next_one_is() {
        let now = Instant::now();
        let long_enough = now + TYPING_SETTLES_IN;
        // As an Escape over `greet` leaves the view.
        let mut completing = Completing {
            nothing_more_for: Some(asked("greet")),
            ..Default::default()
        };

        completing.saw(Some(&asked("greet")), now);
        assert_eq!(completing.next(CanAnswer::Yes, long_enough), Next::Nothing);

        completing.saw(Some(&asked("greeting")), now);
        assert_eq!(completing.next(CanAnswer::Yes, long_enough), Next::Ask);
    }

    /// A server that answered with nothing is not asked the same question again every time
    /// the typing stops, and neither is one that could not answer at all.
    #[test]
    fn a_word_the_server_had_nothing_for_is_not_asked_about_again() {
        let now = Instant::now();
        let mut completing = Completing::default();
        completing.saw(Some(&asked("greet")), now);
        completing.asked = Some(asked("greet"));
        completing.answered(&asked("greet"), Some(Vec::new()), None);

        assert!(completing.on_offer().is_empty());
        assert_eq!(
            completing.next(CanAnswer::Yes, now + TYPING_SETTLES_IN),
            Next::Nothing
        );
    }

    /// The protocol leaves the filtering to whoever asked: only the rows that could finish
    /// what has been typed are offered, and never more than a screenful of lists.
    #[test]
    fn only_the_rows_that_could_finish_the_word_are_offered() {
        let rows = rows_for(
            Some("str"),
            offered(&["String", "str", "as_ref", "Struct", "u32"]),
            None,
        );
        let labels: Vec<&str> = rows.iter().map(|row| row.label.as_str()).collect();
        assert_eq!(labels, ["String", "str", "Struct"]);

        let many: Vec<String> = (0..200).map(|number| format!("greet{number}")).collect();
        let many: Vec<&str> = many.iter().map(String::as_str).collect();
        assert_eq!(rows_for(Some("greet"), offered(&many), None).len(), MOST_ROWS);
    }

    /// The detail and the text to insert are the server's, carried through untouched: the
    /// editor puts `insert` into the buffer, not the label that was read.
    #[test]
    fn a_server_row_becomes_an_editor_row_with_its_detail_and_its_insertion_intact() {
        let rows = rows_for(
            Some("gre"),
            vec![LspCompletion {
                label: "greet".to_string(),
                detail: Some("fn(&str) -> String".to_string()),
                insert: "greet(${1:name})".to_string(),
                kind: None,
            }],
            None,
        );

        assert_eq!(rows[0].label, "greet");
        assert_eq!(rows[0].detail.as_deref(), Some("fn(&str) -> String"));
        assert_eq!(rows[0].insert, "greet(${1:name})");
    }

    /// A function offered by the server is a call by the time the editor has it, and the row
    /// beside it is not - which is [`calling`]'s decision, taken on every row of every answer
    /// as it is turned into a list.
    #[test]
    fn a_function_in_an_answer_is_offered_as_a_call_and_the_variable_beside_it_is_not() {
        let now = Instant::now();
        let mut completing = Completing::default();
        completing.saw(Some(&asked("gre")), now);
        completing.asked = Some(asked("gre"));
        completing.answered(
            &asked("gre"),
            Some(vec![
                LspCompletion {
                    label: "greet".to_string(),
                    detail: None,
                    insert: "greet".to_string(),
                    kind: Some(crate::source::LspCompletionKind::Function),
                },
                LspCompletion {
                    label: "greeting".to_string(),
                    detail: None,
                    insert: "greeting".to_string(),
                    kind: Some(crate::source::LspCompletionKind::Variable),
                },
            ]),
            None,
        );

        let rows = completing.on_offer();
        assert_eq!(rows[0].insert, "greet()");
        assert_eq!(rows[0].caret_back, 1);
        assert_eq!(rows[1].insert, "greeting");
        assert_eq!(rows[1].caret_back, 0);
    }

    /// The same answer where the caret already sits in front of a parenthesis - `gre|(x)`,
    /// completing over a call that is already written. A second pair would leave `greet()(x)`.
    #[test]
    fn a_function_completed_over_an_existing_call_is_offered_without_a_second_pair() {
        let now = Instant::now();
        let mut completing = Completing::default();
        completing.saw(Some(&asked("gre")), now);
        completing.asked = Some(asked("gre"));
        completing.answered(
            &asked("gre"),
            Some(vec![LspCompletion {
                label: "greet".to_string(),
                detail: None,
                insert: "greet".to_string(),
                kind: Some(crate::source::LspCompletionKind::Function),
            }]),
            Some('('),
        );

        assert_eq!(completing.on_offer()[0].insert, "greet");
        assert_eq!(completing.on_offer()[0].caret_back, 0);
    }
    /// The whole point of the card: a `.` is worth a list of its own. What makes it worth one
    /// is the server having said so - the same `,` that means nothing anywhere means nothing
    /// here too, and a server that named no triggers at all is asked about nothing but words.
    #[test]
    fn a_character_the_server_calls_a_trigger_is_asked_about_with_no_prefix_and_nothing_else_is()
    {
        let caret = TextPoint {
            offset: 40,
            line: 3,
            column: 14,
        };
        let triggers = ['.', ':', '\'', '('];
        let behind = |typed| AtTheCaret {
            typed: Some(typed),
            triggers: &triggers,
        };

        // `thing.` - no word to finish, and the question is what can go there at all.
        let after_a_dot = asked_at(None, Some(&caret), behind('.'))
            .expect("a trigger the server named is worth asking about");
        assert_eq!(after_a_dot.prefix(), None);
        assert_eq!(after_a_dot.at().column, 14);

        // A comma is punctuation nobody's server named, and the start of a line is behind
        // nothing at all.
        assert_eq!(asked_at(None, Some(&caret), behind(',')), None);
        assert_eq!(
            asked_at(
                None,
                Some(&caret),
                AtTheCaret {
                    typed: None,
                    triggers: &triggers
                }
            ),
            None
        );

        // And a server that named none - or one whose answer has not come back yet - is asked
        // about words and nothing else, exactly as it was before any of this.
        assert_eq!(
            asked_at(
                None,
                Some(&caret),
                AtTheCaret {
                    typed: Some('.'),
                    triggers: &[]
                }
            ),
            None
        );

        // A word being typed is still a question about the word, whatever is behind it.
        assert_eq!(
            asked_at(Some("gre"), Some(&caret), behind('e'))
                .expect("a word is a question")
                .prefix(),
            Some("gre")
        );
    }

    /// With nothing typed towards anything there is nothing to filter on, and the rows stand
    /// in the order the server sent them: what it puts first after a `.` is the field of the
    /// thing to the left rather than whatever sorts first. Filtering a prefix-less answer
    /// against the empty string would let every row through anyway; re-sorting it would throw
    /// away the one thing the server knows better.
    #[test]
    fn an_answer_with_no_prefix_to_filter_on_keeps_the_order_the_server_sent_it_in() {
        let rows = rows_for(None, offered(&["zip", "alpha", "Middle", "u32"]), None);
        let labels: Vec<&str> = rows.iter().map(|row| row.label.as_str()).collect();
        assert_eq!(labels, ["zip", "alpha", "Middle", "u32"]);

        // The cap is what it always was: a `.` on a big type answers with hundreds of rows,
        // and the rest are a list nobody scrolls.
        let many: Vec<String> = (0..200).map(|number| format!("field{number}")).collect();
        let many: Vec<&str> = many.iter().map(String::as_str).collect();
        assert_eq!(rows_for(None, offered(&many), None).len(), MOST_ROWS);
    }

    /// Escape means stop offering *that*, not stop offering. A dismissal that outlived the
    /// question it was about would leave a file with no lists for the rest of the session,
    /// which is the same bug in a new place: the `.` typed after the dismissed word is a
    /// different question and a fair one.
    #[test]
    fn a_dismissed_word_does_not_stop_a_trigger_typed_after_it_from_being_asked_about() {
        let now = Instant::now();
        let long_enough = now + TYPING_SETTLES_IN;
        // As an Escape over `greet` leaves the view.
        let mut completing = Completing {
            nothing_more_for: Some(asked("greet")),
            ..Default::default()
        };
        completing.saw(Some(&asked("greet")), now);
        assert_eq!(completing.next(CanAnswer::Yes, long_enough), Next::Nothing);

        // `greet.` - the caret has moved and there is no word at all now.
        completing.saw(Some(&asked_with_no_prefix("greet.".len())), now);
        assert_eq!(completing.next(CanAnswer::Yes, long_enough), Next::Ask);
    }

    /// And the other half of the same rule: a place the server had nothing to offer at is not
    /// asked about again, but the next trigger is its own question. A `:` on its own is the
    /// case that made this matter - the server names `:` and answers the first one of a `::`
    /// with nothing, and the second one has to still be asked about.
    #[test]
    fn a_trigger_the_server_had_nothing_for_leaves_the_next_one_worth_asking_about() {
        let now = Instant::now();
        let long_enough = now + TYPING_SETTLES_IN;
        let mut completing = Completing::default();

        let first_colon = asked_with_no_prefix(4);
        completing.saw(Some(&first_colon), now);
        completing.asked = Some(first_colon.clone());
        completing.answered(&first_colon, Some(Vec::new()), None);
        assert!(completing.on_offer().is_empty());
        assert_eq!(completing.next(CanAnswer::Yes, long_enough), Next::Nothing);

        // The second `:` of `std::`, one column along.
        let second_colon = asked_with_no_prefix(5);
        completing.saw(Some(&second_colon), now);
        assert_eq!(completing.next(CanAnswer::Yes, long_enough), Next::Ask);

        // And what comes back for it is offered whole, since there is no prefix to filter on.
        completing.asked = Some(second_colon.clone());
        completing.answered(&second_colon, Some(offered(&["fs", "io", "vec"])), None);
        assert_eq!(completing.on_offer().len(), 3);
    }
}

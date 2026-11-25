//! Autocorrect and autocomplete: typing assistance from the body's spellchecker
//! ([`crate::Spelling`], macOS's).
//!
//! With autocorrect on, a word is checked as it's finished (Space, Enter, `-`, or punctuation once
//! typing pauses). A text replacement or a misspelling's correction replaces it in one undo step,
//! marked with grey dots while the caret stays near it. A misspelling with no correction is marked
//! with red dots. The caret resting on a marked word shows the suggestion pill, which offers the
//! original word to undo a correction, or up to three corrections and `Learn "…"`. Resting at the
//! end of a misspelled word being typed shows its one correction. The body's own spellcheck is off
//! meanwhile.
//!
//! Autocomplete shows what's likely next at the end of a line as ghost text, and Tab or → takes
//! it. The editor's own predictions come first: the names of a draft's people, and on its first
//! line a greeting to them (`Hi Priya`). Otherwise it offers the rest of the word being typed,
//! from the spellchecker's completions.
//!
//! The marks aren't in the document. They're kept by block and characters, and follow edits by
//! comparing the blocks' text before and after, as long as the word they mark still reads the
//! same, whole.

mod pill;
mod words;

use std::{collections::HashSet, ops::Range};

pub use pill::{SuggestionKind, Suggestions};
pub use words::{GREETING_LANGUAGES, Named, People};

use self::{pill::Pill, words::Completion};
use crate::{
    editor::Editor,
    model::{Pos, Selection},
};

/// How long the caret rests before a suggestion shows, and typing pauses after punctuation
/// before the word before it is finished.
const DEBOUNCE: f64 = 0.3;
/// The punctuation that finishes the word before it once typing pauses.
const PUNCTUATION: [char; 7] = ['.', ',', '!', '?', '"', ':', ';'];
/// How many characters past a corrected word the caret can go before its dots go for good.
const NEAR: usize = 3;
/// How long a mark made by re-checking an edited word stays hidden after typing pauses.
const REVEAL: f64 = 0.6;

/// The editor's typing assistance.
#[derive(Default)]
pub(crate) struct State {
    /// Autocorrect is on (`typingAssistance/enabled`).
    pub(crate) on: bool,
    /// Autocomplete is on (`typingAssistance/autocompleteEnabled`).
    pub(super) complete: bool,
    pub(super) annotations: Vec<Annotation>,
    /// The blocks' text the marks were last placed against, and the document's revision then.
    texts: Vec<String>,
    revision: Option<u64>,
    /// Words whose correction was undone, lower-cased. They're marked, and never corrected again
    /// in this body.
    pub(super) undone: HashSet<String>,
    /// Words people are named with, lower-cased, never corrected or marked (the people the draft
    /// may be to).
    pub(super) names: HashSet<String>,
    /// The revision and caret where punctuation was typed. Once typing pauses, the word before the
    /// punctuation is finished.
    punctuation: Option<(u64, Pos)>,
    /// The blocks where following the last edit dropped a mark (its word read otherwise).
    dropped: Vec<usize>,
    /// Capitalisations the user undid by editing them, lower-cased, which are never made again in
    /// this body.
    rejected_capitals: HashSet<String>,
    /// Whether a snippet was just inserted, so no word is finished until a key other than Space or
    /// Enter.
    after_snippet: bool,
    pub(super) pill: Option<Pill>,
    /// The revision and selection the pill was last looked for at.
    pub(super) looked: Option<(u64, Selection)>,
    /// The marked word whose pill was dismissed, which doesn't offer it again until the caret
    /// leaves.
    pub(super) skip: Option<(usize, Range<usize>)>,
    /// A word learned from the pill, for the app's hint.
    pub(crate) learned: Option<String>,
    /// The ghost text as it was first shown. Typing it takes it a letter at a time, and Backspace
    /// gives the letters back.
    pub(crate) completion: Option<Completion>,
    /// The people the draft is from and to, for names and greetings.
    pub(super) people: People,
    /// The languages whose greetings are offered.
    pub(super) languages: Vec<String>,
    /// Whether a greeting was the last thing taken, with only spaces and Backspace typed since, in
    /// which case no name is offered.
    pub(super) greeted: bool,
}

/// A marked word.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct Annotation {
    pub(super) block: usize,
    pub(super) range: Range<usize>,
    /// The text marked. The mark goes once the text reads otherwise.
    pub(super) word: String,
    pub(super) mark: Mark,
    /// Its dots wait for typing to pause.
    pub(super) hidden: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) enum Mark {
    /// A correction, from `original`. `near` is set while its dots show, which is until the caret
    /// leaves the word, and `capital` is set for a capitalisation.
    Corrected {
        original: String,
        near: bool,
        capital: bool,
    },
    /// Misspelled, with no correction, or one `undone`.
    Incorrect { undone: bool },
}

/// A word marked by autocorrect, as [`Editor::autocorrect_marks`] lists them.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MarkedWord {
    /// Its block.
    pub block: usize,
    /// Its characters in the block.
    pub range: Range<usize>,
    /// For a correction, the word as typed.
    pub corrected_from: Option<String>,
    /// Whether its dots show.
    pub dotted: bool,
}

/// Whether `ch` is part of a word.
pub(super) fn is_word_char(ch: char) -> bool {
    ch.is_alphanumeric() || matches!(ch, '\'' | '’')
}

/// The word ending at `end` in `chars`, without apostrophes around it.
pub(super) fn word_before(chars: &[char], end: usize) -> Option<Range<usize>> {
    let end = end.min(chars.len());
    let mut start = end;
    while start > 0 && is_word_char(chars[start - 1]) {
        start -= 1;
    }
    let (mut from, mut to) = (start, end);
    while from < to && !chars[from].is_alphanumeric() {
        from += 1;
    }
    while to > from && !chars[to - 1].is_alphanumeric() {
        to -= 1;
    }
    (from < to && chars[from..to].iter().any(|ch| ch.is_alphabetic())).then_some(from..to)
}

/// Whether `range` of `chars` reads `word` whole, with no word characters running on past its ends.
fn reads(chars: &[char], range: &Range<usize>, word: &str) -> bool {
    if range.end > chars.len() || chars[range.clone()].iter().copied().ne(word.chars()) {
        return false;
    }
    let edge = |at: usize| chars[at].is_alphanumeric();
    let before = range.start > 0 && edge(range.start) && is_word_char(chars[range.start - 1]);
    let after = range.end < chars.len() && edge(range.end - 1) && is_word_char(chars[range.end]);
    !before && !after
}

/// Whether `with` is `word` in other case.
pub(super) fn is_capitalisation(word: &str, with: &str) -> bool {
    with != word && with.to_lowercase() == word.to_lowercase()
}

/// How many characters `a` and `b` have in common at the start and, after that, at the end.
fn common_ends(a: &[char], b: &[char]) -> (usize, usize) {
    let prefix = a.iter().zip(b).take_while(|(x, y)| x == y).count();
    let suffix = a[prefix..]
        .iter()
        .rev()
        .zip(b[prefix..].iter().rev())
        .take_while(|(x, y)| x == y)
        .count();
    (prefix, suffix)
}

impl Editor {
    /// Turns autocorrect on or off. Its marks go either way.
    pub fn set_autocorrect(&mut self, on: bool) {
        if self.autocorrect.on != on {
            self.autocorrect.on = on;
            self.autocorrect.annotations.clear();
            self.autocorrect.pill = None;
            self.autocorrect.looked = None;
            self.autocorrect.punctuation = None;
        }
    }

    /// Turns autocomplete on or off.
    pub fn set_autocomplete(&mut self, on: bool) {
        if self.autocorrect.complete != on {
            self.autocorrect.complete = on;
            self.ghost = None;
            self.autocorrect.completion = None;
        }
    }

    /// Words autocorrect leaves alone: the names of the people the draft may be to, which a
    /// spellchecker doesn't know.
    pub fn set_names(&mut self, words: impl IntoIterator<Item = String>) {
        self.autocorrect.names = words.into_iter().map(|word| word.to_lowercase()).collect();
    }

    /// The words autocorrect has marked.
    pub fn autocorrect_marks(&mut self) -> Vec<MarkedWord> {
        self.sync_marks();
        self.autocorrect
            .annotations
            .iter()
            .map(|annotation| {
                let (corrected_from, dotted) = match &annotation.mark {
                    Mark::Corrected { original, near, .. } => (Some(original.clone()), *near),
                    Mark::Incorrect { .. } => (None, !annotation.hidden),
                };
                MarkedWord {
                    block: annotation.block,
                    range: annotation.range.clone(),
                    corrected_from,
                    dotted,
                }
            })
            .collect()
    }

    /// The dotted words in block `index`, as each one's characters and whether it's a correction
    /// (grey) rather than a misspelling (red).
    pub(crate) fn dotted_words(
        &self,
        index: usize,
    ) -> impl Iterator<Item = (Range<usize>, bool)> + '_ {
        self.autocorrect
            .annotations
            .iter()
            .filter(move |annotation| annotation.block == index)
            .filter_map(|annotation| match annotation.mark {
                Mark::Corrected { near: true, .. } => Some((annotation.range.clone(), true)),
                Mark::Corrected { near: false, .. } => None,
                Mark::Incorrect { .. } => {
                    (!annotation.hidden).then(|| (annotation.range.clone(), false))
                }
            })
    }

    /// Moves the marks to where the edits since they were placed left their words, dropping
    /// those whose words changed.
    pub(super) fn sync_marks(&mut self) {
        if self.autocorrect.revision == Some(self.revision) {
            return;
        }
        self.autocorrect.dropped.clear();
        let texts: Vec<String> = self.doc.blocks.iter().map(|block| block.text()).collect();
        if !self.autocorrect.annotations.is_empty() {
            let old = &self.autocorrect.texts;
            let prefix = old.iter().zip(&texts).take_while(|(a, b)| a == b).count();
            let most = old.len().min(texts.len()) - prefix;
            let suffix = old
                .iter()
                .rev()
                .zip(texts.iter().rev())
                .take(most)
                .take_while(|(a, b)| a == b)
                .count();
            let (old_changed, new_changed) =
                (prefix..old.len() - suffix, prefix..texts.len() - suffix);
            let chars: Vec<Vec<char>> = texts.iter().map(|text| text.chars().collect()).collect();
            let moved = |annotation: &Annotation| -> Option<(usize, Range<usize>)> {
                if annotation.block < old_changed.start {
                    return Some((annotation.block, annotation.range.clone()));
                }
                if annotation.block >= old_changed.end {
                    return Some((
                        annotation.block + texts.len() - old.len(),
                        annotation.range.clone(),
                    ));
                }
                let before: Vec<char> = old[annotation.block].chars().collect();
                // Within the first block changed, before what changed.
                if annotation.block == old_changed.start && !new_changed.is_empty() {
                    let (common, _) = common_ends(&before, &chars[new_changed.start]);
                    if annotation.range.end <= common {
                        return Some((new_changed.start, annotation.range.clone()));
                    }
                }
                // Within the last block changed, after what changed (and after what the first
                // block kept, if it's the same block).
                if annotation.block + 1 == old_changed.end && !new_changed.is_empty() {
                    let last = new_changed.end - 1;
                    let after = &chars[last];
                    let suffix = if old_changed.len() == 1 && new_changed.len() == 1 {
                        common_ends(&before, after).1
                    } else {
                        before
                            .iter()
                            .rev()
                            .zip(after.iter().rev())
                            .take_while(|(x, y)| x == y)
                            .count()
                    };
                    if annotation.range.start >= before.len() - suffix {
                        let shift = after.len() as isize - before.len() as isize;
                        let start = (annotation.range.start as isize + shift) as usize;
                        return Some((last, start..start + annotation.range.len()));
                    }
                }
                None
            };
            let annotations = std::mem::take(&mut self.autocorrect.annotations);
            let (mut kept, mut dropped, mut rejected) = (Vec::new(), Vec::new(), Vec::new());
            for annotation in annotations {
                match moved(&annotation).filter(|(block, range)| {
                    chars
                        .get(*block)
                        .is_some_and(|chars| reads(chars, range, &annotation.word))
                }) {
                    Some((block, range)) => kept.push(Annotation {
                        block,
                        range,
                        ..annotation
                    }),
                    None => {
                        dropped.push(new_changed.start.min(texts.len().saturating_sub(1)));
                        if let Mark::Corrected {
                            original,
                            capital: true,
                            ..
                        } = &annotation.mark
                        {
                            rejected.push(original.to_lowercase());
                        }
                    }
                }
            }
            self.autocorrect.annotations = kept;
            self.autocorrect.dropped = dropped;
            self.autocorrect.rejected_capitals.extend(rejected);
            if self
                .autocorrect
                .skip
                .as_ref()
                .is_some_and(|(block, range)| {
                    !self
                        .autocorrect
                        .annotations
                        .iter()
                        .any(|annotation| annotation.block == *block && annotation.range == *range)
                })
            {
                self.autocorrect.skip = None;
            }
        }
        self.autocorrect.texts = texts;
        self.autocorrect.revision = Some(self.revision);
    }

    pub(super) fn annotate(&mut self, block: usize, range: Range<usize>, mark: Mark) {
        self.sync_marks();
        let word: String = self.doc.blocks[block]
            .text()
            .chars()
            .skip(range.start)
            .take(range.len())
            .collect();
        self.autocorrect.annotations.retain(|annotation| {
            annotation.block != block
                || annotation.range.end <= range.start
                || annotation.range.start >= range.end
        });
        let at = self.autocorrect.annotations.partition_point(|annotation| {
            (annotation.block, annotation.range.start) < (block, range.start)
        });
        self.autocorrect.annotations.insert(
            at,
            Annotation {
                block,
                range,
                word,
                mark,
                hidden: false,
            },
        );
    }

    /// Replaces the word at `range` of block `block` with `with` in one undo step, keeping the
    /// caret where it was in the text around the word (after the word if the caret was in it).
    /// Returns the new word's characters.
    pub(super) fn replace_word(
        &mut self,
        block: usize,
        range: Range<usize>,
        with: &str,
    ) -> Range<usize> {
        self.sync_marks();
        self.undo.save(&self.doc, self.selection);
        let marks = self.doc.blocks[block].marks_at(range.start + 1);
        self.doc
            .delete(Pos::new(block, range.start), Pos::new(block, range.end));
        let end = self
            .doc
            .insert_text(Pos::new(block, range.start), with, marks);
        let len = end.offset - range.start;
        let place = |pos: Pos| match pos {
            pos if pos.block != block || pos.offset < range.start => pos,
            pos if pos.offset >= range.end => Pos::new(block, pos.offset - range.len() + len),
            _ => end,
        };
        self.selection = Selection::new(place(self.selection.anchor), place(self.selection.focus));
        self.changed();
        range.start..range.start + len
    }

    /// The word ending at `end` was finished, so it's corrected, marked, or left be.
    pub(crate) fn finish_word(&mut self, end: Pos) {
        if !self.autocorrect.on || self.spelling.engine.is_none() || self.autocorrect.after_snippet
        {
            return;
        }
        self.sync_marks();
        let Some(block) = self
            .doc
            .blocks
            .get(end.block)
            .filter(|block| block.is_inline() && !block.in_code())
        else {
            return;
        };
        let text = block.text();
        let chars: Vec<char> = text.chars().collect();
        let Some(range) = word_before(&chars, end.offset) else {
            return;
        };
        if self
            .autocorrect
            .annotations
            .iter()
            .any(|annotation| annotation.block == end.block && annotation.range == range)
        {
            return;
        }
        let word: String = chars[range.clone()].iter().collect();
        let key = word.to_lowercase();
        if self.autocorrect.names.contains(&key) {
            return;
        }
        let Some(engine) = self.spelling.engine.as_mut() else {
            return;
        };
        let replacement = engine.replacement(&word).filter(|with| *with != word);
        if replacement.is_none() && !engine.check(&text).contains(&range) {
            return;
        }
        // It's a capitalisation when the correction, or else macOS's likeliest guess, is the word
        // in other case.
        let correction = replacement.or_else(|| {
            engine
                .correction(&text, range.clone())
                .filter(|with| *with != word)
        });
        let correction = correction.or_else(|| {
            engine
                .guesses(&text, range.clone())
                .into_iter()
                .next()
                .filter(|guess| is_capitalisation(&word, guess))
        });
        let capital = correction
            .as_ref()
            .is_some_and(|with| is_capitalisation(&word, with));
        // One the user undid by editing it, or one already made of the word in its paragraph,
        // isn't made again.
        if capital
            && (self.autocorrect.rejected_capitals.contains(&key)
                || self.capitalised_in(end.block, &key))
        {
            return;
        }
        if !capital && self.autocorrect.undone.contains(&key) {
            self.annotate(end.block, range, Mark::Incorrect { undone: true });
            return;
        }
        match correction {
            Some(with) => {
                let corrected = self.replace_word(end.block, range, &with);
                self.annotate(
                    end.block,
                    corrected,
                    Mark::Corrected {
                        original: word,
                        near: true,
                        capital,
                    },
                );
            }
            None => self.annotate(end.block, range, Mark::Incorrect { undone: false }),
        }
    }

    /// Whether block `block` has had `key` (lower-cased) capitalised.
    fn capitalised_in(&self, block: usize, key: &str) -> bool {
        self.autocorrect.annotations.iter().any(|annotation| {
            annotation.block == block
                && annotation.word.to_lowercase() == key
                && matches!(
                    &annotation.mark,
                    Mark::Corrected { capital: true, .. } | Mark::Incorrect { undone: true }
                )
        })
    }

    /// A snippet was inserted at the caret, so the words it ends with aren't corrected as Space or
    /// Enter follow it, until another key is typed.
    pub fn snippet_inserted(&mut self) {
        self.autocorrect.after_snippet = true;
    }

    /// An edit at the caret (a character, Backspace, Delete) inside a word, or onto a marked one.
    /// The word is checked again, and a misspelling is marked (never corrected), with its dots
    /// showing once typing pauses.
    pub(crate) fn recheck_edited_word(&mut self) {
        if !self.autocorrect.on || self.autocorrect.after_snippet || !self.selection.is_collapsed()
        {
            return;
        }
        self.sync_marks();
        let caret = self.selection.focus;
        let Some(block) = self
            .doc
            .blocks
            .get(caret.block)
            .filter(|block| block.is_inline() && !block.in_code())
        else {
            return;
        };
        let text = block.text();
        let chars: Vec<char> = text.chars().collect();
        let word_at = |at: usize| chars.get(at).copied().is_some_and(is_word_char);
        let inside = caret.offset > 0 && word_at(caret.offset - 1) && word_at(caret.offset);
        let onto_mark = self.autocorrect.dropped.contains(&caret.block)
            && (caret.offset > 0 && word_at(caret.offset - 1) || word_at(caret.offset));
        if !inside && !onto_mark {
            return;
        }
        let end = (caret.offset..chars.len())
            .find(|at| !word_at(*at))
            .unwrap_or(chars.len());
        let Some(range) = word_before(&chars, end).filter(|range| range.start <= caret.offset)
        else {
            return;
        };
        let word: String = chars[range.clone()].iter().collect();
        let key = word.to_lowercase();
        if self.autocorrect.names.contains(&key)
            || self
                .autocorrect
                .annotations
                .iter()
                .any(|annotation| annotation.block == caret.block && annotation.range == range)
        {
            return;
        }
        let Some(engine) = self.spelling.engine.as_mut() else {
            return;
        };
        if engine.check(&text).contains(&range) {
            self.annotate(
                caret.block,
                range.clone(),
                Mark::Incorrect { undone: false },
            );
            if let Some(annotation) = self
                .autocorrect
                .annotations
                .iter_mut()
                .find(|annotation| annotation.block == caret.block && annotation.range == range)
            {
                annotation.hidden = true;
            }
        }
    }

    /// A character was typed at the caret. `-` finishes the word before it, and punctuation does
    /// once typing pauses. Autocomplete follows.
    pub(crate) fn typed_for_assistance(&mut self, ch: char) {
        if ch != ' ' {
            self.autocorrect.greeted = false;
        }
        let caret = self.selection.focus;
        if self.autocorrect.on && caret.offset > 0 {
            if ch == '-' {
                self.finish_word(Pos::new(caret.block, caret.offset - 1));
            } else if PUNCTUATION.contains(&ch) {
                self.autocorrect.punctuation = Some((self.revision, self.selection.focus));
            }
        }
        // The key after a snippet that isn't Space or Enter.
        if ch != ' ' {
            self.autocorrect.after_snippet = false;
        }
        if is_word_char(ch) {
            self.recheck_edited_word();
        }
        self.complete();
    }

    /// Backspace at the caret. An edited word is checked again, and autocomplete follows.
    pub(crate) fn deleted_for_assistance(&mut self) {
        self.recheck_edited_word();
        self.complete();
    }

    /// Updates the marks and the pill this frame, `idle` seconds after the caret or the document
    /// last changed. `focused` is whether the editor has the keyboard. Returns how long until
    /// something is due, if anything is.
    pub(crate) fn assist(&mut self, focused: bool, idle: f64) -> Option<f64> {
        if !self.autocorrect.on {
            return None;
        }
        self.sync_marks();
        let caret = self.selection.focus;
        for annotation in &mut self.autocorrect.annotations {
            if let Mark::Corrected { near, .. } = &mut annotation.mark
                && (annotation.block != caret.block
                    || caret.offset < annotation.range.start
                    || caret.offset > annotation.range.end + NEAR)
            {
                *near = false;
            }
        }
        if self
            .autocorrect
            .skip
            .as_ref()
            .is_some_and(|(block, range)| {
                *block != caret.block || caret.offset < range.start || caret.offset > range.end
            })
        {
            self.autocorrect.skip = None;
        }
        // A re-checked word's dots show once typing has paused.
        let hidden = self
            .autocorrect
            .annotations
            .iter()
            .any(|annotation| annotation.hidden);
        if hidden && idle >= REVEAL {
            self.autocorrect
                .annotations
                .iter_mut()
                .for_each(|annotation| annotation.hidden = false);
        }
        let reveal = (hidden && idle < REVEAL).then_some(REVEAL - idle);
        // The pill goes with any change or caret move, and is looked for again once the caret
        // rests.
        let now = (self.revision, self.selection);
        if self.autocorrect.looked != Some(now) {
            self.autocorrect.pill = None;
        }
        if !focused {
            self.autocorrect.pill = None;
            return reveal;
        }
        let wait = DEBOUNCE - idle;
        if let Some((revision, at)) = self.autocorrect.punctuation
            && (revision != self.revision || at != caret)
        {
            self.autocorrect.punctuation = None;
        }
        if wait > 0.0 {
            let due = (self.autocorrect.punctuation.is_some()
                || self.autocorrect.looked != Some(now))
            .then_some(wait);
            return due.into_iter().chain(reveal).reduce(f64::min);
        }
        if self.autocorrect.punctuation.take().is_some() {
            let chars: Vec<char> = self.doc.blocks[caret.block].text().chars().collect();
            let mut end = caret.offset;
            while end > 0 && PUNCTUATION.contains(&chars[end - 1]) {
                end -= 1;
            }
            self.finish_word(Pos::new(caret.block, end));
        }
        let now = (self.revision, self.selection);
        if self.autocorrect.looked != Some(now) {
            self.autocorrect.looked = Some(now);
            self.autocorrect.pill = self.find_pill();
            if self.autocorrect.pill.is_some() {
                self.ghost = None;
            }
        }
        reveal
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn chars(text: &str) -> Vec<char> {
        text.chars().collect()
    }

    #[test]
    fn the_word_before_leaves_out_apostrophes_and_numbers() {
        assert_eq!(word_before(&chars("I saw teh"), 9), Some(6..9));
        assert_eq!(word_before(&chars("don't "), 5), Some(0..5));
        assert_eq!(word_before(&chars("'quoted' "), 8), Some(1..7));
        assert_eq!(
            word_before(&chars("at 10"), 5),
            None,
            "numbers aren't words"
        );
        assert_eq!(word_before(&chars("a, "), 3), None);
    }

    #[test]
    fn a_word_reads_whole_or_not_at_all() {
        let text = chars("the cat, them");
        assert!(reads(&text, &(0..3), "the"));
        assert!(reads(&text, &(4..7), "cat"), "punctuation after it");
        assert!(!reads(&text, &(9..12), "the"), "a word runs on");
    }
}

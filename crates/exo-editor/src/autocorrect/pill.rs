//! The autocorrect suggestion pill: finding what it offers, keyboard and hover navigation, undo of
//! corrections, and learning words.

use std::ops::Range;

use egui::Key;

use super::{Mark, is_capitalisation, is_word_char, word_before};
use crate::{
    editor::Editor,
    model::{Pos, Selection},
};

/// At most this many corrections in the pill.
const SUGGESTIONS: usize = 3;

/// What the suggestion pill offers.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SuggestionKind {
    /// `UNDO`: the word as typed, before its correction.
    Undo,
    /// Corrections for a marked misspelling, then `Learn "…"`.
    Multiple,
    /// The correction of the misspelled word being typed.
    Single,
}

/// The suggestion pill as it shows.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Suggestions {
    /// What it offers.
    pub kind: SuggestionKind,
    /// The word it's for.
    pub word: String,
    /// Its items: the original word, or corrections.
    pub items: Vec<String>,
    /// Whether `Learn "{word}"` follows them.
    pub learn: bool,
    /// The item chosen with the keys (`Learn` after the items), or none until ↓.
    pub selected: Option<usize>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Pill {
    pub(crate) suggestions: Suggestions,
    /// The word's block and characters.
    pub(crate) block: usize,
    pub(crate) range: Range<usize>,
}

impl Editor {
    /// The suggestion pill, while it shows.
    pub fn suggestions(&self) -> Option<&Suggestions> {
        self.autocorrect.pill.as_ref().map(|pill| &pill.suggestions)
    }

    /// The block and characters of the pill's word, for placing the pill.
    pub(crate) fn pill_word(&self) -> Option<(usize, Range<usize>)> {
        self.autocorrect
            .pill
            .as_ref()
            .map(|pill| (pill.block, pill.range.clone()))
    }

    /// What the pill offers for the caret where it is.
    pub(super) fn find_pill(&mut self) -> Option<Pill> {
        let caret = self.selection.focus;
        if !self.selection.is_collapsed() || self.trigger.is_some() {
            return None;
        }
        let on = |range: &Range<usize>| range.start <= caret.offset && caret.offset <= range.end;
        let marked = self
            .autocorrect
            .annotations
            .iter()
            .find(|annotation| annotation.block == caret.block && on(&annotation.range))
            .cloned();
        let text = self.doc.blocks[caret.block].text();
        if let Some(annotation) = marked {
            if self.autocorrect.skip.as_ref() == Some(&(annotation.block, annotation.range.clone()))
            {
                return None;
            }
            let (kind, items, learn) = match &annotation.mark {
                Mark::Corrected {
                    original,
                    near: true,
                    ..
                } => (SuggestionKind::Undo, vec![original.clone()], false),
                Mark::Corrected { near: false, .. } => return None,
                Mark::Incorrect { .. } => {
                    let guesses = self
                        .spelling
                        .engine
                        .as_mut()?
                        .guesses(&text, annotation.range.clone());
                    let items: Vec<String> = guesses
                        .into_iter()
                        .filter(|guess| *guess != annotation.word)
                        .take(SUGGESTIONS)
                        .collect();
                    (SuggestionKind::Multiple, items, true)
                }
            };
            return Some(Pill {
                suggestions: Suggestions {
                    kind,
                    word: annotation.word.clone(),
                    items,
                    learn,
                    selected: None,
                },
                block: annotation.block,
                range: annotation.range,
            });
        }
        // For the misspelled word just typed, its correction.
        if !self.spelling.typing {
            return None;
        }
        let chars: Vec<char> = text.chars().collect();
        let range = word_before(&chars, caret.offset).filter(|range| {
            range.end == caret.offset && chars.get(caret.offset).is_none_or(|ch| !is_word_char(*ch))
        })?;
        let word: String = chars[range.clone()].iter().collect();
        if self.autocorrect.names.contains(&word.to_lowercase())
            || self.autocorrect.undone.contains(&word.to_lowercase())
        {
            return None;
        }
        let engine = self.spelling.engine.as_mut()?;
        if !engine.check(&text).contains(&range) {
            return None;
        }
        let correction = engine
            .correction(&text, range.clone())
            .filter(|with| *with != word)?;
        Some(Pill {
            suggestions: Suggestions {
                kind: SuggestionKind::Single,
                word,
                items: vec![correction],
                learn: false,
                selected: None,
            },
            block: caret.block,
            range,
        })
    }

    /// Handles a key while the pill shows, and returns whether the pill took it. ↓ starts
    /// choosing, ←/→ go round the items, Enter or Tab takes the chosen one, and ↑ puts the pill
    /// away. Escape does too, unless the pill offers only `Learn`.
    pub(crate) fn pill_key(&mut self, key: Key) -> bool {
        let Some(pill) = &mut self.autocorrect.pill else {
            return false;
        };
        let count = pill.suggestions.items.len() + usize::from(pill.suggestions.learn);
        let selected = pill.suggestions.selected;
        match (key, selected) {
            (Key::ArrowDown, None) => pill.suggestions.selected = Some(0),
            (Key::ArrowDown, Some(_)) => {}
            (Key::ArrowUp, Some(_)) => self.dismiss_pill(),
            (Key::ArrowLeft, Some(at)) => {
                pill.suggestions.selected = Some((at + count - 1) % count)
            }
            (Key::ArrowRight, Some(at)) => pill.suggestions.selected = Some((at + 1) % count),
            (Key::Enter | Key::Tab, Some(at)) => self.pick_suggestion(at),
            (Key::Escape, _) if !pill.suggestions.items.is_empty() => self.dismiss_pill(),
            _ => return false,
        }
        true
    }

    /// Whether the pill takes Tab now (an item is chosen).
    pub(crate) fn pill_wants_tab(&self) -> bool {
        self.autocorrect
            .pill
            .as_ref()
            .is_some_and(|pill| pill.suggestions.selected.is_some())
    }

    /// Whether the pill takes Escape now.
    pub(crate) fn pill_wants_escape(&self) -> bool {
        self.autocorrect
            .pill
            .as_ref()
            .is_some_and(|pill| !pill.suggestions.items.is_empty())
    }

    /// Hovering item `index` with the mouse moving chooses it.
    pub(crate) fn hover_suggestion(&mut self, index: usize) {
        if let Some(pill) = &mut self.autocorrect.pill {
            pill.suggestions.selected = Some(index);
        }
    }

    /// Puts the pill away. A word being typed is marked as a correction turned down, and a marked
    /// word's pill doesn't come back until the caret leaves the word.
    fn dismiss_pill(&mut self) {
        let Some(pill) = self.autocorrect.pill.take() else {
            return;
        };
        if pill.suggestions.kind == SuggestionKind::Single {
            self.autocorrect
                .undone
                .insert(pill.suggestions.word.to_lowercase());
            self.annotate(
                pill.block,
                pill.range.clone(),
                Mark::Incorrect { undone: true },
            );
        }
        self.autocorrect.skip = Some((pill.block, pill.range));
    }

    /// Takes item `index` of the pill: the original word back, marked as misspelled; a correction,
    /// marked as one; or `Learn`, which adds the word to the user's dictionary and unmarks it
    /// everywhere.
    pub(crate) fn pick_suggestion(&mut self, index: usize) {
        let Some(pill) = self.autocorrect.pill.take() else {
            return;
        };
        self.autocorrect.looked = None;
        let Pill {
            suggestions,
            block,
            range,
        } = pill;
        if self.doc.blocks.get(block).is_none_or(|text| {
            text.text()
                .chars()
                .skip(range.start)
                .take(range.len())
                .ne(suggestions.word.chars())
        }) {
            return;
        }
        let Some(item) = suggestions.items.get(index) else {
            if suggestions.learn {
                self.sync_marks();
                // The word wherever it's marked or was corrected.
                self.autocorrect
                    .annotations
                    .retain(|annotation| match &annotation.mark {
                        Mark::Corrected { original, .. } => *original != suggestions.word,
                        Mark::Incorrect { .. } => annotation.word != suggestions.word,
                    });
                self.learn_spelling(&suggestions.word);
                self.autocorrect.learned = Some(suggestions.word);
            }
            return;
        };
        let word = self.replace_word(block, range, item);
        let after = Pos::new(block, word.end);
        self.selection = Selection::caret(after);
        match suggestions.kind {
            SuggestionKind::Undo => {
                self.autocorrect.undone.insert(item.to_lowercase());
                self.annotate(block, word, Mark::Incorrect { undone: true });
            }
            SuggestionKind::Multiple | SuggestionKind::Single => {
                let capital = is_capitalisation(&suggestions.word, item);
                self.annotate(
                    block,
                    word,
                    Mark::Corrected {
                        original: suggestions.word,
                        near: true,
                        capital,
                    },
                );
            }
        }
    }
}

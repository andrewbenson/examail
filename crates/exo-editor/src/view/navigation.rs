//! Caret movement by graphemes, words, visual lines and blocks.

use egui::{Key, Modifiers, Pos2, pos2};

use crate::{
    editor::Editor,
    grapheme,
    model::{Block, Pos, Selection},
};

/// A block's text in grapheme clusters, for moving by words.
pub(super) struct Words {
    chars: Vec<char>,
    pub(super) clusters: Vec<(usize, usize)>,
}

impl Words {
    pub(super) fn of(block: &Block) -> Self {
        let text = block.text();
        Self {
            clusters: grapheme::clusters(&text),
            chars: text.chars().collect(),
        }
    }

    /// Cluster `index`'s class, by its first character: 0 for a word's, 1 for whitespace, 2
    /// for anything else.
    pub(super) fn class(&self, index: usize) -> u8 {
        let ch = self.chars[self.clusters[index].0];
        if ch.is_alphanumeric() || ch == '_' || ch == '\'' {
            0
        } else if ch.is_whitespace() {
            1
        } else {
            2
        }
    }

    pub(super) fn is_word(&self, index: usize) -> bool {
        self.class(index) == 0
    }
}

impl Editor {
    /// The word around `pos` (double click): letters and digits, or a run of spaces, or one
    /// other character, in whole graphemes.
    pub(super) fn word_at(&self, pos: Pos) -> Selection {
        let words = Words::of(&self.doc.blocks[pos.block]);
        let Some(at) = words
            .clusters
            .iter()
            .position(|(_, end)| *end > pos.offset)
            .or(words.clusters.len().checked_sub(1))
        else {
            return Selection::caret(pos);
        };
        let kind = words.class(at);
        let same = |index: &usize| kind != 2 && words.class(*index) == kind;
        let first = (0..at).rev().take_while(same).last().unwrap_or(at);
        let last = (at + 1..words.clusters.len())
            .take_while(same)
            .last()
            .unwrap_or(at);
        Selection::new(
            Pos::new(pos.block, words.clusters[first].0),
            Pos::new(pos.block, words.clusters[last].1),
        )
    }

    pub(super) fn move_caret(&mut self, key: Key, modifiers: Modifiers) {
        let mac = modifiers.mac_cmd || modifiers.command;
        let (selection, caret) = (self.selection, self.selection.focus);
        let collapsing = !selection.is_collapsed() && !modifiers.shift;
        let target = match key {
            Key::ArrowLeft if collapsing && !modifiers.alt && !mac => selection.start(),
            Key::ArrowRight if collapsing && !modifiers.alt && !mac => selection.end(),
            Key::ArrowLeft if mac => self.line_start(caret),
            Key::ArrowRight if mac => self.line_end(caret),
            Key::ArrowLeft if modifiers.alt => self.word_start(caret),
            Key::ArrowRight if modifiers.alt => self.word_end(caret),
            Key::ArrowLeft => self.step(caret, false),
            Key::ArrowRight => self.step(caret, true),
            Key::Home => self.line_start(caret),
            Key::End => self.line_end(caret),
            Key::ArrowUp if mac => Pos::default(),
            Key::ArrowDown if mac => self.doc.end(),
            Key::ArrowUp if modifiers.alt => {
                if caret.offset > 0 {
                    Pos::new(caret.block, 0)
                } else {
                    Pos::new(caret.block.saturating_sub(1), 0)
                }
            }
            Key::ArrowDown if modifiers.alt => {
                let len = self.doc.blocks[caret.block].len();
                if caret.offset < len {
                    Pos::new(caret.block, len)
                } else {
                    self.doc.clamp(Pos::new(caret.block + 1, usize::MAX))
                }
            }
            Key::ArrowUp | Key::ArrowDown => self.vertical(
                if collapsing {
                    if key == Key::ArrowUp {
                        selection.start()
                    } else {
                        selection.end()
                    }
                } else {
                    caret
                },
                key == Key::ArrowDown,
            ),
            _ => caret,
        };
        let keep_x = self.view.preferred_x;
        let next = if modifiers.shift {
            Selection::new(selection.anchor, target)
        } else {
            Selection::caret(target)
        };
        self.set_selection(next);
        self.view.preferred_x = keep_x;
        self.view.blink_reset = true;
        self.view.scroll_to_caret = true;
    }

    /// One grapheme left or right, or across a block boundary.
    fn step(&self, pos: Pos, forward: bool) -> Pos {
        let block = &self.doc.blocks[pos.block];
        if forward {
            if pos.offset >= block.len() {
                return self.doc.clamp(Pos::new(pos.block + 1, 0)).max(pos);
            }
            Pos::new(pos.block, grapheme::next(&block.text(), pos.offset))
        } else {
            if pos.offset == 0 {
                return if pos.block == 0 {
                    pos
                } else {
                    Pos::new(pos.block - 1, self.doc.blocks[pos.block - 1].len())
                };
            }
            Pos::new(pos.block, grapheme::previous(&block.text(), pos.offset))
        }
    }

    /// ⌥←: the start of the word before `pos` (or the previous block's end at a block start).
    pub(super) fn word_start(&self, pos: Pos) -> Pos {
        if pos.offset == 0 {
            return self.step(pos, false);
        }
        let words = Words::of(&self.doc.blocks[pos.block]);
        let before = (0..words.clusters.len())
            .rev()
            .skip_while(|index| words.clusters[*index].1 > pos.offset);
        let start = before
            .skip_while(|index| !words.is_word(*index))
            .take_while(|index| words.is_word(*index))
            .last();
        Pos::new(pos.block, start.map_or(0, |index| words.clusters[index].0))
    }

    /// ⌥→: the end of the word after `pos`.
    pub(super) fn word_end(&self, pos: Pos) -> Pos {
        let block = &self.doc.blocks[pos.block];
        if pos.offset >= block.len() {
            return self.step(pos, true);
        }
        let words = Words::of(block);
        let after =
            (0..words.clusters.len()).skip_while(|index| words.clusters[*index].0 < pos.offset);
        let end = after
            .skip_while(|index| !words.is_word(*index))
            .take_while(|index| words.is_word(*index))
            .last();
        Pos::new(
            pos.block,
            end.map_or(block.len(), |index| words.clusters[index].1),
        )
    }

    /// The start of the visual line `pos` is on.
    pub(super) fn line_start(&self, pos: Pos) -> Pos {
        self.row_bounds(pos).0
    }

    /// The end of the visual line `pos` is on.
    fn line_end(&self, pos: Pos) -> Pos {
        self.row_bounds(pos).1
    }

    fn row_bounds(&self, pos: Pos) -> (Pos, Pos) {
        let Some(view) = self
            .view
            .layout
            .as_ref()
            .and_then(|layout| layout.blocks.get(pos.block))
        else {
            return (
                Pos::new(pos.block, 0),
                Pos::new(pos.block, self.doc.blocks[pos.block].len()),
            );
        };
        let index = view.galley_index(pos.offset);
        let mut start = 0;
        for row in &view.galley.rows {
            let end = start + row.char_count_excluding_newline().0;
            if index <= end {
                return (
                    Pos::new(pos.block, view.document_offset(start)),
                    Pos::new(pos.block, view.document_offset(end)),
                );
            }
            start += row.char_count_including_newline().0;
        }
        (
            Pos::new(pos.block, 0),
            Pos::new(pos.block, self.doc.blocks[pos.block].len()),
        )
    }

    /// The position a line up or down from `pos`, keeping the caret's x.
    fn vertical(&mut self, pos: Pos, down: bool) -> Pos {
        let origin = Pos2::ZERO;
        let here = self.char_rect(origin, pos);
        let x = *self.view.preferred_x.get_or_insert(here.min.x);
        let half = self.view.body.line() / 2.0;
        let y = if down {
            here.max.y + half
        } else {
            here.min.y - half
        };
        let Some(layout) = &self.view.layout else {
            return pos;
        };
        if y < 0.0 {
            return Pos::default();
        }
        if y > layout.height {
            return self.doc.end();
        }
        // Skip margins between blocks by landing in the nearest line box.
        let target_y = layout
            .blocks
            .iter()
            .map(|view| (view.top, view.top + view.height))
            .find(|(top, bottom)| y < *bottom && (down || y >= *top))
            .map_or(y, |(top, bottom)| y.clamp(top + 1.0, bottom - 1.0));
        let target_y = if !down {
            layout
                .blocks
                .iter()
                .rev()
                .map(|view| (view.top, view.top + view.height))
                .find(|(top, _)| y >= *top)
                .map_or(target_y, |(top, bottom)| y.clamp(top + 1.0, bottom - 1.0))
        } else {
            target_y
        };
        self.hit(origin, pos2(x, target_y))
    }
}

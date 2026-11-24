//! Dates in the body. Each date phrase in a line keeps to one line (`white-space: nowrap`) and has
//! a hairline under it. While the caret is on a date, `Previous Day` / `Next Day` show under it,
//! the calendar pane shows its day, and ⌘⇧– and ⌘⇧= move it a day.
//!
//! Exo finds dates in each block's text as it lays the block out, rather than keeping them in the
//! document the way autocorrect's marks are kept, so nothing reaches the saved or sent HTML. The
//! app works out what day a date means and what it says a day later ([`ActiveDate::text`]), and
//! [`Editor::shift_date`] writes it.

use std::ops::Range;

use crate::{
    editor::Editor,
    model::{Block, Content, Piece},
};

/// The detected date the caret is on ([`Editor::active_date`]).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ActiveDate {
    /// Its block.
    pub block: usize,
    /// Its characters in the block.
    pub range: Range<usize>,
    /// What it says (`tomorrow`, `next Friday`).
    pub text: String,
}

/// The dates in `block`, as character ranges. The search runs over the block's text nodes (runs of
/// text) joined with spaces, and a date counts only if it lies within one text node outside any
/// link.
pub(crate) fn detect(block: &Block) -> Vec<Range<usize>> {
    let Content::Inline(runs) = &block.content else {
        return Vec::new();
    };
    // Each text node: its first position in the block, where it starts in `joined`, its text,
    // and whether it sits inside a link.
    let mut nodes = Vec::new();
    let mut joined = String::new();
    let mut position = 0;
    for run in runs {
        if let Piece::Text(text) = &run.piece {
            if !nodes.is_empty() {
                joined.push(' ');
            }
            nodes.push((
                position,
                joined.len(),
                text.as_str(),
                run.marks.iter().any(|mark| mark.is("a")),
            ));
            joined.push_str(text);
        }
        position += run.piece.len();
    }
    exo_mail::find_dates(&joined)
        .into_iter()
        .filter_map(|date| {
            let &(first, start, text, in_link) = nodes
                .iter()
                .rev()
                .find(|(_, start, _, _)| *start <= date.start)?;
            if in_link || date.end > start + text.len() {
                return None;
            }
            let chars = |byte: usize| text[..byte - start].chars().count();
            Some(first + chars(date.start)..first + chars(date.end))
        })
        .collect()
}

impl Editor {
    /// The detected date the caret is on. The caret is on a date when it's collapsed in the date,
    /// or up to three characters past it in the text after it, unless a `:`, ` @` or ` ;` comes
    /// between. A caret at a date's start belongs to the text before it, which is where Chromium
    /// puts a caret between two text nodes, so a caret at the start of a date that follows text on
    /// its line isn't on the date.
    pub fn active_date(&self) -> Option<ActiveDate> {
        if self.settings.skip_dates || !self.selection.is_collapsed() {
            return None;
        }
        let caret = self.selection.focus;
        let block = self.doc.blocks.get(caret.block)?;
        let chars: Vec<char> = block.text().chars().collect();
        let range = detect(block).into_iter().find(|range| {
            let at_start = caret.offset == range.start
                && (range.start == 0
                    || chars[range.start - 1] == '\n'
                    || chars[range.start - 1] == '\u{FFFC}');
            if at_start || (range.start < caret.offset && caret.offset <= range.end) {
                return true;
            }
            // The text after the date must be in the same text node, so one run of text holds the
            // date's last character and everything up to the caret.
            let after = range.end..caret.offset;
            let Some(between) = chars
                .get(after.clone())
                .map(|chars| chars.iter().collect::<String>())
            else {
                return false;
            };
            (1..4).contains(&after.len())
                && same_run(block, range.end - 1, caret.offset)
                && !between.contains(':')
                && !space_then(&between, '@')
                && !space_then(&between, ';')
        })?;
        Some(ActiveDate {
            block: caret.block,
            text: chars[range.clone()].iter().collect(),
            range,
        })
    }

    /// Rewrites the date the caret is on to say `text`, leaving the caret at its end. It isn't an
    /// undo step of its own, because it counts as typing.
    pub fn shift_date(&mut self, date: &ActiveDate, text: &str) {
        if self.active_date().as_ref() != Some(date) {
            return;
        }
        let block = &mut self.doc.blocks[date.block];
        let marks = block.marks_at(date.range.start + 1);
        block.remove(date.range.clone());
        block.insert(date.range.start, vec![crate::model::Run::text(text, marks)]);
        self.selection = crate::model::Selection::caret(crate::model::Pos::new(
            date.block,
            date.range.start + text.chars().count(),
        ));
        self.changed();
    }
}

/// Whether positions `from..to` of `block` are in one run of text.
fn same_run(block: &Block, from: usize, to: usize) -> bool {
    let mut start = 0;
    block.runs().iter().any(|run| {
        let end = start + run.piece.len();
        let inside = matches!(run.piece, Piece::Text(_)) && start <= from && to <= end;
        start = end;
        inside
    })
}

/// Whether `text` has `ch` right after whitespace.
fn space_then(text: &str, ch: char) -> bool {
    let chars: Vec<char> = text.chars().collect();
    chars
        .windows(2)
        .any(|pair| pair[0].is_whitespace() && pair[1] == ch)
}

//! Dragging the selection within the body, as Chromium's `MoveSelectionCommand` moves it, after
//! an undo step. As on a Mac, a move deletes smartly (a space beside the text goes with it)
//! and a selection made by words inserts smartly (spaces put between it and the words it lands
//! next to). ⌥ copies instead.
//!
//! Text dropped from outside the editor (another app, another editor) goes in as a paste of it
//! would, and ends up selected. A selection moved out to a field or another editor in the window
//! leaves this editor as a move's would.

use super::Editor;
use crate::model::{Document, Pos, Selection};

impl Editor {
    /// Drops the selection at `at` (in the document as it is now) in one undo step, moving it
    /// there, or copying it when `copy`. The dropped text is selected after
    /// (`kSelectReplacement`). `smart` is whether the selection was made by words. A drop on the
    /// selection itself does nothing.
    pub(crate) fn drop_selection(&mut self, at: Pos, copy: bool, smart: bool) {
        let (start, end) = (self.selection.start(), self.selection.end());
        if self.selection.is_collapsed() || (start <= at && at <= end) {
            return;
        }
        let mut fragment = self.doc.fragment(start, end);
        // Part of one line goes as text, out of the line's containers.
        if let [block] = fragment.as_mut_slice() {
            block.path.clear();
        }
        let mut fragment = Document::new(fragment);
        super::fix_cursors(&mut fragment);
        self.undo.save(&self.doc, self.selection);
        let (trimmed_start, trimmed_end) = self.doc.trim_atoms(start, end);
        let (from, to) = if copy {
            (trimmed_start, trimmed_end)
        } else {
            let (from, to) = self.smart_delete_range(trimmed_start, trimmed_end);
            self.doc.trim_atoms(from, to)
        };
        let (dropped, selected_end) = if copy {
            self.place_dropped(at, fragment, smart).0
        } else if at < from {
            let (selected, end, added, tail_merged) = self.place_dropped(at, fragment, smart);
            let follow = |pos: Pos| follow_insertion(pos, at, end, added, tail_merged);
            self.doc.delete(follow(from), follow(to));
            selected
        } else {
            let before = self.doc.blocks.len();
            self.doc.delete(from, to);
            let at = follow_deletion(at, from, to, before, self.doc.blocks.len());
            self.place_dropped(at, fragment, smart).0
        };
        self.selection = Selection::new(dropped, selected_end);
        self.changed();
    }

    /// Puts text dropped from outside the editor in at `at`, in one undo step, and selects it. The
    /// `html` goes in cleaned as a paste's is, or else the plain `text` goes in as plain text is
    /// pasted. The source's styles don't come along.
    pub(crate) fn drop_text(&mut self, at: Pos, html: Option<&str>, text: &str) {
        self.collapse_to(self.doc.clamp(at));
        let start = self.caret();
        match html {
            Some(markup) => {
                let fragment = self.pasted_html(markup);
                self.insert_fragment(fragment, false);
            }
            None => self.insert_fragment(super::plain_fragment(text), true),
        }
        self.selection = Selection::new(start, self.caret());
        self.changed();
    }

    /// Takes the selection out, as a move out of the editor does (`DeleteSelectionAfterDragging`,
    /// a smart delete), in one undo step.
    pub(crate) fn delete_dragged(&mut self) {
        let (start, end) = (self.selection.start(), self.selection.end());
        if self.selection.is_collapsed() {
            return;
        }
        let (trimmed_start, trimmed_end) = self.doc.trim_atoms(start, end);
        let (from, to) = self.smart_delete_range(trimmed_start, trimmed_end);
        let (from, to) = self.doc.trim_atoms(from, to);
        self.undo.save(&self.doc, self.selection);
        let pos = self.doc.delete(from, to);
        self.collapse_to(pos);
        self.changed();
    }

    /// `start..end` with the space Chromium's smart delete takes with it (`DeleteSelectionCommand`
    /// with `smart_delete_`): none if the range starts or ends with whitespace itself, else the
    /// space before the range, else the one after.
    fn smart_delete_range(&self, start: Pos, end: Pos) -> (Pos, Pos) {
        let char_at = |pos: Pos| self.doc.blocks[pos.block].text().chars().nth(pos.offset);
        let space = |ch: Option<char>| ch.is_some_and(|ch| ch == ' ' || ch == '\u{A0}');
        let first = char_at(start);
        let last = end
            .offset
            .checked_sub(1)
            .and_then(|offset| char_at(Pos::new(end.block, offset)));
        if space(first) || space(last) {
            return (start, end);
        }
        if let Some(before) = start.offset.checked_sub(1)
            && char_at(Pos::new(start.block, before)) == Some(' ')
        {
            return (Pos::new(start.block, before), end);
        }
        if space(char_at(end)) {
            return (start, Pos::new(end.block, end.offset + 1));
        }
        (start, end)
    }

    /// Puts `fragment` at `at`, with smart insertion's spaces when `smart`
    /// (`ReplaceSelectionCommand::kSmartReplace`). Returns where the text starts and ends (not
    /// counting its spaces), the position after everything it put in, how many blocks were added
    /// before blocks following `at`, and whether `at`'s split tail merged into the ending block.
    fn place_dropped(
        &mut self,
        at: Pos,
        mut fragment: Document,
        smart: bool,
    ) -> ((Pos, Pos), Pos, usize, bool) {
        let mut dropped = at;
        let mut trailing = false;
        if smart
            && self.doc.blocks[at.block].is_inline()
            && let (Some(first), Some(last)) = (fragment.blocks.first(), fragment.blocks.last())
        {
            let text = self.doc.blocks[at.block].text();
            let before = at
                .offset
                .checked_sub(1)
                .and_then(|offset| text.chars().nth(offset));
            let after = text.chars().nth(at.offset);
            let leading = first.is_inline()
                && before.is_some_and(|ch| !smart_exempt(ch, true))
                && first
                    .text()
                    .chars()
                    .next()
                    .is_some_and(|ch| !smart_exempt(ch, false));
            trailing = last.is_inline()
                && after.is_some_and(|ch| !smart_exempt(ch, false))
                && last
                    .text()
                    .chars()
                    .last()
                    .is_some_and(|ch| !smart_exempt(ch, true));
            if leading {
                self.doc
                    .insert_text(at, " ", self.doc.blocks[at.block].marks_at(at.offset));
                dropped.offset += 1;
            }
            if trailing && let Some(last) = fragment.blocks.last_mut() {
                let marks = last.marks_at(last.len());
                last.insert(last.len(), vec![crate::model::Run::text(" ", marks)]);
            }
        }
        self.collapse_to(dropped);
        let (added, tail_merged) = self.place_fragment(fragment);
        // The space put after the dropped text isn't part of it.
        let end = self.caret();
        (
            (
                dropped,
                if trailing {
                    Pos::new(end.block, end.offset - 1)
                } else {
                    end
                },
            ),
            end,
            added,
            tail_merged,
        )
    }
}

/// Where `pos`, which isn't in `start..end`, ends up once that range is deleted. The document had
/// `blocks` blocks before the deletion and has `after` now.
fn follow_deletion(pos: Pos, start: Pos, end: Pos, blocks: usize, after: usize) -> Pos {
    if pos <= start {
        pos
    } else if pos.block == end.block {
        Pos::new(start.block, start.offset + (pos.offset - end.offset))
    } else {
        Pos::new(pos.block + after - blocks, pos.offset)
    }
}

/// Where `pos`, which is after `at`, ends up once a fragment put in at `at` ends at `end`.
/// `added` is how many blocks were added before the blocks after `at`, and `tail_merged` is
/// whether the remainder of `at`'s block merged onto `end`'s block rather than staying on its own
/// block after `end`.
fn follow_insertion(pos: Pos, at: Pos, end: Pos, added: usize, tail_merged: bool) -> Pos {
    if pos.block == at.block {
        if tail_merged {
            Pos::new(end.block, end.offset + (pos.offset - at.offset))
        } else {
            Pos::new(at.block + added, pos.offset - at.offset)
        }
    } else {
        Pos::new(pos.block + added, pos.offset)
    }
}

/// Characters smart insertion puts no space beside (`IsCharacterSmartReplaceExemptConservative`):
/// whitespace; after the text, any punctuation; and these before or after it.
fn smart_exempt(ch: char, previous: bool) -> bool {
    if ch.is_whitespace() || (!previous && ch.is_ascii_punctuation()) {
        return true;
    }
    let set = if previous {
        "([\"'#$/-`{"
    } else {
        ")].,;:?'!\"%*-/}"
    };
    set.contains(ch)
}

#[cfg(test)]
mod tests {
    use crate::{Editor, Pos, Selection};

    #[test]
    fn dragging_text_adjacent_to_a_rule_or_table_keeps_the_atom() {
        let mut editor = Editor::from_html("<div><hr></div><div>alpha beta</div>");
        // Selection starts right after the `<hr>` atom and covers `alpha`.
        editor.set_selection(Selection::new(Pos::new(0, 1), Pos::new(1, 5)));
        editor.drop_selection(Pos::new(1, 10), false, true);
        assert_eq!(editor.html(), "<div><hr></div><div>beta alpha<br></div>");

        let mut table_editor = Editor::from_html(
            "<table><tbody><tr><td>cell</td></tr></tbody></table><div>alpha beta</div>",
        );
        table_editor.set_selection(Selection::new(Pos::new(0, 1), Pos::new(1, 5)));
        table_editor.drop_selection(Pos::new(1, 10), false, true);
        assert_eq!(
            table_editor.html(),
            "<table><tbody><tr><td>cell</td></tr></tbody></table><div>beta alpha<br></div>"
        );
    }

    #[test]
    fn dragging_multi_block_quote_or_list_upward_deletes_the_source_blocks() {
        // Multi-block quote dragged into the middle of a preceding line (split tail does not merge).
        let mut quote = Editor::from_html(
            "<div>hello world</div><blockquote><div>q1</div><div>q2</div></blockquote><div>tail</div>",
        );
        quote.set_selection(Selection::new(Pos::new(1, 0), Pos::new(2, 2)));
        quote.drop_selection(Pos::new(0, 5), false, false);
        let texts: Vec<String> = quote
            .document()
            .blocks
            .iter()
            .map(|block| block.text())
            .collect();
        assert_eq!(texts, ["hello", "q1", "q2", " world", "", "tail"]);
        assert_eq!(
            quote
                .document()
                .blocks
                .iter()
                .filter(|block| block.text() == "q1" || block.text() == "q2")
                .count(),
            2
        );

        // Multi-block list at document end (where `ensure_bottom_line` appends a line) dragged to
        // the start of block 0.
        let mut list = Editor::from_html("<div>intro</div><ul><li>one</li><li>two</li></ul>");
        list.set_selection(Selection::new(Pos::new(1, 0), Pos::new(2, 3)));
        list.drop_selection(Pos::new(0, 0), false, false);
        let html = list.html();
        assert!(
            html.starts_with("<ul><li>one<br></li><li>two<br></li></ul><div>intro<br></div>"),
            "unexpected html after upward list drag: {html}"
        );
        assert_eq!(html.matches("one").count(), 1, "{html}");
        assert_eq!(html.matches("two").count(), 1, "{html}");

        // Selection starting later on the SAME block (`pos.block == at.block`) and ending in a
        // blockquote (so `at`'s split tail does not merge into `end.block`) dragged to `(0, 0)`.
        let mut same_block = Editor::from_html(
            "<div>hello alpha</div><blockquote><div>q1</div></blockquote><div>end</div>",
        );
        same_block.set_selection(Selection::new(Pos::new(0, 6), Pos::new(1, 2)));
        same_block.drop_selection(Pos::new(0, 0), false, false);
        let texts: Vec<String> = same_block
            .document()
            .blocks
            .iter()
            .map(|block| block.text())
            .collect();
        assert_eq!(texts, ["alpha", "q1", "hello", "end"]);
    }

    #[test]
    fn smart_drop_starting_or_ending_in_rule_or_table_preserves_the_atom() {
        // Selection ending in `<hr>` dropped smartly into the middle of `"foobar"`.
        let mut rule = Editor::from_html("<div>foobar</div><div>alpha</div><div><hr></div>");
        rule.set_selection(Selection::new(Pos::new(1, 0), Pos::new(2, 1)));
        rule.drop_selection(Pos::new(0, 3), true, true);
        assert!(
            rule.html().contains("<hr>"),
            "smart drop must not overwrite `<hr>` with a space: {}",
            rule.html()
        );
        assert_eq!(
            rule.html().matches("<hr>").count(),
            2,
            "copying `<hr>` keeps both copies intact: {}",
            rule.html()
        );

        // Selection ending in `<table>` dropped smartly into the middle of `"foobar"`.
        let mut table = Editor::from_html(
            "<div>foobar</div><div>alpha</div><table><tbody><tr><td>cell</td></tr></tbody></table>",
        );
        table.set_selection(Selection::new(Pos::new(1, 0), Pos::new(2, 1)));
        table.drop_selection(Pos::new(0, 3), true, true);
        assert_eq!(
            table.html().matches("<table").count(),
            2,
            "smart drop must keep the table intact: {}",
            table.html()
        );

        // Selection starting in `<hr>` dropped smartly into the middle of `"foobar"`.
        let mut lead_rule = Editor::from_html("<div>foobar</div><div><hr></div><div>alpha</div>");
        lead_rule.set_selection(Selection::new(Pos::new(1, 0), Pos::new(2, 5)));
        lead_rule.drop_selection(Pos::new(0, 3), true, true);
        assert_eq!(
            lead_rule.html().matches("<hr>").count(),
            2,
            "smart drop starting in `<hr>` keeps both copies intact: {}",
            lead_rule.html()
        );
    }
}

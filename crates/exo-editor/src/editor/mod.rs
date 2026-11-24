//! The editor's state and its commands. Everything here works without a UI, and the widget
//! ([`crate::view`]) turns input into these calls.

mod drop;
mod formatting;
mod images;
mod paste;
mod quote;
mod triggers;

use std::collections::HashSet;

pub use formatting::{Color, Format, LinkAction};
pub use images::{Fit, Upload};
pub(crate) use paste::{fix_cursors, link_run, plain_fragment, starts_with_ignore_ascii_case};
pub use triggers::unicode_replacement;

use crate::{
    grapheme, html,
    model::{Block, Document, Element, Piece, Pos, Run, Selection},
    trigger::{self, Trigger},
    undo::{UndoMark, UndoStack},
};

/// Options that change what typing does, and where the editor is.
#[derive(Clone, Copy, Debug, Default)]
pub struct Settings {
    /// With `convertHyphenToBulletList`, `-` and Space at the start of a line start a bulleted
    /// list.
    pub hyphen_starts_list: bool,
    /// Dates aren't detected (a snippet's body, an event's About).
    pub skip_dates: bool,
    /// The draft is popped out, so its date tooltip keeps 7.5px from the window's left rather than
    /// 22.5px.
    pub popped_out: bool,
    /// Another draft is popped out, so this one's date tooltip keeps 7.5px from the window's right
    /// rather than 22.5px.
    pub another_popped_out: bool,
}

/// The editor: a document, a selection, its undo stack, and the editing commands.
pub struct Editor {
    pub(crate) doc: Document,
    pub(crate) selection: Selection,
    pub(crate) undo: UndoStack,
    /// Marks the next typed text takes instead of its neighbour's (⌘B on a caret).
    pub(in crate::editor) pending: Option<Vec<Element>>,
    /// The trigger text of a list just made from markdown (`* `), restored by Backspace.
    pub(in crate::editor) auto_list: Option<String>,
    /// Where the caret was after the last Unicode replacement. Backspace there undoes it.
    pub(in crate::editor) replacement: Option<Pos>,
    /// The inline trigger typed into, if it's still open.
    pub(crate) trigger: Option<Trigger>,
    pub(in crate::editor) trigger_dismissed: bool,
    /// Whether a trigger character was typed with nothing picked since, so a `+` opens its
    /// contacts once a name follows it.
    pub(in crate::editor) trigger_armed: bool,
    /// Ghost text shown after the caret (a prediction), accepted with Tab or →.
    pub(crate) ghost: Option<String>,
    pub(crate) settings: Settings,
    /// The image the resizer is on ([`Self::focused_image`]).
    pub(in crate::editor) image_focus: Option<Pos>,
    /// Content-IDs of images put in that get Best Fit's width once their pixels come.
    pub(in crate::editor) fit_on_load: HashSet<String>,
    /// `data:` images pasted, for the app to keep ([`Self::take_uploads`]).
    pub(in crate::editor) uploads: Vec<Upload>,
    /// Bumped on every change, so views know to lay out again.
    pub(crate) revision: u64,
    pub(crate) view: crate::view::ViewState,
    pub(crate) spelling: crate::spelling::State,
    pub(crate) autocorrect: crate::autocorrect::State,
}

impl Default for Editor {
    fn default() -> Self {
        Self::new()
    }
}

impl Editor {
    /// An empty body of one line. Its `div` carries the empty `class` outgoing HTML has on a
    /// draft's first line.
    pub fn new() -> Self {
        let mut first = Block::empty(Vec::new());
        first.element = Element::with("div", &[("class", "")]);
        Self::with_document(Document::new(vec![first]))
    }

    /// An editor on `document`, the caret at its start.
    fn with_document(document: Document) -> Self {
        let mut editor = Self {
            doc: document,
            selection: Selection::default(),
            undo: UndoStack::default(),
            pending: None,
            auto_list: None,
            replacement: None,
            trigger: None,
            trigger_dismissed: false,
            trigger_armed: false,
            ghost: None,
            settings: Settings::default(),
            image_focus: None,
            fit_on_load: HashSet::new(),
            uploads: Vec::new(),
            revision: 0,
            view: crate::view::ViewState::default(),
            spelling: crate::spelling::State::default(),
            autocorrect: crate::autocorrect::State::default(),
        };
        editor.undo.reset(&editor.doc, editor.selection);
        editor
    }

    /// An editor on a draft body.
    pub fn from_html(body: &str) -> Self {
        Self::with_document(load(body))
    }

    /// Replaces the body and clears undo.
    pub fn set_html(&mut self, body: &str) {
        self.replace_document(load(body));
    }

    /// Replaces the body with `body` in one undo step. The caret keeps its line counted from the
    /// end, as an edit at the top of the body leaves it.
    pub fn modify_html(&mut self, body: &str) {
        self.undo.save(&self.doc, self.selection);
        let from_end = self.doc.blocks.len() - 1 - self.caret().block;
        let offset = self.caret().offset;
        self.doc = load(body);
        let block = (self.doc.blocks.len() - 1).saturating_sub(from_end);
        self.collapse_to(self.snap(Pos::new(block, offset)));
        self.trigger = None;
        self.changed();
    }

    pub(in crate::editor) fn replace_document(&mut self, doc: Document) {
        self.doc = doc;
        self.selection = Selection::default();
        self.undo.reset(&self.doc, self.selection);
        self.view.toolbar.reset();
        self.touch();
    }

    /// The body as HTML, which is the draft body that's saved.
    pub fn html(&self) -> String {
        html::write(&self.doc)
    }

    /// The document.
    pub fn document(&self) -> &Document {
        &self.doc
    }

    /// The selection.
    pub fn selection(&self) -> Selection {
        self.selection
    }

    /// Moves the selection (clamped to the document).
    pub fn set_selection(&mut self, selection: Selection) {
        let selection = Selection::new(self.snap(selection.anchor), self.snap(selection.focus));
        if selection != self.selection {
            self.selection = selection;
            self.image_focus = None;
            self.pending = None;
            self.replacement = None;
            self.ghost = None;
            self.spelling.typing = false;
            self.update_trigger(false);
            self.view.caret_moved();
        }
    }

    /// `pos` inside the document, and not inside a grapheme.
    pub(in crate::editor) fn snap(&self, pos: Pos) -> Pos {
        let pos = self.doc.clamp(pos);
        Pos::new(
            pos.block,
            grapheme::snap(&self.doc.blocks[pos.block].text(), pos.offset),
        )
    }

    /// Selects everything (⌘A).
    pub fn select_all(&mut self) {
        self.set_selection(Selection::new(Pos::default(), self.doc.end()));
    }

    /// Sets the typing options.
    pub fn set_settings(&mut self, settings: Settings) {
        if settings.skip_dates != self.settings.skip_dates {
            // Dates change how lines wrap.
            self.view.forget_layout();
        }
        self.settings = settings;
    }

    /// Whether the body is visually empty (shows the placeholder).
    pub fn is_empty(&self) -> bool {
        self.doc.is_empty()
    }

    /// The selected text, blocks separated by newlines.
    pub fn selected_text(&self) -> String {
        self.doc.text(self.selection.start(), self.selection.end())
    }

    pub(in crate::editor) fn touch(&mut self) {
        self.revision += 1;
        self.spelling.typing = false;
        self.view.caret_moved();
    }

    /// The document changed. Lines then get the `<br>`s that keep the sent mail showing them as the
    /// editor does ([`Document::fix_lines`]).
    pub(crate) fn changed(&mut self) {
        self.doc.fix_lines();
        self.undo.changed();
        self.view.toolbar.reset();
        self.ghost = None;
        self.touch();
    }

    pub(in crate::editor) fn caret(&self) -> Pos {
        self.selection.focus
    }

    pub(in crate::editor) fn collapse_to(&mut self, pos: Pos) {
        self.selection = Selection::caret(pos);
    }

    /// The caret, where text can go. On an atom (a rule, a table), which can't hold text, a new
    /// line after the atom (or before it, from its start) takes the caret.
    pub(in crate::editor) fn caret_for_text(&mut self) -> Pos {
        let caret = self.caret();
        let block = &self.doc.blocks[caret.block];
        if block.is_inline() {
            return caret;
        }
        let at = Pos::new(caret.block + usize::from(caret.offset > 0), 0);
        self.doc
            .blocks
            .insert(at.block, Block::empty(block.path.clone()));
        self.collapse_to(at);
        at
    }

    /// Deletes the selection if there is one, and returns where the caret is.
    pub(in crate::editor) fn delete_selection(&mut self) -> Pos {
        let (start, end) = (self.selection.start(), self.selection.end());
        if start == end {
            return start;
        }
        let pos = self.doc.delete(start, end);
        self.doc.ensure_bottom_line();
        self.collapse_to(pos);
        pos
    }

    /// The marks text typed at the caret gets.
    pub(in crate::editor) fn typing_marks(&self) -> Vec<Element> {
        if let Some(pending) = &self.pending {
            return pending.clone();
        }
        let caret = self.caret();
        self.doc.blocks[caret.block].marks_at(caret.offset)
    }

    // Typing

    /// Types `text` at the caret, as keystrokes: newlines are Enter, spaces (and the whitespace a
    /// browser shows as one) Space, everything else a character. A paste of plain text goes
    /// through [`Self::paste_text`].
    pub fn type_text(&mut self, text: &str) {
        for ch in text.chars() {
            match ch {
                '\n' => self.enter(false),
                ch if is_collapsible(ch) => self.space(),
                ch if is_invisible(ch) => {}
                ch => self.type_char(ch),
            }
        }
    }

    fn type_char(&mut self, ch: char) {
        if !self.selection.is_collapsed() {
            self.undo.save(&self.doc, self.selection);
            self.delete_selection();
        }
        let marks = self.typing_marks();
        self.insert_at_caret(&ch.to_string(), marks);
        self.replace_unicode(ch);
        self.update_trigger(trigger::is_trigger_char(ch));
        self.spelling.typing = true;
        self.typed_for_assistance(ch);
    }

    pub(in crate::editor) fn insert_at_caret(&mut self, text: &str, marks: Vec<Element>) {
        let caret = self.caret_for_text();
        let end = self.doc.insert_text(caret, text, marks.clone());
        self.pending = self.pending.take().map(|_| marks);
        self.collapse_to(end);
        self.changed();
    }

    /// Space, with the markdown list and quote triggers.
    pub fn space(&mut self) {
        if self.selection.is_collapsed() && self.markdown_trigger() {
            return;
        }
        self.auto_list = None;
        self.undo.record(&self.doc, self.selection, false);
        self.autolink();
        if !self.selection.is_collapsed() {
            self.delete_selection();
        }
        let caret = self.caret();
        let mut marks = self.typing_marks();
        // At the end of a link, the space goes after it.
        let block = &self.doc.blocks[caret.block];
        let next = block
            .slice(caret.offset..caret.offset + 1)
            .pop()
            .map(|run| run.marks)
            .unwrap_or_default();
        if self.pending.is_none() {
            marks.retain(|mark| !mark.is("a") || next.contains(mark));
        }
        self.insert_at_caret(" ", marks);
        self.finish_word(caret);
        self.update_trigger(false);
        self.complete();
    }

    /// Enter, or Shift+Enter, which makes a new line. Enter also finishes the word before it for
    /// autocorrect.
    pub fn enter(&mut self, shift: bool) {
        let before = self.selection.start();
        self.break_line(shift);
        if !shift && self.caret().block > before.block {
            self.finish_word(before);
        }
    }

    /// Enter, or Shift+Enter with `shift`: a rule from dashes, a code block from a fence, the
    /// markdown lists, and links typed before it.
    fn break_line(&mut self, shift: bool) {
        self.auto_list = None;
        let caret_block = &self.doc.blocks[self.selection.start().block];
        if shift && caret_block.list().is_some() {
            // A soft break inside a list item.
            self.undo.record(&self.doc, self.selection, false);
            self.delete_selection();
            let caret = self.caret_for_text();
            let marks = self.typing_marks();
            self.doc.blocks[caret.block].insert(
                caret.offset,
                vec![Run {
                    piece: Piece::Break,
                    marks,
                }],
            );
            self.collapse_to(Pos::new(caret.block, caret.offset + 1));
            return self.changed();
        }
        self.undo.record(&self.doc, self.selection, false);
        if self.rule_from_dashes() || self.code_from_fence() {
            return;
        }
        self.autolink();
        let pending = self.pending.clone();
        let caret = self.delete_selection();
        let block = &self.doc.blocks[caret.block];
        if !block.is_inline() {
            // Enter on an atom opens a line beside it.
            self.caret_for_text();
            return self.changed();
        }
        if block.in_code() {
            return self.code_enter(caret, shift);
        }
        if block.in_blockquote() {
            let pos = self.doc.break_out_of_quote(caret);
            self.collapse_to(pos);
            return self.changed();
        }
        if block.element.tag.is_empty() {
            // In loose inline content (no block element), the browser inserts a `<br>`.
            let marks = block.marks_at(caret.offset);
            self.doc.blocks[caret.block].insert(
                caret.offset,
                vec![Run {
                    piece: Piece::Break,
                    marks,
                }],
            );
            self.collapse_to(Pos::new(caret.block, caret.offset + 1));
            return self.changed();
        }
        if block.is_empty() && block.list().is_some() {
            // An empty item leaves its list (outdents). First the body gets an editable last line
            // if it has none, so that line stays below the item.
            self.doc.ensure_bottom_line();
            self.doc
                .modify_blocks(caret.block..caret.block + 1, Document::decrease_list_level);
            self.collapse_to(Pos::new(caret.block, 0));
            self.doc.renumber(caret.block);
            return self.changed();
        }
        let marks_before = self.doc.blocks[caret.block].marks_at(caret.offset);
        let pos = self.doc.split(caret);
        self.collapse_to(pos);
        self.doc.renumber(pos.block);
        // The new line keeps typing in the formatting the caret was in.
        self.pending = pending.or_else(|| {
            (!marks_before.is_empty() && self.doc.blocks[pos.block].is_empty())
                .then_some(marks_before)
        });
        self.changed();
    }

    /// Enter in a code block. It's a line break (`\n`), or, on an empty line without Shift, a move
    /// out of the block onto a plain line.
    fn code_enter(&mut self, caret: Pos, shift: bool) {
        let block = &self.doc.blocks[caret.block];
        let is_break = |offset: usize| {
            block
                .slice(offset..offset + 1)
                .first()
                .is_some_and(|run| run.piece == Piece::Break)
        };
        let after_break = caret.offset > 0 && is_break(caret.offset - 1);
        let before_break = is_break(caret.offset);
        if !shift
            && (caret.offset == 0 || after_break)
            && (caret.offset == block.len() || before_break)
        {
            let line =
                caret.offset - usize::from(after_break)..caret.offset + usize::from(before_break);
            let pos = self.doc.leave_code(caret.block, line);
            self.collapse_to(pos);
            return self.changed();
        }
        let marks = self.typing_marks();
        let block = &mut self.doc.blocks[caret.block];
        block.insert(
            caret.offset,
            vec![Run {
                piece: Piece::Break,
                marks,
            }],
        );
        block.fix_placeholder();
        self.collapse_to(Pos::new(caret.block, caret.offset + 1));
        self.changed();
    }

    /// Backspace, with list outdenting and the Unicode-replacement undo.
    pub fn backspace(&mut self) {
        if self
            .replacement
            .take()
            .is_some_and(|at| self.selection.is_collapsed() && at == self.caret())
        {
            self.undo();
            return;
        }
        let auto_list = self.auto_list.take();
        self.undo.save(&self.doc, self.selection);
        if !self.selection.is_collapsed() {
            self.delete_selection();
            return self.changed();
        }
        let caret = self.caret();
        if caret.offset > 0 {
            let block = &self.doc.blocks[caret.block];
            let start = grapheme::backspace(&block.text(), caret.offset);
            let removed = block.slice(start..caret.offset);
            self.doc.delete(Pos::new(caret.block, start), caret);
            // Deleting a one-character format keeps it alive for the next keystroke.
            if let [run] = removed.as_slice()
                && !run.marks.is_empty()
                && self.doc.blocks[caret.block].marks_at(start) != run.marks
            {
                self.pending = Some(run.marks.clone());
            }
            self.collapse_to(Pos::new(caret.block, start));
            self.changed();
            self.spelling.typing = true;
            return self.update_trigger(false);
        }
        let block = &self.doc.blocks[caret.block];
        // At the start of a list's first item, outdent (restoring a markdown trigger just used).
        if block.list().is_some() && !self.is_later_item(caret.block) {
            let in_ordered =
                block.path.len() >= 2 && block.path[block.path.len() - 2].element.is("ol");
            self.doc
                .modify_blocks(caret.block..caret.block + 1, Document::decrease_list_level);
            if let Some(trigger) = auto_list.filter(|_| !in_ordered) {
                self.doc
                    .insert_text(Pos::new(caret.block, 0), &trigger, Vec::new());
            }
            self.collapse_to(Pos::new(caret.block, 0));
            return self.changed();
        }
        if caret.block > 0 {
            // A rule before the line goes with it.
            let pos = self.doc.merge_with_previous(caret.block);
            self.collapse_to(pos);
            return self.changed();
        }
        // At the very start, leave the list or quote.
        if block.list().is_some() {
            self.doc.modify_blocks(0..1, Document::decrease_list_level);
        } else if block.in_blockquote() {
            self.doc
                .modify_blocks(0..1, Document::unwrap_outer_blockquote);
        } else {
            return;
        }
        self.collapse_to(Pos::new(0, 0));
        self.changed();
    }

    /// Whether block `index` is a list item with an earlier item in the same list.
    fn is_later_item(&self, index: usize) -> bool {
        let Some((depth, list)) = self.doc.blocks[index].list() else {
            return false;
        };
        index > 0
            && self.doc.blocks[index - 1]
                .path
                .get(depth)
                .is_some_and(|previous| previous.id == list.id)
    }

    /// Forward delete.
    pub fn delete_forward(&mut self) {
        self.undo.save(&self.doc, self.selection);
        if !self.selection.is_collapsed() {
            self.delete_selection();
            return self.changed();
        }
        let caret = self.caret();
        let block = &self.doc.blocks[caret.block];
        if caret.offset < block.len() {
            let end = grapheme::next(&block.text(), caret.offset);
            self.doc.delete(caret, Pos::new(caret.block, end));
            self.changed();
            self.spelling.typing = true;
            return;
        }
        if caret.block + 1 < self.doc.blocks.len() {
            let pos = self.doc.merge_with_previous(caret.block + 1);
            self.collapse_to(pos);
            self.changed();
        }
    }

    /// Deletes the selection after it was copied (⌘X), in one undo step.
    pub(crate) fn cut(&mut self) {
        self.undo.save(&self.doc, self.selection);
        self.delete_selection();
        self.changed();
    }

    /// Deletes from `start` to the caret (⌥⌫ to a word start, ⌘⌫ to a line start), in one undo
    /// step.
    pub fn delete_back_to(&mut self, start: Pos) {
        self.undo.save(&self.doc, self.selection);
        let pos = self.doc.delete(start.min(self.caret()), self.caret());
        self.collapse_to(pos);
        self.changed();
    }

    /// Deletes from the caret to `end` (⌥⌦), in one undo step.
    pub fn delete_forward_to(&mut self, end: Pos) {
        self.undo.save(&self.doc, self.selection);
        let pos = self.doc.delete(self.caret(), end.max(self.caret()));
        self.collapse_to(pos);
        self.changed();
    }

    // Undo

    /// Undo (⌘Z). Returns the marks of the change undone ([`Self::mark_undo`]), so the caller
    /// can revert what it tied to it.
    pub fn undo(&mut self) -> Vec<UndoMark> {
        self.view.toolbar.reset();
        match self.undo.undo(&self.doc, self.selection) {
            Some((doc, selection, marks)) => {
                self.restore(doc, selection);
                marks
            }
            None => Vec::new(),
        }
    }

    /// Redo (⌘⇧Z, ⌘Y). Returns the marks of the change redone.
    pub fn redo(&mut self) -> Vec<UndoMark> {
        self.view.toolbar.reset();
        match self.undo.redo() {
            Some((doc, selection, marks)) => {
                self.restore(doc, selection);
                marks
            }
            None => Vec::new(),
        }
    }

    /// Collapses the selection to its focus.
    pub(crate) fn collapse_selection(&mut self) {
        self.set_selection(Selection::caret(self.selection.focus));
    }

    /// Puts back an earlier state, either an undo step or the document as it was when the colour
    /// picker opened (Esc).
    pub(crate) fn restore(&mut self, doc: Document, selection: Selection) {
        self.doc = doc;
        self.selection = selection;
        self.image_focus = None;
        self.pending = None;
        self.replacement = None;
        self.trigger = None;
        self.ghost = None;
        self.touch();
    }

    /// Whether ⌘Z would do anything.
    pub fn can_undo(&self) -> bool {
        self.undo.can_undo()
    }

    /// Whether redo would do anything.
    pub fn can_redo(&self) -> bool {
        self.undo.can_redo()
    }

    /// Ties `mark` to the latest change, so [`Self::undo`] and [`Self::redo`] hand it back (a
    /// recipient change made from the body).
    pub fn mark_undo(&mut self, mark: UndoMark) {
        self.undo.mark(mark);
    }

    /// Records an undo step now, before an emoji, mention or snippet goes in.
    pub fn save_undo(&mut self) {
        self.undo.save(&self.doc, self.selection);
    }
}

/// A draft body as the editor holds it, sanitized (`exo_mail::sanitize_fragment`), read, and
/// given its trailing `<br>`s.
fn load(body: &str) -> Document {
    let mut doc = html::parse(&exo_mail::sanitize_fragment(body));
    fix_cursors(&mut doc);
    doc
}

/// `text` as a run can hold it. Whitespace a browser shows as a space becomes a space, and what
/// it shows as nothing goes. Text enters the model already as it shows, so what's written reads
/// back the same.
pub(in crate::editor) fn plain(text: &str) -> String {
    text.chars()
        .filter(|ch| !is_invisible(*ch))
        .map(|ch| if is_collapsible(ch) { ' ' } else { ch })
        .collect()
}

/// HTML whitespace other than a no-break space, which CSS shows as (at most) one space.
fn is_collapsible(ch: char) -> bool {
    matches!(ch, ' ' | '\t' | '\n' | '\r' | '\x0C')
}

/// Control characters (other than whitespace) and U+FEFF, a zero-width caret placeholder a draft
/// can carry, which the saved HTML drops.
fn is_invisible(ch: char) -> bool {
    (ch.is_control() && !is_collapsible(ch)) || ch == '\u{FEFF}'
}

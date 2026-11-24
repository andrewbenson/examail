//! Inline marks (`<b>`, `<i>`, `<u>`, `<s>`), colour swatches, links, and block-level list, quote
//! and indent commands.

use std::ops::Range;

use super::{Editor, plain, starts_with_ignore_ascii_case};
use crate::{
    blocks::{INDENT_STYLE, QUOTE_STYLE},
    model::{ContainerKind, Document, Element, Piece, Pos, Selection},
};

/// An inline format with a key of its own.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Format {
    /// `<b>` (⌘B).
    Bold,
    /// `<i>` (⌘I).
    Italic,
    /// `<u>` (⌘U).
    Underline,
    /// `<s>` (⌘⇧X).
    Strikethrough,
}

impl Format {
    /// The element the format wraps text in.
    pub const fn tag(self) -> &'static str {
        match self {
            Self::Bold => "b",
            Self::Italic => "i",
            Self::Underline => "u",
            Self::Strikethrough => "s",
        }
    }
}

/// A swatch of the colour subformatter, in order.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Color {
    /// `No color`, a dark gray that renders as the theme's text colour.
    None,
    /// `Lava Red`.
    LavaRed,
    /// `Irascible Orange`.
    IrascibleOrange,
    /// `Seafoam Green`.
    SeafoamGreen,
    /// `Conscientious Blue`.
    ConscientiousBlue,
    /// `Pretty In Pink`.
    PrettyInPink,
}

impl Color {
    /// Every swatch, in the picker's order.
    pub const ALL: [Self; 6] = [
        Self::None,
        Self::LavaRed,
        Self::IrascibleOrange,
        Self::SeafoamGreen,
        Self::ConscientiousBlue,
        Self::PrettyInPink,
    ];

    /// The swatch's name, its tooltip.
    pub const fn name(self) -> &'static str {
        match self {
            Self::None => "No color",
            Self::LavaRed => "Lava Red",
            Self::IrascibleOrange => "Irascible Orange",
            Self::SeafoamGreen => "Seafoam Green",
            Self::ConscientiousBlue => "Conscientious Blue",
            Self::PrettyInPink => "Pretty In Pink",
        }
    }

    /// The CSS colour.
    pub const fn rgb(self) -> [u8; 3] {
        match self {
            Self::None => [34, 34, 34],
            Self::LavaRed => [222, 84, 84],
            Self::IrascibleOrange => [233, 150, 56],
            Self::SeafoamGreen => [92, 186, 135],
            Self::ConscientiousBlue => [74, 143, 225],
            Self::PrettyInPink => [201, 118, 209],
        }
    }

    /// The `span.colour` it applies, as outgoing HTML has it.
    pub fn element(self) -> Element {
        let [r, g, b] = self.rgb();
        let important = if self == Self::None {
            ""
        } else {
            " !important"
        };
        Element::with(
            "span",
            &[
                ("class", "colour"),
                ("style", &format!("color:rgb({r}, {g}, {b}){important}")),
            ],
        )
    }

    /// The swatch a `span.colour` element is, if it's one of the palette's.
    pub fn of(element: &Element) -> Option<Self> {
        Self::ALL
            .into_iter()
            .find(|color| color.element() == *element)
    }
}

/// What the link button and ⌘K do with the current selection.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum LinkAction {
    /// Nothing is selected and the caret isn't in a link, so ⌘K isn't the editor's (it opens the
    /// palette).
    None,
    /// The selection is inside one link, which ⌘K removes.
    Remove,
    /// Open the link input, prefilled with this (empty, or the `href` of a link the selection
    /// extends).
    Edit(String),
}

/// The `href` a link to `url` gets: `url`, or `http://` and `url` without a scheme. `None` for a
/// `chrome://` URL, which is ignored, and a `data:` URL, which the sanitizer would strip from the
/// draft.
fn link_href(url: &str) -> Option<String> {
    if starts_with_ignore_ascii_case(url, "chrome://")
        || starts_with_ignore_ascii_case(url, "chrome-extension://")
    {
        return None;
    }
    const SCHEMES: [&str; 13] = [
        "about:", "data:", "file:///", "ftp://", "http://", "https://", "irc://", "mailto:",
        "ssh://", "tel:", "ws://", "wss://", "chrome:",
    ];
    let href = if SCHEMES
        .iter()
        .any(|scheme| starts_with_ignore_ascii_case(url, scheme))
    {
        url.to_owned()
    } else {
        format!("http://{url}")
    };
    // The one scheme on the list that the sanitizer takes off a link.
    (!starts_with_ignore_ascii_case(&href, "data:")).then_some(href)
}

impl Editor {
    // Formats

    /// Whether the selection (or the caret) is all in `tag`.
    pub fn has_format(&self, tag: &str) -> bool {
        if self.selection.is_collapsed() {
            return self.typing_marks().iter().any(|mark| mark.is(tag));
        }
        let mut seen = false;
        for (index, range) in self.selected_ranges() {
            for run in self.doc.blocks[index].slice(range) {
                if let Piece::Text(_) = run.piece {
                    if !run.marks.iter().any(|mark| mark.is(tag)) {
                        return false;
                    }
                    seen = true;
                }
            }
        }
        seen
    }

    /// Toggles `format` on the selection, or for what's typed next at a caret (⌘B, ⌘I, ⌘U,
    /// ⌘⇧X).
    pub fn toggle_format(&mut self, format: Format) {
        let tag = format.tag();
        self.undo.save(&self.doc, self.selection);
        if self.has_format(tag) {
            self.format(None, |mark| mark.is(tag));
        } else {
            self.format(Some(Element::new(tag)), |_| false);
        }
    }

    /// Changes the format, without an undo step. On the selection, it drops the marks `remove`
    /// matches and wraps what isn't in `add` yet in `add` (innermost). At a caret, it does that to
    /// what's typed next.
    fn format(&mut self, add: Option<Element>, remove: impl Fn(&Element) -> bool) {
        self.format_inner(add, remove, true);
    }

    fn format_inner(
        &mut self,
        add: Option<Element>,
        remove: impl Fn(&Element) -> bool,
        record_change: bool,
    ) {
        let restyle = |marks: &mut Vec<Element>| {
            marks.retain(|mark| !remove(mark));
            if let Some(element) = &add
                && !marks.contains(element)
            {
                marks.push(element.clone());
            }
        };
        if self.selection.is_collapsed() {
            let mut marks = self.typing_marks();
            restyle(&mut marks);
            self.pending = Some(marks);
            return self.touch();
        }
        for (index, range) in self.selected_ranges() {
            let block = &mut self.doc.blocks[index];
            let mut runs = block.slice(range.clone());
            for run in &mut runs {
                restyle(&mut run.marks);
            }
            block.remove(range.clone());
            block.insert(range.start, runs);
        }
        if record_change {
            self.changed();
        } else {
            self.doc.fix_lines();
            self.ghost = None;
            self.touch();
        }
    }

    /// The selected part of each block, `(block, offsets)`.
    fn selected_ranges(&self) -> Vec<(usize, Range<usize>)> {
        let (start, end) = (self.selection.start(), self.selection.end());
        (start.block..=end.block)
            .filter(|index| self.doc.blocks[*index].is_inline())
            .map(|index| {
                let from = if index == start.block {
                    start.offset
                } else {
                    0
                };
                let to = if index == end.block {
                    end.offset
                } else {
                    self.doc.blocks[index].len()
                };
                (index, from..to)
            })
            .filter(|(_, range)| !range.is_empty())
            .collect()
    }

    /// The swatch the selection (or caret) is coloured with, which the colour picker starts on.
    pub fn color(&self) -> Color {
        let marks = if self.selection.is_collapsed() {
            self.typing_marks()
        } else {
            let start = self.selection.start();
            self.doc.blocks[start.block].marks_at(start.offset + 1)
        };
        marks
            .iter()
            .rev()
            .find_map(Color::of)
            .unwrap_or(Color::None)
    }

    /// Colours the selection, or what's typed next at a caret (the colour subformatter).
    pub fn set_color(&mut self, color: Color) {
        self.undo.save(&self.doc, self.selection);
        self.apply_color(color);
    }

    /// Colours the selection, recording the change on the undo stack.
    pub(crate) fn apply_color(&mut self, color: Color) {
        self.format(Some(color.element()), |mark| {
            mark.is("span") && mark.attr("class") == Some("colour")
        });
    }

    /// Applies `color` as a live colour-picker preview without touching the undo stack; confirming
    /// records a single snapshot of the pre-picker document, and Escape rolls back cleanly.
    pub(crate) fn preview_color(&mut self, color: Color) {
        self.format_inner(
            Some(color.element()),
            |mark| mark.is("span") && mark.attr("class") == Some("colour"),
            false,
        );
    }

    // Links

    /// The link the caret is in, if any (for the link popover). The caret is in a link when the
    /// text it types into is.
    pub fn link_at_caret(&self) -> Option<String> {
        if !self.selection.is_collapsed() {
            return None;
        }
        let caret = self.caret();
        let marks = self.doc.blocks[caret.block].marks_at(caret.offset);
        marks
            .iter()
            .find(|mark| mark.is("a"))
            .and_then(|a| a.attr("href").map(str::to_owned))
    }

    /// What ⌘K or the link button does now.
    pub fn link_action(&self) -> LinkAction {
        if self.selection.is_collapsed() {
            return if self.link_at_caret().is_some() {
                LinkAction::Remove
            } else {
                LinkAction::None
            };
        }
        let mut hrefs = Vec::new();
        let mut unlinked = false;
        for (index, range) in self.selected_ranges() {
            for run in self.doc.blocks[index].slice(range) {
                match run
                    .marks
                    .iter()
                    .find(|mark| mark.is("a"))
                    .and_then(|a| a.attr("href"))
                {
                    Some(href) => hrefs.push(href.to_owned()),
                    None => unlinked = true,
                }
            }
        }
        match hrefs.first() {
            Some(_) if !unlinked && hrefs.windows(2).all(|pair| pair[0] == pair[1]) => {
                LinkAction::Remove
            }
            Some(href) => LinkAction::Edit(href.clone()),
            None => LinkAction::Edit(String::new()),
        }
    }

    /// Saves the link input. An empty input removes the link, `chrome://` is ignored, and a
    /// missing scheme becomes `http://`. Then the selection links there with `target="_blank"`. A
    /// caret gets the URL (without its scheme) as the link's text. A `data:` URL, which the
    /// sanitizer would strip from the draft, is ignored too.
    pub fn set_link(&mut self, input: &str) {
        let url = input.trim();
        if url.is_empty() {
            return self.remove_link();
        }
        let Some(href) = link_href(url) else {
            return;
        };
        self.undo.save(&self.doc, self.selection);
        if self.selection.is_collapsed() {
            let text = plain(
                href.split_once("//")
                    .map_or(href.as_str(), |(_, rest)| rest),
            );
            let start = self.caret_for_text();
            let marks = self.typing_marks();
            let end = self.doc.insert_text(start, &text, marks);
            self.selection = Selection::new(start, end);
        }
        self.format(
            Some(Element::with("a", &[("href", &href), ("target", "_blank")])),
            |mark| mark.is("a"),
        );
    }

    /// `url` pasted over a selection that's exactly a link's text, white space around it aside.
    /// The link goes to `url`, and its text shows the new URL as it showed the old one (the whole
    /// URL, its origin, or its host and some of what follows), in one undo step. Text that didn't
    /// show its URL is only linked anew. Returns whether the link was rewritten.
    pub(super) fn relink(&mut self, url: &str) -> bool {
        let (start, end) = (self.selection.start(), self.selection.end());
        if start.block != end.block {
            return false;
        }
        let index = start.block;
        let block = &self.doc.blocks[index];
        let chars: Vec<char> = block.text().chars().collect();
        let (mut from, mut to) = (start.offset, end.offset);
        while from < to && chars[from].is_whitespace() {
            from += 1;
        }
        while to > from && chars[to - 1].is_whitespace() {
            to -= 1;
        }
        let runs = block.slice(from..to);
        let Some(link) = runs
            .first()
            .and_then(|run| run.marks.iter().find(|mark| mark.is("a")))
            .cloned()
        else {
            return false;
        };
        let linked = |offset: usize| {
            block
                .slice(offset..offset + 1)
                .first()
                .is_some_and(|run| run.marks.contains(&link))
        };
        let whole = runs.iter().all(|run| run.marks.contains(&link))
            && !(from > 0 && linked(from - 1))
            && !(to < block.len() && linked(to));
        if !whole {
            return false;
        }
        // The link's own text is what's linked anew.
        self.selection = Selection::new(Pos::new(index, from), Pos::new(index, to));
        let old = link
            .attr("href")
            .and_then(|href| url::Url::parse(href).ok());
        let Some(href) = link_href(url) else {
            return false;
        };
        let text = old.zip(url::Url::parse(&href).ok()).and_then(|(old, new)| {
            crate::url_form::matched(&new, &old, &chars[from..to].iter().collect::<String>())
        });
        let Some(text) = text else {
            return false;
        };
        self.undo.save(&self.doc, self.selection);
        let mut marks = runs[0].marks.clone();
        marks.retain(|mark| !mark.is("a"));
        marks.push(Element::with("a", &[("href", &href), ("target", "_blank")]));
        self.doc.blocks[index].remove(from..to);
        let end = self.doc.insert_text(Pos::new(index, from), &text, marks);
        self.selection = Selection::new(Pos::new(index, from), end);
        self.changed();
        true
    }

    /// The whole link the caret is in: its block, its range there and its element.
    pub(crate) fn link_around_caret(&self) -> Option<(usize, Range<usize>, Element)> {
        if !self.selection.is_collapsed() {
            return None;
        }
        let caret = self.caret();
        let block = &self.doc.blocks[caret.block];
        let link = block
            .marks_at(caret.offset)
            .into_iter()
            .find(|mark| mark.is("a") && mark.attr("href").is_some())?;
        let linked = |offset: usize| {
            block
                .slice(offset..offset + 1)
                .first()
                .is_some_and(|run| run.marks.contains(&link))
        };
        let mut start = caret.offset;
        while start > 0 && linked(start - 1) {
            start -= 1;
        }
        let mut end = caret.offset;
        while end < block.len() && linked(end) {
            end += 1;
        }
        Some((caret.block, start..end, link))
    }

    /// Removes links from the selection, or the whole link the caret is in.
    pub fn remove_link(&mut self) {
        if self.selection.is_collapsed() {
            let Some((block, range, _)) = self.link_around_caret() else {
                return;
            };
            self.undo.save(&self.doc, self.selection);
            let saved = self.selection;
            self.selection =
                Selection::new(Pos::new(block, range.start), Pos::new(block, range.end));
            self.format(None, |mark| mark.is("a"));
            self.selection = saved;
            return;
        }
        self.undo.save(&self.doc, self.selection);
        self.format(None, |mark| mark.is("a"));
    }

    // Blocks

    /// The selected blocks, with the Tab rule that a selection ending at the very start of a later
    /// block doesn't take that block.
    fn selected_blocks(&self, pull_back: bool) -> Range<usize> {
        let (start, end) = (self.selection.start(), self.selection.end());
        let last = if pull_back && end.offset == 0 && end.block > start.block {
            end.block - 1
        } else {
            end.block
        };
        start.block..last + 1
    }

    /// Runs a block operation on the selection, which stays where it is (block operations move
    /// blocks between containers, never add or remove any). Quote operations then keep a line
    /// after the last block; list operations don't.
    fn modify(
        &mut self,
        pull_back: bool,
        bottom_line: bool,
        operation: impl FnOnce(&mut Document, Range<usize>),
    ) {
        self.undo.save(&self.doc, self.selection);
        self.doc
            .modify_blocks(self.selected_blocks(pull_back), operation);
        if bottom_line {
            self.doc.ensure_bottom_line();
        }
        self.doc.renumber(self.selection.focus.block);
        self.changed();
    }

    /// Whether the selection is in a list, where Tab and Shift+Tab indent.
    pub fn in_list(&self) -> bool {
        let range = self.selected_blocks(false);
        self.doc
            .common_path(range.clone())
            .iter()
            .any(|c| matches!(c.element.container_kind(), ContainerKind::List { .. }))
            || (range.len() == 1 && self.doc.blocks[range.start].list().is_some())
    }

    fn in_quote(&self) -> bool {
        let range = self.selected_blocks(false);
        self.doc
            .common_path(range)
            .iter()
            .any(|c| c.element.container_kind() == ContainerKind::Quote)
    }

    fn in_any_blockquote(&self) -> bool {
        let range = self.selected_blocks(false);
        self.doc
            .common_path(range)
            .iter()
            .any(|c| c.element.is("blockquote"))
    }

    /// Toggles a numbered (`ordered`) or bulleted list (⌘⇧7, ⌘⇧8).
    pub fn toggle_list(&mut self, ordered: bool) {
        let tag = if ordered { "ol" } else { "ul" };
        let range = self.selected_blocks(false);
        let common = self.doc.common_path(range.clone());
        let inside = common.iter().any(|c| c.element.is(tag))
            || (range.len() == 1
                && self.doc.blocks[range.start]
                    .path
                    .iter()
                    .any(|c| c.element.is(tag)));
        // Snapshot before stripping `style` so undo restores the attribute intact.
        self.undo.save(&self.doc, self.selection);
        // First, the selection's common ancestor loses its `style`.
        if range.len() == 1 {
            self.doc.blocks[range.start]
                .element
                .attrs
                .retain(|(name, _)| name != "style");
        } else if let Some(ancestor) = common.last().map(|c| c.id) {
            for block in &mut self.doc.blocks {
                if let Some(container) = block.path.iter_mut().find(|c| c.id == ancestor) {
                    container.element.attrs.retain(|(name, _)| name != "style");
                }
            }
        }
        self.doc.modify_blocks(range, |doc, range| {
            if inside {
                doc.remove_list(range)
            } else {
                doc.make_list(range, ordered, None)
            }
        });
        self.doc.renumber(self.selection.focus.block);
        self.changed();
    }

    /// Toggles a quote (⌘⇧9). Inside any blockquote, it removes them all, and otherwise it wraps
    /// the selection in one.
    pub fn toggle_quote(&mut self) {
        let inside = self.in_any_blockquote();
        self.modify(false, true, |doc, range| {
            if inside {
                doc.unwrap_all_blockquotes(range)
            } else {
                doc.wrap_blockquote(range, QUOTE_STYLE)
            }
        });
    }

    /// ⌘]: nests list items, adds a quote level inside a quote, and indents anything else by 40px.
    pub fn increase_level(&mut self) {
        if self.in_list() {
            return self.modify(false, false, Document::increase_list_level);
        }
        let style = if self.in_quote() {
            QUOTE_STYLE
        } else {
            INDENT_STYLE
        };
        self.modify(false, true, |doc, range| doc.wrap_blockquote(range, style));
    }

    /// ⌘[: outdents list items, or removes the outermost quote or indent.
    pub fn decrease_level(&mut self) {
        if self.in_list() {
            return self.modify(false, false, Document::decrease_list_level);
        }
        self.modify(false, true, Document::unwrap_outer_blockquote);
    }

    /// Tab in a list: nests the selected items. Returns whether the selection was in
    /// a list (outside one, Tab isn't the editor's).
    pub fn indent_list(&mut self) -> bool {
        if !self.in_list() {
            return false;
        }
        self.modify(true, false, Document::increase_list_level);
        true
    }

    /// Shift+Tab in a list: outdents the selected items. Returns whether it applied.
    pub fn outdent_list(&mut self) -> bool {
        if !self.in_list() {
            return false;
        }
        self.modify(true, false, Document::decrease_list_level);
        true
    }
}

#[cfg(test)]
mod tests {
    use crate::{Editor, Pos, Selection};

    #[test]
    fn toggle_list_undo_restores_stripped_style() {
        // Single-block selection strips the block element's `style`.
        let mut single = Editor::from_html(r#"<div style="color:red">alpha</div>"#);
        let before = single.html();
        single.toggle_list(false);
        assert!(
            !single.html().contains("color:red"),
            "toggle_list strips style: {}",
            single.html()
        );
        single.undo();
        assert_eq!(single.html(), before, "undo restores stripped block style");

        // Multi-block selection strips the common ancestor container's `style`.
        let mut multi = Editor::from_html(
            r#"<blockquote style="margin:0 0 0 40px"><div>one</div><div>two</div></blockquote>"#,
        );
        let before = multi.html();
        multi.set_selection(Selection::new(Pos::new(0, 0), Pos::new(1, 3)));
        multi.toggle_list(true);
        assert!(
            !multi.html().contains("margin:0 0 0 40px"),
            "toggle_list strips ancestor style: {}",
            multi.html()
        );
        multi.undo();
        assert_eq!(
            multi.html(),
            before,
            "undo restores stripped container style"
        );
    }
}

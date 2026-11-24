//! Autolinking, HTML and plain-text pasting, fragment insertion, and signature/quote splicing.

use std::ops::Range;

use super::{Editor, plain};
use crate::{
    html,
    model::{Block, Content, Document, Element, Piece, Pos, Run, Selection},
};

/// The links autolinking makes in one text node, by running [`exo_mail::match_link`] and then
/// again on what follows each link.
fn autolinks(text: &str) -> Vec<(Range<usize>, String)> {
    let mut found = Vec::new();
    let mut start = 0;
    while let Some((range, href)) = exo_mail::match_link(&text[start..]) {
        if range.is_empty() {
            break;
        }
        found.push((start + range.start..start + range.end, href));
        start += range.end;
    }
    found
}

/// Whether autolinking leaves `text` alone outside a plain-text paste, which it does for a text
/// node that is only an email address.
fn is_address(text: &str) -> bool {
    matches!(exo_mail::match_link(text), Some((range, href)) if range.len() == text.len() && href.starts_with("mailto:"))
}

/// `run`, with the links that `finder` finds in its text wrapped in `link(href)` as the innermost
/// mark. `finder` gives `(byte range, href)` pairs in order. This autolinks one text node, and a
/// run already in a link stays as it is.
pub(crate) fn link_run(
    run: Run,
    finder: impl Fn(&str) -> Vec<(Range<usize>, String)>,
    link: impl Fn(&str) -> Element,
) -> Vec<Run> {
    let Piece::Text(text) = &run.piece else {
        return vec![run];
    };
    if run.marks.iter().any(|mark| mark.is("a")) {
        return vec![run];
    }
    let mut out = Vec::new();
    let mut last = 0;
    for (range, href) in finder(text) {
        if range.start > last {
            out.push(Run::text(&text[last..range.start], run.marks.clone()));
        }
        let mut marks = run.marks.clone();
        marks.push(link(&href));
        out.push(Run::text(&text[range.clone()], marks));
        last = range.end;
    }
    if last < text.len() {
        out.push(Run::text(&text[last..], run.marks.clone()));
    }
    out
}

/// The link autolinking makes, with `target="_blank"`.
fn autolink_element(href: &str) -> Element {
    Element::with("a", &[("href", href), ("target", "_blank")])
}

pub(crate) fn starts_with_ignore_ascii_case(text: &str, prefix: &str) -> bool {
    text.get(..prefix.len())
        .is_some_and(|head| head.eq_ignore_ascii_case(prefix))
}

/// After loading, a block element without a `<br>` gets one at its end.
pub(crate) fn fix_cursors(doc: &mut Document) {
    for block in &mut doc.blocks {
        if !block.element.tag.is_empty()
            && block.is_inline()
            && !block.runs().iter().any(|run| run.piece == Piece::Break)
        {
            block.trailing_break = true;
        }
    }
}

/// Plain text as a paste makes it, with a block per line, less one trailing newline.
pub(crate) fn plain_fragment(text: &str) -> Document {
    let text = text.replace("\r\n", "\n");
    let text = text.strip_suffix('\n').unwrap_or(&text);
    let blocks = text
        .split('\n')
        .map(|line| {
            let line = plain(line);
            Block::div(
                Vec::new(),
                if line.is_empty() {
                    Vec::new()
                } else {
                    vec![Run::text(line, Vec::new())]
                },
            )
        })
        .collect();
    Document::new(blocks)
}

impl Editor {
    /// Links URLs in the run before the caret, on Space and Enter (a text that is only an email
    /// address is left alone).
    pub(super) fn autolink(&mut self) {
        let caret = self.caret();
        let block = &mut self.doc.blocks[caret.block];
        // The text node the caret is in, which is the run that ends at (or holds) the caret.
        let mut end = 0;
        let Some(index) = block.runs().iter().position(|run| {
            let start = end;
            end += run.piece.len();
            caret.offset > start && caret.offset <= end
        }) else {
            return;
        };
        if matches!(&block.runs()[index].piece, Piece::Text(text) if is_address(text)) {
            return;
        }
        let runs = block.runs_mut();
        let linked = link_run(runs.remove(index), autolinks, autolink_element);
        runs.splice(index..index, linked);
        crate::model::normalize(runs);
    }

    // Paste and insertion

    /// Pastes HTML at the selection, through the paste cleanup. Its `data:` images become the
    /// draft's inline attachments ([`Self::take_uploads`]).
    pub fn paste_html(&mut self, markup: &str) {
        let fragment = self.pasted_html(markup);
        self.insert_fragment(fragment, false);
    }

    /// `markup` cleaned as a paste is, with its `data:` images taken as the draft's.
    pub(crate) fn pasted_html(&mut self, markup: &str) -> Document {
        let mut fragment = html::parse_paste(&exo_mail::sanitize_fragment(
            &exo_mail::prepare_paste(markup),
        ));
        self.take_pasted_images(&mut fragment);
        fragment
    }

    /// Pastes plain text as a block per line, less one trailing newline. A single URL pasted over
    /// a selection links it instead, rewriting a link's text that showed its old URL
    /// ([`Self::relink`]).
    pub fn paste_text(&mut self, text: &str) {
        let trimmed = text.trim();
        if !self.selection.is_collapsed()
            && !trimmed.is_empty()
            && !trimmed.contains(char::is_whitespace)
            && (starts_with_ignore_ascii_case(trimmed, "http://")
                || starts_with_ignore_ascii_case(trimmed, "https://")
                || starts_with_ignore_ascii_case(trimmed, "www."))
        {
            if !self.relink(trimmed) {
                self.set_link(trimmed);
            }
            return;
        }
        self.insert_fragment(plain_fragment(text), true);
    }

    /// Inserts a document fragment at the selection. Its URLs are linked (a text that is only an
    /// email address too if `emails`, as for plain text), and its blocks get their trailing
    /// `<br>`. Its first block's content joins the caret's line, and the rest of that line joins
    /// the end of its last block.
    pub(crate) fn insert_fragment(&mut self, mut fragment: Document, emails: bool) {
        for block in &mut fragment.blocks {
            if let Content::Inline(runs) = &mut block.content {
                *runs = std::mem::take(runs)
                    .into_iter()
                    .flat_map(|run| {
                        let address =
                            !emails && matches!(&run.piece, Piece::Text(text) if is_address(text));
                        if address {
                            vec![run]
                        } else {
                            link_run(run, autolinks, autolink_element)
                        }
                    })
                    .collect();
            }
        }
        fix_cursors(&mut fragment);
        self.undo.save(&self.doc, self.selection);
        self.delete_selection();
        self.place_fragment(fragment);
    }

    /// Puts `fragment`'s blocks in at the caret, leaving the caret after them. Returns how many
    /// blocks were added before the blocks following the caret (excluding any bottom line appended
    /// at the document's end), and whether the remainder of the caret's original block stayed
    /// merged into the caret's ending block.
    pub(super) fn place_fragment(&mut self, fragment: Document) -> (usize, bool) {
        let before = self.doc.blocks.len();
        let was_inline = self.doc.blocks[self.caret().block].is_inline();
        let caret = self.caret_for_text();
        let mut blocks = fragment.blocks;
        let marks = self.typing_marks();
        let inline_only = blocks.len() == 1 && blocks[0].path.is_empty() && blocks[0].is_inline();
        if inline_only {
            let mut runs = blocks.remove(0).runs().to_vec();
            for run in &mut runs {
                // A pasted link inside a link replaces it, because HTML can't nest links.
                let linked = run.marks.iter().any(|mark| mark.is("a"));
                run.marks.splice(
                    0..0,
                    marks
                        .iter()
                        .filter(|mark| !(linked && mark.is("a")))
                        .cloned(),
                );
            }
            let len: usize = runs.iter().map(|run| run.piece.len()).sum();
            self.doc.blocks[caret.block].insert(caret.offset, runs);
            self.collapse_to(Pos::new(caret.block, caret.offset + len));
            self.changed();
            return (self.doc.blocks.len() - before, was_inline);
        }
        // Split the line and put the fragment's blocks between the halves, in its containers.
        let after = self.doc.split(caret);
        let base_path = self.doc.blocks[caret.block].path.clone();
        let mut ids = std::collections::HashMap::new();
        let inserted: Vec<Block> = blocks
            .into_iter()
            .map(|mut block| {
                for container in &mut block.path {
                    container.id = *ids
                        .entry(container.id)
                        .or_insert_with(|| self.doc.fresh_id());
                }
                block.path.splice(0..0, base_path.iter().cloned());
                block
            })
            .collect();
        let count = inserted.len();
        self.doc.blocks.splice(after.block..after.block, inserted);
        let last = after.block + count - 1;
        // The first and last fragment blocks join the line's halves when they're plain lines.
        let end = Pos::new(last, self.doc.blocks[last].len());
        let mut caret_at = end;
        let mut tail_merged = false;
        if self.doc.blocks[last + 1].path == self.doc.blocks[last].path
            && self.doc.blocks[last].is_inline()
        {
            caret_at = self.doc.merge_with_previous(last + 1);
            tail_merged = true;
        }
        if self.doc.blocks[after.block].path == self.doc.blocks[caret.block].path
            && self.doc.blocks[after.block].is_inline()
        {
            let joined = self.doc.merge_with_previous(after.block);
            if caret_at.block == after.block {
                caret_at = Pos::new(joined.block, joined.offset + caret_at.offset);
            } else {
                caret_at.block -= 1;
            }
        }
        if self.doc.blocks[caret.block].is_empty()
            && caret.block + 1 < self.doc.blocks.len()
            && self.doc.blocks[caret.block].path == base_path
            && caret_at.block > caret.block
        {
            self.doc.blocks.remove(caret.block);
            caret_at.block -= 1;
        }
        let added = self.doc.blocks.len() - before;
        self.doc.ensure_bottom_line();
        self.collapse_to(self.doc.clamp(caret_at));
        self.changed();
        (added, was_inline && tail_merged)
    }

    /// Inserts an already-built HTML fragment at the caret (a snippet's body) in one undo step.
    pub fn insert_html(&mut self, markup: &str) {
        let fragment = html::parse(&exo_mail::sanitize_fragment(markup));
        self.insert_fragment(fragment, false);
    }

    /// Opens the quote and signature into the body ("…"). The body is rebuilt around them
    /// ([`outgoing::inlined`](crate::outgoing::inlined)) and undo starts over. `signature` is the
    /// signature block ([`crate::outgoing::signature`]), and `quote` is the reply or forward
    /// quote, or `None` for new mail.
    ///
    /// This doesn't sanitize, so `target="_blank"` stays on the quote's links. Both parts must
    /// already be sanitized, as for [`outgoing::html`](crate::outgoing::html).
    pub fn reveal_quoted(&mut self, signature: Option<&str>, quote: Option<&str>) {
        let mut doc = html::parse(&crate::outgoing::inlined(&self.html(), signature, quote));
        fix_cursors(&mut doc);
        // The empty classes outgoing HTML has on what shows text.
        crate::outgoing::add_empty_classes(&mut doc.blocks);
        self.replace_document(doc);
    }

    /// Swaps the signature, for a From changed after "…" was opened. The signature block that
    /// draft `draft_id` put in the body becomes `new`, unless its text no longer reads as `old` (it
    /// was edited), in which case it's left alone. Both are [`crate::outgoing::signature`] blocks.
    /// Undo starts over. Returns whether it was swapped.
    pub fn switch_signature(&mut self, draft_id: &str, old: &str, new: &str) -> bool {
        let has_id = |block: &Block| {
            block.element.attr("data-signature-draft-id") == Some(draft_id)
                || block
                    .path
                    .iter()
                    .any(|c| c.element.attr("data-signature-draft-id") == Some(draft_id))
        };
        let Some(start) = self.doc.blocks.iter().position(&has_id) else {
            return false;
        };
        let end = start
            + self.doc.blocks[start..]
                .iter()
                .take_while(|block| has_id(block))
                .count();
        // Their text without whitespace, which is what's compared.
        let squashed = |blocks: &[Block]| -> String {
            blocks
                .iter()
                .flat_map(|block| match &block.content {
                    Content::Raw { text, .. } => {
                        format!("\u{FFFC}{text}").chars().collect::<Vec<_>>()
                    }
                    _ => block.text().chars().collect::<Vec<_>>(),
                })
                .filter(|ch| !ch.is_whitespace())
                .collect()
        };
        let old_blocks = if old.trim().is_empty() {
            Vec::new()
        } else {
            html::parse(old).blocks
        };
        if squashed(&self.doc.blocks[start..end]) != squashed(&old_blocks) {
            return false;
        }
        let first = &self.doc.blocks[start];
        let outer_path: Vec<crate::model::Container> = match first
            .path
            .iter()
            .position(|c| c.element.attr("data-signature-draft-id") == Some(draft_id))
        {
            Some(depth) => first.path[..depth].to_vec(),
            None => first.path.clone(),
        };
        let mut replacement = if new.trim().is_empty() {
            Vec::new()
        } else {
            let mut parsed = html::parse(new);
            fix_cursors(&mut parsed);
            crate::outgoing::add_empty_classes(&mut parsed.blocks);
            parsed.blocks
        };
        let mut ids = std::collections::HashMap::new();
        for block in &mut replacement {
            for container in &mut block.path {
                container.id = *ids
                    .entry(container.id)
                    .or_insert_with(|| self.doc.fresh_id());
            }
            block.path.splice(0..0, outer_path.iter().cloned());
        }
        let removed = end - start;
        let added = replacement.len();
        self.doc.blocks.splice(start..end, replacement);
        if self.doc.blocks.is_empty() {
            self.doc.blocks.push(Block::empty(Vec::new()));
        }
        let map_pos = |pos: Pos| -> Pos {
            if pos.block < start {
                pos
            } else if pos.block >= end {
                Pos::new((pos.block - removed) + added, pos.offset)
            } else {
                Pos::new(start, 0)
            }
        };
        self.selection = Selection::new(
            self.snap(map_pos(self.selection.anchor)),
            self.snap(map_pos(self.selection.focus)),
        );
        self.image_focus = None;
        self.pending = None;
        self.replacement = None;
        self.trigger = None;
        self.ghost = None;
        self.undo.reset(&self.doc, self.selection);
        self.touch();
        true
    }
}

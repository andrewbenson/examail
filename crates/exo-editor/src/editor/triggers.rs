//! Markdown block triggers (`* `, `1. `, `> `, `---`, ```` ``` ````), Unicode symbol replacements,
//! inline triggers (`:`, `;`, `+`, `@`, `-`), and ghost-text predictions.

use super::{Editor, plain};
use crate::{
    blocks::QUOTE_STYLE,
    model::{Block, Content, Document, Element, Pos, Run},
    trigger::{self, Trigger},
};

/// The Unicode replacement, shared with the subject field. After `typed` was typed, returns the
/// replacement for the end of `before` (the text up to the caret, `typed` included) and how many
/// characters it replaces. For example, `->` becomes `→`.
pub fn unicode_replacement(typed: char, before: &[char]) -> Option<(usize, &'static str)> {
    if !matches!(typed, '>' | '-' | '—' | '=' | '~' | '<' | '.') {
        return None;
    }
    // Each rule's alternatives must follow the start, a word character or whitespace.
    let boundary = |at: usize| {
        at == 0
            || before[at - 1].is_ascii_alphanumeric()
            || before[at - 1] == '_'
            || before[at - 1].is_whitespace()
    };
    UNICODE_REPLACEMENTS
        .iter()
        .find_map(|(patterns, replacement)| {
            patterns.iter().find_map(|pattern| {
                let pattern: Vec<char> = pattern.chars().collect();
                (before.ends_with(&pattern) && boundary(before.len() - pattern.len()))
                    .then_some((pattern.len(), *replacement))
            })
        })
}

/// The Unicode replacements. The first rule that matches wins, and a rule's longer alternatives
/// are tried first, as a regex's leftmost match does.
const UNICODE_REPLACEMENTS: [(&[&str], &str); 11] = [
    (&["-->", "->", "—>"], "→"),
    (&["==>", "=>"], "⇒"),
    (&["--"], "—"),
    (&["<="], "⇐"),
    (&["<-", "<—"], "←"),
    (&["<~"], "↜"),
    (&["~~>", "~>"], "↝"),
    (&[">="], "≥"),
    (&["=<"], "≤"),
    (&["<->", "←>"], "⭤"),
    (&["..."], "…"),
];

impl Editor {
    /// On a line of three or more dashes (an em dash counts as two), outside lists, Enter puts a
    /// rule before the line and empties it, before anything else it would do (in a quote too).
    pub(super) fn rule_from_dashes(&mut self) -> bool {
        let caret = self.caret();
        let block = &self.doc.blocks[caret.block];
        if !self.selection.is_collapsed() || block.list().is_some() || !block.is_inline() {
            return false;
        }
        let text: String = block
            .text()
            .chars()
            .filter(|ch| *ch != '\u{200B}')
            .collect();
        let dashes = text.chars().filter(|ch| *ch == '-').count();
        let em = text.chars().filter(|ch| *ch == '—').count();
        let weight = text.chars().count() + em;
        if weight < 3 || weight != dashes + 2 * em {
            return false;
        }
        let rule = Block {
            path: block.path.clone(),
            element: Element::new("div"),
            content: Content::Rule,
            trailing_break: false,
        };
        self.doc
            .delete(Pos::new(caret.block, 0), Pos::new(caret.block, block.len()));
        self.doc.blocks.insert(caret.block, rule);
        self.collapse_to(Pos::new(caret.block + 1, 0));
        self.pending = None;
        self.changed();
        true
    }

    /// On a line of just ```` ``` ```` or `~~~~`, outside lists, Enter makes the line an empty
    /// code block with the caret in it. The fence's line is emptied, because a space left where
    /// the fence was, with the caret before it, would keep a second Enter at the end of the code
    /// from leaving the block, and on a later line would make it wrap the line above instead.
    pub(super) fn code_from_fence(&mut self) -> bool {
        let caret = self.caret();
        let block = &self.doc.blocks[caret.block];
        let text: String = block
            .text()
            .chars()
            .filter(|ch| *ch != '\u{200B}')
            .collect();
        if !self.selection.is_collapsed()
            || caret.offset == 0
            || block.list().is_some()
            || block.in_code()
            || !matches!(text.as_str(), "```" | "~~~~")
        {
            return false;
        }
        let len = block.len();
        self.doc
            .delete(Pos::new(caret.block, 0), Pos::new(caret.block, len));
        self.doc.blocks[caret.block].trailing_break = true;
        self.doc
            .modify_blocks(caret.block..caret.block + 1, Document::make_code);
        self.doc.ensure_bottom_line();
        self.collapse_to(Pos::new(caret.block, 0));
        self.pending = None;
        self.changed();
        true
    }

    /// The markdown triggers: `*`, `1.` or `>` alone before the caret, then Space.
    pub(super) fn markdown_trigger(&mut self) -> bool {
        let caret = self.caret();
        let block = &self.doc.blocks[caret.block];
        if !block.is_inline() || block.element.tag.is_empty() {
            return false;
        }
        let prefix: String = block
            .text()
            .chars()
            .take(caret.offset)
            .filter(|ch| *ch != '\u{200B}')
            .collect();
        let number = prefix.strip_suffix('.').filter(|digits| {
            (1..=4).contains(&digits.len()) && digits.bytes().all(|b| b.is_ascii_digit())
        });
        let bullet = prefix == "*" || prefix == "-" && self.settings.hyphen_starts_list;
        let ordered = number.is_some_and(|digits| {
            let number = digits.parse::<u32>().ok();
            number == Some(1) && !block.is_list_item()
                || number.is_some_and(|number| self.continues_list(caret.block, number))
        });
        if bullet || ordered {
            self.undo.save(&self.doc, self.selection);
            self.auto_list = Some(format!("{prefix} "));
            self.doc.delete(Pos::new(caret.block, 0), caret);
            let value = number.map(|digits| digits.trim_start_matches('0').to_owned());
            self.doc
                .modify_blocks(caret.block..caret.block + 1, |doc, range| {
                    doc.make_list(range, !bullet, value.as_deref())
                });
            self.doc.ensure_bottom_line();
            self.collapse_to(Pos::new(caret.block, 0));
            self.doc.renumber(caret.block);
            self.changed();
            return true;
        }
        if prefix == ">" {
            self.undo.save(&self.doc, self.selection);
            let block = &mut self.doc.blocks[caret.block];
            block.remove(0..caret.offset);
            if block.is_empty() {
                self.doc
                    .insert_text(Pos::new(caret.block, 0), "\u{200B}", Vec::new());
            }
            self.doc
                .modify_blocks(caret.block..caret.block + 1, |doc, range| {
                    doc.wrap_blockquote(range, QUOTE_STYLE)
                });
            self.doc.ensure_bottom_line();
            let len = self.doc.blocks[caret.block].len();
            self.collapse_to(Pos::new(caret.block, len));
            self.changed();
            return true;
        }
        false
    }

    /// Whether `number.` starting block `index` continues a numbered list cut short before it.
    /// That's the nearest `ol` among the block's earlier siblings, if its first number plus its
    /// items is `number`.
    fn continues_list(&self, index: usize, number: u32) -> bool {
        let path = &self.doc.blocks[index].path;
        let depth = path.len();
        let in_parent = |block: &Block| {
            block.path.len() >= depth && block.path.iter().zip(path).all(|(a, b)| a.id == b.id)
        };
        let Some(list) = self.doc.blocks[..index]
            .iter()
            .rev()
            .take_while(|block| in_parent(block))
            .find_map(|block| {
                block
                    .path
                    .get(depth)
                    .filter(|container| container.element.is("ol"))
            })
        else {
            return false;
        };
        let items: Vec<&Block> = self
            .doc
            .blocks
            .iter()
            .filter(|block| {
                block.path.len() == depth + 1
                    && block.path[depth].id == list.id
                    && block.is_list_item()
            })
            .collect();
        let parse = |value: Option<&str>| value.and_then(|value| value.trim().parse::<u32>().ok());
        let start = parse(list.element.attr("start")).unwrap_or(1);
        let first = parse(items.first().and_then(|item| item.element.attr("value"))).unwrap_or(0);
        number == start.max(first) + items.len() as u32
    }

    /// Unicode replacements: `->` becomes `→` and so on, as the last character is typed.
    pub(super) fn replace_unicode(&mut self, typed: char) {
        let caret = self.caret();
        let before: Vec<char> = self.doc.blocks[caret.block]
            .text()
            .chars()
            .take(caret.offset)
            .filter(|ch| *ch != '\u{200B}')
            .collect();
        let Some((len, replacement)) = unicode_replacement(typed, &before) else {
            return;
        };
        self.undo.save(&self.doc, self.selection);
        let start = caret.offset - len;
        let block = &mut self.doc.blocks[caret.block];
        let marks = block.marks_at(start + 1);
        block.remove(start..caret.offset);
        block.insert(start, vec![Run::text(replacement, marks)]);
        let at = Pos::new(caret.block, start + replacement.chars().count());
        self.collapse_to(at);
        self.replacement = Some(at);
        self.changed();
    }

    // Inline triggers and ghost text

    /// Replaces `start..end` with `text` in one undo step, which is how a caller resolves an inline
    /// trigger (an emoji for `:smile`, a name for `+ali`).
    pub fn replace(&mut self, start: Pos, end: Pos, text: &str) {
        let (start, end) = (
            self.doc.clamp(start.min(end)),
            self.doc.clamp(start.max(end)),
        );
        self.undo.save(&self.doc, self.selection);
        let marks = self.doc.blocks[start.block].marks_at(start.offset);
        let pos = self.doc.delete(start, end);
        self.collapse_to(pos);
        let pos = self.caret_for_text();
        let end = self.doc.insert_text(pos, &plain(text), marks);
        self.collapse_to(end);
        self.trigger = None;
        self.trigger_armed = false;
        self.changed();
    }

    /// The inline trigger being typed (`:smi`, `;intro`, `+ali`), for the caller to resolve.
    pub fn trigger(&self) -> Option<&Trigger> {
        self.trigger.as_ref()
    }

    /// The name of the `+`, `@` or `-` mention the caret is at the end of, at its line's end or
    /// before a space.
    pub fn mention_before_caret(&self) -> Option<String> {
        if !self.selection.is_collapsed() {
            return None;
        }
        let caret = self.caret();
        let text: Vec<char> = self.doc.blocks[caret.block].text().chars().collect();
        if text.get(caret.offset).is_some_and(|ch| !ch.is_whitespace()) {
            return None;
        }
        let before: String = text[..caret.offset.min(text.len())].iter().collect();
        match trigger::find(&before)? {
            (trigger::TriggerKind::Contact(_), _, name) if !name.is_empty() => Some(name),
            _ => None,
        }
    }

    /// Lets the next edit open the trigger before the caret as if its character had just been
    /// typed, so Backspace into a mention opens its contacts again.
    pub fn arm_trigger(&mut self) {
        self.trigger_dismissed = false;
        self.trigger_armed = true;
    }

    /// Closes the trigger's dropdown until another trigger character is typed (Esc).
    pub fn dismiss_trigger(&mut self) {
        self.trigger = None;
        self.trigger_dismissed = true;
        self.trigger_armed = false;
    }

    /// Re-reads the trigger before the caret. `typed` says a trigger character was just typed,
    /// which is what opens one (a caret merely landing on `:smile` doesn't).
    pub(super) fn update_trigger(&mut self, typed: bool) {
        if typed {
            self.trigger_dismissed = false;
            self.trigger_armed = true;
        }
        if self.trigger_dismissed || !self.selection.is_collapsed() {
            self.trigger = None;
            return;
        }
        let caret = self.caret();
        let block = &self.doc.blocks[caret.block];
        let before: String = block.text().chars().take(caret.offset).collect();
        let found = trigger::find(&before);
        self.trigger = match (found, &self.trigger) {
            (Some((kind, start, query)), _)
                if typed
                    || self.trigger_armed
                    || self.trigger.as_ref().is_some_and(|open| {
                        open.start.block == caret.block && open.start.offset == start
                    }) =>
            {
                Some(Trigger {
                    kind,
                    query,
                    start: Pos::new(caret.block, start),
                    end: caret,
                    anchor: egui::Rect::NOTHING,
                })
            }
            _ => None,
        };
    }

    /// Shows `text` as ghost text after the caret (a prediction), or hides it.
    pub fn set_ghost_text(&mut self, text: Option<String>) {
        self.ghost = text
            .map(|text| plain(&text))
            .filter(|text| !text.is_empty() && self.selection.is_collapsed());
    }

    /// The ghost text showing after the caret.
    pub fn ghost_text(&self) -> Option<&str> {
        self.ghost.as_deref()
    }

    /// Accepts the ghost text (Tab or →), inserting it as plain text in one undo step. Returns
    /// whether there was any.
    pub fn accept_ghost_text(&mut self) -> bool {
        let Some(text) = self.ghost.take() else {
            return false;
        };
        self.took_completion();
        self.undo.save(&self.doc, self.selection);
        let marks = self.typing_marks();
        self.insert_at_caret(&text, marks);
        true
    }
}

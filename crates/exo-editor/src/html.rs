//! HTML in and out: a draft body loaded and saved, and the paste cleanup.
//!
//! [`write`] produces what Chromium's `innerHTML` gives for the DOM the document stands for: block
//! elements with their attributes, inline elements nested as recorded, `<br>` in empty lines,
//! and spaces as Chromium's editing leaves them (`&nbsp;` where a plain space would collapse).
//! [`parse`] reads that back exactly, and reads foreign HTML (Gmail drafts, quoted replies,
//! signatures) with unknown structure as containers and text directly inside a container as a
//! line of its own. Reading doesn't sanitize, so the editor runs what it loads or pastes through
//! `exo_mail::sanitize_fragment` first.

use exo_mail::dom::{self, NodeRef, Style};

use crate::model::{Block, Container, Content, Document, Element, Piece, Run};

/// Elements laid out as blocks (the editor's blocks and containers, plus the HTML block-level
/// elements pasted mail uses).
const BLOCK_TAGS: [&str; 33] = [
    "address",
    "article",
    "aside",
    "blockquote",
    "center",
    "dd",
    "details",
    "dialog",
    "dir",
    "div",
    "dl",
    "dt",
    "fieldset",
    "figcaption",
    "figure",
    "footer",
    "form",
    "h1",
    "h2",
    "h3",
    "h4",
    "h5",
    "h6",
    "header",
    "hr",
    "li",
    "main",
    "nav",
    "ol",
    "p",
    "pre",
    "section",
    "ul",
];
/// Kept whole, as markup the editor doesn't edit inside.
const OPAQUE_TAGS: [&str; 1] = ["table"];
/// Left out with their contents: what the editor's clean-up drops, foreign content (SVG, MathML)
/// the editor can't show, and what runs or embeds other content, in case unsanitized markup
/// gets here.
const DROPPED_TAGS: [&str; 21] = [
    "applet",
    "base",
    "embed",
    "frame",
    "frameset",
    "head",
    "iframe",
    "link",
    "math",
    "meta",
    "noembed",
    "noframes",
    "noscript",
    "object",
    "plaintext",
    "script",
    "style",
    "svg",
    "template",
    "title",
    "xmp",
];
/// How deep elements nest. Chromium's parser (`kMaximumHTMLParserDOMTreeDepth`) adds deeper ones
/// as siblings, so a loaded body never nests deeper. It also bounds the reader's recursion.
const MAX_DEPTH: usize = 512;

/// Reads a draft body as the editor holds it once loaded. It doesn't sanitize.
pub fn parse(html: &str) -> Document {
    let (document, root) = fragment(html);
    let mut reader = Reader::default();
    reader.container(&root);
    dom::drop_tree(document);
    Document::new(reader.blocks)
}

/// Whether every top-level node of `html` is a `<blockquote>` or has no text.
pub(crate) fn is_blockquoted(html: &str) -> bool {
    let (document, root) = fragment(html);
    let result = dom::children(&root)
        .iter()
        .all(|child| dom::is(child, "blockquote") || dom::text_content(child).is_empty());
    dom::drop_tree(document);
    result
}

/// Reads pasted HTML with the paste cleanup. Only colour, weight, style and decoration survive as
/// inline styles, and the result is normalized the way the editor cleans what it inserts. It
/// doesn't sanitize.
pub(crate) fn parse_paste(html: &str) -> Document {
    let (document, root) = fragment(html);
    let mut reader = Reader {
        paste: true,
        ..Reader::default()
    };
    reader.container(&root);
    dom::drop_tree(document);
    Document::new(reader.blocks)
}

/// Parses `html` as a page's body would be (a `div`'s `innerHTML` parses the same way for the
/// content the editor holds). Returns the document, which must outlive any use of the tree, and
/// its `<body>`.
fn fragment(html: &str) -> (NodeRef, NodeRef) {
    let document = dom::parse(html);
    let body = dom::body(&document).unwrap_or_else(|| document.clone());
    prune(&body);
    (document, body)
}

/// Takes [`DROPPED_TAGS`] out of `root`, tables included, and gives elements deeper than
/// [`MAX_DEPTH`] their children as siblings. Iterative, so hostile nesting can't overflow the
/// stack.
fn prune(root: &NodeRef) {
    let mut levels = vec![(root.clone(), 0)];
    while let Some((node, depth)) = levels.pop() {
        let mut pending: std::collections::VecDeque<NodeRef> = dom::take_children(&node).into();
        let mut kept = Vec::with_capacity(pending.len());
        while let Some(child) = pending.pop_front() {
            if let Some(tag) = dom::tag(&child) {
                dom::clear_template_contents(&child);
                if DROPPED_TAGS.contains(&tag) {
                    dom::drop_tree(child);
                    continue;
                }
                if depth >= MAX_DEPTH {
                    for grandchild in dom::take_children(&child).into_iter().rev() {
                        pending.push_front(grandchild);
                    }
                } else {
                    levels.push((child.clone(), depth + 1));
                }
            }
            kept.push(child);
        }
        dom::set_children(&node, kept);
    }
}

#[derive(Default)]
struct Reader {
    blocks: Vec<Block>,
    path: Vec<Container>,
    marks: Vec<Element>,
    next_id: u32,
    paste: bool,
}

impl Reader {
    fn container(&mut self, node: &NodeRef) {
        let mut inline: Vec<Run> = Vec::new();
        for child in dom::children(node) {
            if let Some(tag) = dom::tag(&child) {
                if is_block(tag) || OPAQUE_TAGS.contains(&tag) {
                    self.flush_loose(&mut inline);
                    self.block(&child);
                } else if !DROPPED_TAGS.contains(&tag) {
                    self.inline(&child, &mut inline);
                }
            } else if dom::is_text(&child) {
                self.inline(&child, &mut inline);
            }
        }
        self.flush_loose(&mut inline);
    }

    /// Text and inline elements directly inside a container, read as a line with no block
    /// element.
    fn flush_loose(&mut self, inline: &mut Vec<Run>) {
        if inline.is_empty() {
            return;
        }
        let runs = std::mem::take(inline);
        let mut block = Block::div(self.path.clone(), runs);
        block.element = Element::new("");
        if finish(&mut block) || block.trailing_break {
            self.blocks.push(block);
        }
    }

    fn block(&mut self, node: &NodeRef) {
        let element = element_of(node, self.paste);
        if OPAQUE_TAGS.contains(&element.tag.as_str()) {
            let mut block = Block::div(self.path.clone(), Vec::new());
            block.element = Element::new("");
            block.content = Content::Raw {
                html: dom::outer_html(node),
                text: text_of(node),
            };
            self.blocks.push(block);
            return;
        }
        if element.is("hr") {
            let mut block = Block::div(self.path.clone(), Vec::new());
            block.element = Element::new("");
            block.content = Content::Rule;
            self.blocks.push(block);
            return;
        }
        let children = dom::children(node);
        let has_blocks = children.iter().any(|child| {
            dom::tag(child).is_some_and(|tag| is_block(tag) || OPAQUE_TAGS.contains(&tag))
        });
        // `<div><hr></div>`: the editor's rule line.
        let mut non_blank = children.iter().filter(|child| !is_blank(child));
        if let (Some(single), None) = (non_blank.next(), non_blank.next())
            && dom::is(single, "hr")
        {
            self.blocks.push(Block {
                path: self.path.clone(),
                element,
                content: Content::Rule,
                trailing_break: false,
            });
            return;
        }
        let is_container =
            has_blocks || matches!(element.tag.as_str(), "blockquote" | "ul" | "ol" | "dl");
        if is_container {
            drop(children);
            self.next_id += 1;
            self.path.push(Container {
                id: self.next_id,
                element,
            });
            let before = self.blocks.len();
            self.container(node);
            if self.blocks.len() == before {
                // An empty container still holds an empty line, as the browser shows it.
                let mut block = Block::empty(self.path.clone());
                block.element = Element::new("");
                self.blocks.push(block);
            }
            self.path.pop();
            return;
        }
        let before = self.blocks.len();
        let mut runs = Vec::new();
        for child in &children {
            self.inline(child, &mut runs);
        }
        if self.blocks.len() > before {
            // An inline wrapper inside this block held nested blocks or tables: flush any trailing
            // loose runs and promote this element to a container around what it emitted.
            self.flush_loose(&mut runs);
            self.next_id += 1;
            let container = Container {
                id: self.next_id,
                element,
            };
            let depth = self.path.len();
            for block in &mut self.blocks[before..] {
                block.path.insert(depth, container.clone());
            }
            return;
        }
        let mut block = Block {
            path: self.path.clone(),
            element,
            content: Content::Inline(runs),
            trailing_break: false,
        };
        finish(&mut block);
        self.blocks.push(block);
    }

    fn inline(&mut self, node: &NodeRef, out: &mut Vec<Run>) {
        if let Some(contents) = dom::text(node) {
            let text = contents.replace('\u{FEFF}', "");
            if !text.is_empty() {
                out.push(Run::text(text, self.marks.clone()));
            }
        } else if let Some(tag) = dom::tag(node) {
            if DROPPED_TAGS.contains(&tag) {
                return;
            }
            // Blocks and opaque elements (like `<table>` or `<hr>`) nested inside inline wrappers
            // emit as real blocks with the inline marks distributed onto them rather than pushing
            // block/table tags into `Run::marks`.
            if is_block(tag) || OPAQUE_TAGS.contains(&tag) {
                self.flush_loose(out);
                self.block(node);
                return;
            }
            match tag {
                "br" => out.push(Run {
                    piece: Piece::Break,
                    marks: self.marks.clone(),
                }),
                "img" => out.push(Run {
                    piece: Piece::Image(element_of(node, false)),
                    marks: self.marks.clone(),
                }),
                "wbr" => {}
                _ => {
                    let element = element_of(node, self.paste);
                    let pushed = !element.tag.is_empty()
                        && !(self.paste && element.is("span") && element.attrs.is_empty());
                    if pushed {
                        self.marks.push(element);
                    }
                    for child in dom::children(node) {
                        self.inline(&child, out);
                    }
                    if pushed {
                        self.marks.pop();
                    }
                }
            }
        }
    }
}

fn is_block(tag: &str) -> bool {
    BLOCK_TAGS.contains(&tag)
}

fn is_blank(node: &NodeRef) -> bool {
    dom::text(node).is_some_and(|contents| contents.chars().all(char::is_whitespace))
}

/// The element `node` is, with the editor's tag normalization (`strong` → `b`, `em` → `i`, `ins`
/// → `u`, `strike` → `s`) and, for pastes, the attribute cleanup.
fn element_of(node: &NodeRef, paste: bool) -> Element {
    let Some(raw_tag) = dom::tag(node) else {
        return Element::new("");
    };
    let tag = match raw_tag {
        "strong" => "b",
        "em" => "i",
        "ins" => "u",
        "strike" => "s",
        other => other,
    };
    let mut element = Element::new(tag);
    element.attrs = dom::attrs(node);
    if paste {
        clean_pasted(&mut element);
    }
    element
}

/// The paste's attribute rules. Theme classes and `tabindex` go, and non-table elements
/// keep only `color`, bold `font-weight`, `font-style` and `text-decoration` in their style.
fn clean_pasted(element: &mut Element) {
    const TABLE: [&str; 8] = [
        "table", "colgroup", "col", "tbody", "tr", "td", "thead", "th",
    ];
    element.attrs.retain(|(name, _)| {
        !matches!(
            name.as_str(),
            "tabindex" | "autoplay" | "data-slate-fragment" | "id" | "contenteditable"
        )
    });
    if let Some(class) = element.attr("class") {
        let kept: Vec<&str> = class
            .split_whitespace()
            .filter(|class| {
                !class.starts_with("Theme-")
                    && !matches!(*class, "ShadowHTML" | "Exo-quoting-ellipsis" | "exo-date")
            })
            .collect();
        let kept = kept.join(" ");
        if kept.is_empty() {
            element.attrs.retain(|(name, _)| name != "class");
        } else {
            element.set_attr("class", &kept);
        }
    }
    if TABLE.contains(&element.tag.as_str()) {
        return;
    }
    let style = element.attr("style").unwrap_or_default().to_owned();
    {
        let mut kept: Vec<String> = Style::parse(&style)
            .iter()
            .filter_map(|(property, value, important)| {
                let bang = if important { " !important" } else { "" };
                match property {
                    "color" => Some(format!("color:{value}{bang}")),
                    "font-style" if value != "normal" => Some(format!("font-style:{value}{bang}")),
                    "font-weight"
                        if matches!(value, "bold" | "bolder")
                            || value.parse::<u16>().is_ok_and(|weight| weight > 500) =>
                    {
                        Some(format!("font-weight:700{bang}"))
                    }
                    property if property.starts_with("text-decoration") => {
                        Some(format!("{property}:{value}{bang}"))
                    }
                    _ => None,
                }
            })
            .collect();
        // Pasted paragraphs lose their margins (`P` and `.MsoNormal` get `margin:0px`).
        if element.is("p")
            || element
                .attr("class")
                .is_some_and(|class| class.contains("MsoNormal"))
        {
            kept.push("margin:0px".to_owned());
        }
        if kept.is_empty() {
            element.attrs.retain(|(name, _)| name != "style");
        } else {
            element.set_attr("style", &kept.join(";"));
        }
    }
}

/// Collapses whitespace as CSS lays it out, and moves the closing placeholder `<br>` into the
/// block's flag. Returns whether anything visible is left.
fn finish(block: &mut Block) -> bool {
    let code = block.in_code();
    let Content::Inline(runs) = &mut block.content else {
        return true;
    };
    if code {
        *runs = code_lines(std::mem::take(runs));
    } else {
        collapse_whitespace(runs);
    }
    if matches!(
        runs.last(),
        Some(Run {
            piece: Piece::Break,
            ..
        })
    ) {
        runs.pop();
        block.trailing_break = true;
    }
    crate::model::normalize(runs);
    !runs.is_empty()
}

/// Lays out `white-space: pre-wrap` (a `pre`), where white space stays and each `\n` is a line
/// break.
fn code_lines(runs: Vec<Run>) -> Vec<Run> {
    let mut out = Vec::with_capacity(runs.len());
    for run in runs {
        let Piece::Text(text) = &run.piece else {
            out.push(run);
            continue;
        };
        for (index, line) in text.replace("\r\n", "\n").split('\n').enumerate() {
            if index > 0 {
                out.push(Run {
                    piece: Piece::Break,
                    marks: run.marks.clone(),
                });
            }
            if !line.is_empty() {
                out.push(Run::text(line, run.marks.clone()));
            }
        }
    }
    out
}

/// Lays out `white-space: normal`, where runs of spaces, tabs and newlines become one space and
/// none start or end a line. No-break spaces stay.
fn collapse_whitespace(runs: &mut [Run]) {
    let mut at_line_start = true;
    for run in runs.iter_mut() {
        match &mut run.piece {
            Piece::Text(text) => {
                let mut out = String::with_capacity(text.len());
                for ch in text.chars() {
                    if matches!(ch, ' ' | '\t' | '\n' | '\r' | '\x0C') {
                        if !at_line_start && !out.ends_with(' ') {
                            out.push(' ');
                        }
                    } else {
                        out.push(ch);
                        at_line_start = false;
                    }
                }
                *text = out;
            }
            Piece::Break => at_line_start = true,
            Piece::Image(_) => at_line_start = false,
        }
    }
    // A space before a break or the end doesn't show.
    let mut at_line_end = true;
    for run in runs.iter_mut().rev() {
        match &mut run.piece {
            Piece::Text(text) => {
                if at_line_end {
                    let trimmed = text.trim_end_matches(' ').len();
                    text.truncate(trimmed);
                }
                if !text.is_empty() {
                    at_line_end = false;
                }
            }
            Piece::Break => at_line_end = true,
            Piece::Image(_) => at_line_end = false,
        }
    }
    // Spaces that end one text run and start the next collapse to one.
    for index in 1..runs.len() {
        let previous_ends_space =
            matches!(&runs[index - 1].piece, Piece::Text(text) if text.ends_with(' '));
        if previous_ends_space && let Piece::Text(text) = &mut runs[index].piece {
            let trimmed = text.trim_start_matches(' ').to_owned();
            *text = trimmed;
        }
    }
}

fn text_of(node: &NodeRef) -> String {
    let mut out = String::new();
    let mut stack = vec![node.clone()];
    while let Some(current) = stack.pop() {
        if let Some(text) = dom::text(&current) {
            out.push_str(&text);
        } else {
            if dom::is(&current, "tr") && !out.is_empty() {
                out.push('\n');
            }
            stack.extend(dom::children(&current).into_iter().rev());
        }
    }
    out.split('\n')
        .map(|line| line.split_whitespace().collect::<Vec<_>>().join(" "))
        .filter(|line| !line.is_empty())
        .collect::<Vec<_>>()
        .join("\n")
}

/// `html` parsed as a document and written back, which is how an outgoing message wraps each part
/// (`<html><head></head><body>…</body></html>`).
pub fn as_document(html: &str) -> String {
    let document = dom::parse(html);
    let out = dom::element_children(&document)
        .first()
        .map_or_else(String::new, dom::outer_html);
    dom::drop_tree(document);
    out
}

/// [`as_document`] with its inline images pointed at their Content-IDs, as the quote appended to a
/// sent message is. A `src-cid` image's `src` becomes `cid:{id}`, and the image loses Exo's class.
pub(crate) fn as_sent_document(html: &str) -> String {
    let document = dom::parse(html);
    for node in dom::descendants(&document) {
        if let Some(tag) = dom::tag(&node) {
            dom::clear_template_contents(&node);
            if crate::outgoing::CID_ATTRS
                .iter()
                .any(|(attr, _)| dom::has_attr(&node, attr))
            {
                let mut element = Element {
                    tag: tag.to_owned(),
                    attrs: dom::attrs(&node),
                };
                crate::outgoing::clean_image(&mut element);
                dom::set_attrs(&node, &element.attrs);
            }
        }
    }
    let out = dom::element_children(&document)
        .first()
        .map_or_else(String::new, dom::outer_html);
    dom::drop_tree(document);
    out
}

/// `html` (a kept table's markup) with each `<img>` handed to `image` to rewrite. A kept table's
/// drawing loads its images from the result (`view::table`).
pub(crate) fn map_images(html: &str, mut image: impl FnMut(&mut Element)) -> String {
    let (document, body) = fragment(html);
    for node in dom::select(&body, |node| dom::is(node, "img")) {
        let mut element = Element {
            tag: "img".to_owned(),
            attrs: dom::attrs(&node),
        };
        image(&mut element);
        dom::set_attrs(&node, &element.attrs);
    }
    let out = dom::inner_html(&body);
    dom::drop_tree(document);
    out
}

/// `html` (a kept table) inside the containers of `path`, outermost first, as the editor's DOM
/// has it, for what the table inherits from them (a quote's grey). The containers' boxes (margins,
/// padding, a quote's bar) belong to the editor's own layout, so they're emptied here.
pub(crate) fn in_containers(path: &[Container], html: &str) -> String {
    const EMPTY: &str =
        "margin:0 !important;padding:0 !important;border:0 !important;background:none !important";
    let mut out = String::new();
    for container in path {
        let mut element = container.element.clone();
        let style = element
            .attr("style")
            .map_or_else(|| EMPTY.to_owned(), |style| format!("{style};{EMPTY}"));
        element.set_attr("style", &style);
        open_tag(&element, &mut out);
    }
    out.push_str(html);
    for container in path.iter().rev() {
        close_tag(&container.element, &mut out);
    }
    out
}

// Writing

/// Writes `doc` as the draft body.
pub fn write(doc: &Document) -> String {
    let mut out = String::new();
    write_blocks(&doc.blocks, &mut out);
    out
}

/// Writes `blocks` in their containers. A container opens before its first block and closes
/// after its last. Iterative, so editing can nest containers as deep as it likes.
pub(crate) fn write_blocks(blocks: &[Block], out: &mut String) {
    let mut open: Vec<&Container> = Vec::new();
    for block in blocks {
        let shared = open
            .iter()
            .zip(&block.path)
            .take_while(|(open, container)| open.id == container.id)
            .count();
        for container in open.drain(shared..).rev() {
            close_tag(&container.element, out);
        }
        for container in &block.path[shared..] {
            open_tag(&container.element, out);
            open.push(container);
        }
        write_leaf(block, out);
    }
    for container in open.into_iter().rev() {
        close_tag(&container.element, out);
    }
}

fn write_leaf(block: &Block, out: &mut String) {
    let wrapped = !block.element.tag.is_empty();
    if wrapped {
        open_tag(&block.element, out);
    }
    match &block.content {
        Content::Rule => out.push_str("<hr>"),
        Content::Raw { html, .. } => out.push_str(html),
        Content::Inline(runs) if runs.is_empty() => {
            if block.trailing_break {
                out.push_str("<br>");
            }
        }
        Content::Inline(runs) => {
            write_runs(runs, block.in_code(), out);
            if block.trailing_break {
                out.push_str("<br>");
            }
        }
    }
    if wrapped {
        close_tag(&block.element, out);
    }
}

/// Writes inline content, opening and closing elements only where the run stacks differ. In
/// `code` (a `pre`), spaces stay as typed and a line break is a `\n`, as Chromium keeps them.
pub(crate) fn write_runs(runs: &[Run], code: bool, out: &mut String) {
    let spaces = if code {
        vec![false; runs.iter().map(|run| run.piece.len()).sum()]
    } else {
        space_kinds(runs)
    };
    let mut open: Vec<&Element> = Vec::new();
    let mut position = 0;
    for run in runs {
        let shared = open
            .iter()
            .zip(&run.marks)
            .take_while(|(a, b)| **a == *b)
            .count();
        while open.len() > shared {
            close_tag(open.pop().expect("longer than shared"), out);
        }
        for mark in &run.marks[shared..] {
            open_tag(mark, out);
            open.push(mark);
        }
        match &run.piece {
            Piece::Text(text) => {
                for ch in text.chars() {
                    match ch {
                        ' ' if spaces[position] => out.push_str("&nbsp;"),
                        '\u{A0}' => out.push_str("&nbsp;"),
                        '&' => out.push_str("&amp;"),
                        '<' => out.push_str("&lt;"),
                        '>' => out.push_str("&gt;"),
                        ch => out.push(ch),
                    }
                    position += 1;
                }
            }
            Piece::Break => {
                out.push_str(if code { "\n" } else { "<br>" });
                position += 1;
            }
            Piece::Image(image) => {
                open_tag(image, out);
                position += 1;
            }
        }
    }
    while let Some(mark) = open.pop() {
        close_tag(mark, out);
    }
}

/// For each position of `runs`, whether a space there is written as `&nbsp;`, as Chromium's
/// editing leaves typed spaces. A lone space between words stays a space. Longer runs alternate,
/// starting with a no-break space, and a run at the start or end of a line starts or ends with
/// one.
pub(crate) fn space_kinds(runs: &[Run]) -> Vec<bool> {
    let chars: Vec<Option<char>> = runs
        .iter()
        .flat_map(|run| match &run.piece {
            Piece::Text(text) => text.chars().map(Some).collect::<Vec<_>>(),
            Piece::Break => vec![Some('\n')],
            Piece::Image(_) => vec![None],
        })
        .collect();
    let mut out = vec![false; chars.len()];
    let mut index = 0;
    while index < chars.len() {
        if chars[index] != Some(' ') {
            index += 1;
            continue;
        }
        let start = index;
        while index < chars.len() && chars[index] == Some(' ') {
            index += 1;
        }
        let at_start = start == 0 || chars[start - 1] == Some('\n');
        let at_end = index == chars.len() || chars[index] == Some('\n');
        let len = index - start;
        if len == 1 && !at_start && !at_end {
            continue;
        }
        for (offset, slot) in out[start..index].iter_mut().enumerate() {
            *slot = offset % 2 == 0;
        }
        if at_end {
            out[index - 1] = true;
        }
    }
    out
}

fn open_tag(element: &Element, out: &mut String) {
    out.push('<');
    out.push_str(&element.tag);
    for (name, value) in &element.attrs {
        out.push(' ');
        out.push_str(name);
        out.push_str("=\"");
        dom::push_inner_html_attr(out, value);
        out.push('"');
    }
    out.push('>');
}

fn close_tag(element: &Element, out: &mut String) {
    if matches!(
        element.tag.as_str(),
        "br" | "img" | "hr" | "wbr" | "input" | "col" | "area" | "source"
    ) {
        return;
    }
    out.push_str("</");
    out.push_str(&element.tag);
    out.push('>');
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pasted_inline_styles_ignore_semicolons_inside_urls_and_quotes() {
        let doc = parse_paste(
            r#"<span style="background: url(data:image/png;color:blue;base64,iVBORw0K); font-family: 'Foo;color:green'; color: red; font-weight: 600">ok</span>"#,
        );
        assert_eq!(
            write(&doc),
            r#"<span style="color:red;font-weight:700">ok</span>"#
        );
    }

    #[test]
    fn inline_wrapper_around_blocks_distributes_marks_without_flattening() {
        let doc = parse(r#"<a href="https://example.com"><div>Line 1</div><div>Line 2</div></a>"#);
        assert_eq!(doc.blocks.len(), 2);
        assert_eq!(doc.blocks[0].text(), "Line 1");
        assert_eq!(doc.blocks[1].text(), "Line 2");
        assert_eq!(
            write(&doc),
            r#"<div><a href="https://example.com">Line 1</a></div><div><a href="https://example.com">Line 2</a></div>"#
        );

        // Also when nested inside an enclosing block element.
        let nested = parse(
            r#"<div class="card"><a href="https://example.com"><div>Line 1</div><div>Line 2</div></a></div>"#,
        );
        assert_eq!(nested.blocks.len(), 2);
        assert_eq!(
            write(&nested),
            r#"<div class="card"><div><a href="https://example.com">Line 1</a></div><div><a href="https://example.com">Line 2</a></div></div>"#
        );
    }

    #[test]
    fn inline_wrapper_around_table_keeps_table_opaque() {
        let doc = parse("<span><table><tbody><tr><td>cell</td></tr></tbody></table></span>");
        assert_eq!(doc.blocks.len(), 1);
        assert!(matches!(doc.blocks[0].content, Content::Raw { .. }));
        assert_eq!(
            write(&doc),
            "<table><tbody><tr><td>cell</td></tr></tbody></table>"
        );

        let wrapped =
            parse("<div><span><table><tbody><tr><td>cell</td></tr></tbody></table></span></div>");
        assert_eq!(wrapped.blocks.len(), 1);
        assert!(matches!(wrapped.blocks[0].content, Content::Raw { .. }));
        assert_eq!(
            write(&wrapped),
            "<div><table><tbody><tr><td>cell</td></tr></tbody></table></div>"
        );
    }
}

//! The `text/plain` alternative of a sent message, derived from its HTML in the style of
//! `jaytaylor/html2text`: blocks on their own lines, paragraphs and headings set off by a blank
//! line, links as `text ( url )`, list items as `* `, quotes as `> `.

use exo_mail::dom::{self, NodeRef};

/// `html` as plain text, with CRLF line ends.
pub(crate) fn from_html(html: &str) -> String {
    let document = dom::parse(html);
    let mut out = Writer::default();
    out.walk(&document);
    dom::drop_tree(document);
    out.finish()
}

/// The text of `html` that the user wrote, leaving out quotes, the signature and quoted messages.
pub(crate) fn authored(html: &str) -> String {
    let document = dom::parse(html);
    let mut out = Writer {
        authored: true,
        ..Writer::default()
    };
    out.walk(&document);
    dom::drop_tree(document);
    out.finish().replace("\r\n", "\n")
}

/// Classes whose contents the user didn't write: the revealed signature and quote, as Exo
/// marks them, and Gmail's.
const NOT_AUTHORED: [&str; 4] = [
    "sh-signature",
    "gmail_signature",
    "sh-quoted-content",
    "gmail_quote",
];

#[derive(Default)]
struct Writer {
    /// Leaves out what the user didn't write ([`authored`]).
    authored: bool,
    lines: Vec<String>,
    line: String,
    /// Blank lines wanted before the next text.
    gap: usize,
    /// `> ` levels.
    quote: usize,
    /// Inside `<pre>`, where whitespace is kept.
    pre: bool,
}

enum Step {
    Visit(NodeRef),
    AfterAnchor { start: usize, href: Option<String> },
    Block(usize),
    ExitQuote,
    ExitPre(bool),
    Space,
}

impl Writer {
    fn walk(&mut self, root: &NodeRef) {
        let push_children = |stack: &mut Vec<Step>, node: &NodeRef| {
            for child in dom::children(node).into_iter().rev() {
                stack.push(Step::Visit(child));
            }
        };
        let mut stack = vec![Step::Visit(root.clone())];
        while let Some(step) = stack.pop() {
            match step {
                Step::AfterAnchor { start, href } => {
                    let text = self
                        .flat()
                        .get(start..)
                        .unwrap_or_default()
                        .trim()
                        .to_owned();
                    if let Some(href) = href {
                        let href = href.strip_prefix("mailto:").unwrap_or(&href);
                        if text.is_empty() {
                            self.text(href);
                        } else if text != href {
                            self.text(&format!(" ( {href} )"));
                        }
                    }
                }
                Step::Block(blank) => self.block(blank),
                Step::ExitQuote => {
                    self.quote -= 1;
                    self.block(1);
                }
                Step::ExitPre(was_pre) => {
                    self.pre = was_pre;
                    self.block(1);
                }
                Step::Space => self.text(" "),
                Step::Visit(node) => {
                    if let Some(contents) = dom::text(&node) {
                        self.text(&contents);
                        continue;
                    }
                    let Some(tag) = dom::tag(&node) else {
                        push_children(&mut stack, &node);
                        continue;
                    };
                    if self.authored
                        && (tag == "blockquote"
                            || NOT_AUTHORED
                                .iter()
                                .any(|class| dom::has_class(&node, class)))
                    {
                        continue;
                    }
                    match tag {
                        "head" | "style" | "script" | "title" => {}
                        "br" => self.newline(),
                        "hr" => {
                            self.block(1);
                            self.text("--------");
                            self.block(1);
                        }
                        "img" => {
                            if let Some(alt) =
                                dom::attr(&node, "alt").filter(|alt| !alt.trim().is_empty())
                            {
                                self.text(&alt);
                            }
                        }
                        "a" => {
                            let start = self.flat().len();
                            let href = dom::attr(&node, "href").filter(|href| !href.is_empty());
                            stack.push(Step::AfterAnchor { start, href });
                            push_children(&mut stack, &node);
                        }
                        "p" | "h1" | "h2" | "h3" | "h4" | "h5" | "h6" | "table" => {
                            self.block(2);
                            stack.push(Step::Block(2));
                            push_children(&mut stack, &node);
                        }
                        "blockquote" => {
                            self.block(1);
                            self.quote += 1;
                            stack.push(Step::ExitQuote);
                            push_children(&mut stack, &node);
                        }
                        "pre" => {
                            self.block(1);
                            let was_pre = self.pre;
                            self.pre = true;
                            stack.push(Step::ExitPre(was_pre));
                            push_children(&mut stack, &node);
                        }
                        "li" => {
                            self.block(1);
                            self.text("* ");
                            stack.push(Step::Block(1));
                            push_children(&mut stack, &node);
                        }
                        "div" | "ul" | "ol" | "tr" | "section" | "article" | "header"
                        | "footer" | "body" | "html" => {
                            self.block(1);
                            stack.push(Step::Block(1));
                            push_children(&mut stack, &node);
                        }
                        "td" | "th" => {
                            stack.push(Step::Space);
                            push_children(&mut stack, &node);
                        }
                        _ => push_children(&mut stack, &node),
                    }
                }
            }
        }
    }

    /// What's on the current line, for links to see their text.
    fn flat(&self) -> &str {
        &self.line
    }

    fn text(&mut self, text: &str) {
        let text = text.replace('\u{a0}', " ");
        if self.pre {
            for (index, part) in text.split('\n').enumerate() {
                if index > 0 {
                    self.newline();
                }
                self.push(part);
            }
            return;
        }
        let mut collapsed = String::with_capacity(text.len());
        let mut space = false;
        for ch in text.chars() {
            if ch.is_whitespace() {
                space = true;
            } else {
                if space
                    && (!collapsed.is_empty() || !self.line.is_empty() && !self.line.ends_with(' '))
                {
                    collapsed.push(' ');
                }
                space = false;
                collapsed.push(ch);
            }
        }
        if space && !collapsed.is_empty() {
            collapsed.push(' ');
        }
        if !collapsed.is_empty() {
            self.push(&collapsed);
        }
    }

    fn push(&mut self, text: &str) {
        if self.line.is_empty() {
            for _ in 0..self.gap {
                self.lines.push(self.prefix().trim_end().to_owned());
            }
            self.gap = 0;
        }
        self.line.push_str(text);
    }

    fn prefix(&self) -> String {
        "> ".repeat(self.quote)
    }

    fn newline(&mut self) {
        let line = std::mem::take(&mut self.line);
        self.lines
            .push(format!("{}{}", self.prefix(), line.trim_end()));
    }

    /// Ends the current line, if it has text, and asks for `blank - 1` blank lines before more.
    fn block(&mut self, blank: usize) {
        if !self.line.trim().is_empty() {
            self.newline();
        } else {
            self.line.clear();
        }
        if !self.lines.is_empty() {
            self.gap = self.gap.max(blank - 1);
        }
    }

    fn finish(mut self) -> String {
        if !self.line.trim().is_empty() {
            self.newline();
        }
        while self.lines.last().is_some_and(|line| line.trim().is_empty()) {
            self.lines.pop();
        }
        self.lines.join("\r\n")
    }
}

#[cfg(test)]
mod tests {
    use super::{authored, from_html};

    #[test]
    fn blocks_become_lines_and_links_show_their_address() {
        let html = "<div><div><div><html><head></head><body><div>Hi Ann,</div><div><br></div><div>See <a href=\"https://x.com/a\">the doc</a>.</div></body></html></div><div></div><br/></div></div>";
        assert_eq!(
            from_html(html),
            "Hi Ann,\r\n\r\nSee the doc ( https://x.com/a )."
        );
    }

    #[test]
    fn quotes_and_lists_are_marked() {
        let html = "<div>On Tue, Ann wrote:<br><blockquote><div>First</div><div>Second</div></blockquote><ul><li>one</li><li>two</li></ul></div>";
        assert_eq!(
            from_html(html),
            "On Tue, Ann wrote:\r\n> First\r\n> Second\r\n* one\r\n* two"
        );
    }

    #[test]
    fn authored_text_leaves_out_revealed_signatures_and_quotes() {
        let html = "<div>See attached</div><div class=\"sh-signature\"><div>Maya</div></div><div class=\"sh-quoted-content\"><div>the deck</div></div>";
        assert_eq!(authored(html), "See attached");
    }

    #[test]
    fn deeply_nested_html_does_not_overflow_the_stack() {
        let deep = format!("{}deep{}", "<div>".repeat(5000), "</div>".repeat(5000));
        assert_eq!(from_html(&deep), "deep");
    }
}

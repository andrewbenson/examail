//! An event's description as the event details show it. Calendar's boilerplate lines are dropped
//! and newlines become `<br>`s. The result is sanitized like a message (exo-mail's sanitizer, with
//! `<style>` forbidden), then read into lines of text and links: blocks and `<br>`s break lines,
//! list items are indented 32px behind a bullet or number, and `<b>`/`<strong>` are bold.

use std::sync::LazyLock;

use egui::{Id, Rect, Sense, Ui};
use exo_mail::dom::{self, NodeRef};
use regex::Regex;

/// A line of the description: runs of text, indented for a list.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Line {
    pub indent: f32,
    pub runs: Vec<Run>,
}

/// Text, a link when `link` is set, bold when `bold`.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Run {
    pub text: String,
    pub link: Option<String>,
    pub bold: bool,
}

/// Splits plain `text` into [`Run`]s with any URLs marked as links.
#[must_use]
pub(crate) fn linkify(text: &str) -> Vec<Run> {
    let mut runs = Vec::new();
    let mut at = 0;
    for (range, url) in exo_mail::links(text) {
        if range.start > at {
            runs.push(Run {
                text: text[at..range.start].to_owned(),
                ..Run::default()
            });
        }
        runs.push(Run {
            text: text[range.clone()].to_owned(),
            link: Some(url),
            bold: false,
        });
        at = range.end;
    }
    if at < text.len() {
        runs.push(Run {
            text: text[at..].to_owned(),
            ..Run::default()
        });
    }
    runs
}

/// Registers `rect` as an interactive link box for `href` with a pointing-hand hover cursor.
pub(crate) fn interact_link(ui: &Ui, rect: Rect, id: Id, href: &str) -> egui::Response {
    exo_ui::link_box(ui.ctx(), rect, href);
    ui.interact(rect, id, Sense::click())
        .on_hover_cursor(egui::CursorIcon::PointingHand)
}

/// How far a list sits in: `ul, ol { margin-left: 32px }`.
const LIST_INDENT: f32 = 32.0;

/// Boilerplate that descriptions carry: edit warnings, event links and divider lines.
static USELESS: LazyLock<[Regex; 5]> = LazyLock::new(|| {
    [
        r"Please do not edit this section of the description.",
        r"Do not edit this section of the description.",
        r"View your event at http(.+)($|\s)",
        r"(?m)^-::(~:{1,2})+-$",
        r"(?m)[-_.]{50,}",
    ]
    .map(|pattern| Regex::new(pattern).expect("valid regex"))
});

/// `description` without Calendar's boilerplate, as an invite's About has it. The first three
/// patterns drop their first match and the others drop every match, then the result is trimmed.
pub(crate) fn clean(description: &str) -> String {
    let mut cleaned = description.to_owned();
    for (at, pattern) in USELESS.iter().enumerate() {
        cleaned = if at < 3 {
            pattern.replace(&cleaned, "")
        } else {
            pattern.replace_all(&cleaned, "")
        }
        .into_owned();
    }
    cleaned.trim().to_owned()
}

/// The lines `description` shows as; none when it has no text.
pub fn lines(description: &str) -> Vec<Line> {
    let mut cleaned = description.to_owned();
    // Newlines become `<br/>` inside the loop, after the first pattern.
    for pattern in USELESS.iter() {
        cleaned = pattern.replace(&cleaned, "").replace('\n', "<br/>");
    }
    let html = exo_mail::sanitize_description(cleaned.trim());
    let document = dom::parse(&html);
    let mut reader = Reader::default();
    reader.walk(&document);
    dom::drop_tree(document);
    reader.flush();
    while reader.lines.last().is_some_and(|line| line.runs.is_empty()) {
        reader.lines.pop();
    }
    if reader
        .lines
        .iter()
        .all(|line| line.runs.iter().all(|run| run.text.trim().is_empty()))
    {
        return Vec::new();
    }
    reader.lines
}

/// What the text being read is inside.
#[derive(Clone, Default)]
struct Style {
    link: Option<String>,
    bold: bool,
    indent: f32,
}

#[derive(Default)]
struct Reader {
    lines: Vec<Line>,
    current: Line,
    /// The lists being read, each with whether it's ordered and how many items it has so far.
    lists: Vec<(bool, usize)>,
}

enum Step {
    Visit(NodeRef, Style),
    AfterElement { is_list: bool, block: bool },
}

impl Reader {
    fn walk(&mut self, root: &NodeRef) {
        let mut stack = vec![Step::Visit(root.clone(), Style::default())];
        while let Some(step) = stack.pop() {
            match step {
                Step::AfterElement { is_list, block } => {
                    if is_list {
                        self.lists.pop();
                    }
                    if block {
                        self.flush();
                    }
                }
                Step::Visit(node, style) => {
                    if let Some(contents) = dom::text(&node) {
                        self.text(&contents, &style);
                        continue;
                    }
                    let Some(tag) = dom::tag(&node) else {
                        for child in dom::children(&node).into_iter().rev() {
                            stack.push(Step::Visit(child, style.clone()));
                        }
                        continue;
                    };
                    let mut inner = style.clone();
                    match tag {
                        "br" => {
                            self.flush_line(&style);
                            continue;
                        }
                        "a" => inner.link = dom::attr(&node, "href"),
                        "b" | "strong" => inner.bold = true,
                        "ul" | "ol" => inner.indent += LIST_INDENT,
                        "script" | "style" | "head" | "title" => continue,
                        _ => {}
                    }
                    let block = matches!(
                        tag,
                        "p" | "div"
                            | "li"
                            | "ul"
                            | "ol"
                            | "h1"
                            | "h2"
                            | "h3"
                            | "h4"
                            | "h5"
                            | "h6"
                            | "blockquote"
                            | "pre"
                            | "table"
                            | "tr"
                    );
                    if block {
                        self.flush();
                    }
                    let is_list = matches!(tag, "ul" | "ol");
                    if is_list {
                        self.lists.push((tag == "ol", 0));
                    }
                    if tag == "li" {
                        let marker = match self.lists.last_mut() {
                            Some((true, count)) => {
                                *count += 1;
                                format!("{count}. ")
                            }
                            _ => "• ".to_owned(),
                        };
                        self.current.indent = inner.indent;
                        self.current.runs.push(Run {
                            text: marker,
                            ..Run::default()
                        });
                    }
                    if is_list || block {
                        stack.push(Step::AfterElement { is_list, block });
                    }
                    for child in dom::children(&node).into_iter().rev() {
                        stack.push(Step::Visit(child, inner.clone()));
                    }
                }
            }
        }
    }

    /// Text, its whitespace collapsed (`white-space: normal`).
    fn text(&mut self, text: &str, style: &Style) {
        let mut collapsed = String::new();
        let mut space = self
            .current
            .runs
            .last()
            .is_none_or(|run| run.text.ends_with(' '));
        for c in text.chars() {
            if c.is_whitespace() {
                if !space {
                    collapsed.push(' ');
                }
                space = true;
            } else {
                collapsed.push(c);
                space = false;
            }
        }
        if collapsed.is_empty() {
            return;
        }
        if self.current.runs.is_empty() {
            self.current.indent = style.indent;
        }
        match self.current.runs.last_mut() {
            Some(run) if run.link == style.link && run.bold == style.bold => {
                run.text.push_str(&collapsed)
            }
            _ => self.current.runs.push(Run {
                text: collapsed,
                link: style.link.clone(),
                bold: style.bold,
            }),
        }
    }

    /// Ends the line, even an empty one (a `<br>`).
    fn flush_line(&mut self, style: &Style) {
        let mut line = std::mem::take(&mut self.current);
        if line.runs.is_empty() {
            line.indent = style.indent;
        }
        self.lines.push(line);
    }

    /// Ends the line if it has text (a block's edge).
    fn flush(&mut self) {
        if !self.current.runs.is_empty() {
            self.lines.push(std::mem::take(&mut self.current));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn texts(lines: &[Line]) -> Vec<String> {
        lines
            .iter()
            .map(|line| line.runs.iter().map(|run| run.text.as_str()).collect())
            .collect()
    }

    #[test]
    fn plain_text_keeps_its_lines() {
        let lines = lines("Agenda:\n- metrics\n\nThanks");
        assert_eq!(texts(&lines), ["Agenda:", "- metrics", "", "Thanks"]);
    }

    #[test]
    fn html_reads_into_blocks_links_and_lists() {
        let lines = lines(
            "<p>Join <a href=\"https://example.com/x\">here</a></p><ul><li>One</li><li><b>Two</b></li></ul><ol><li>A</li><li>B</li></ol>",
        );
        assert_eq!(
            texts(&lines),
            ["Join here", "• One", "• Two", "1. A", "2. B"]
        );
        assert_eq!(
            lines[0].runs[1].link.as_deref(),
            Some("https://example.com/x")
        );
        assert_eq!(lines[1].indent, LIST_INDENT);
        assert!(lines[2].runs[1].bold);
    }

    #[test]
    fn scripts_styles_and_unsafe_links_are_dropped() {
        let lines = lines("<style>p{}</style><script>x()</script><a href=\"javascript:x()\">y</a>");
        assert_eq!(texts(&lines), ["y"]);
        assert_eq!(lines[0].runs[0].link, None);
    }

    #[test]
    fn boilerplate_alone_is_no_description() {
        assert!(lines("Please do not edit this section of the description.\n\n").is_empty());
        // Newlines become `<br/>`s after the first pattern, so the line-anchored ones only match
        // a whole description.
        assert_eq!(texts(&lines("Agenda\n-::~:~::-")), ["Agenda", "-::~:~::-"]);
    }

    #[test]
    fn deeply_nested_html_does_not_overflow_the_stack() {
        let deep = format!("{}Agenda{}", "<div>".repeat(5000), "</div>".repeat(5000));
        assert_eq!(texts(&lines(&deep)), ["Agenda"]);
    }
}

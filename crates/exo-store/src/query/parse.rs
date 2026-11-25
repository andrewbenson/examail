//! Lexer, leaf token parser, AST normalization, and split/bulk validation.

use std::collections::VecDeque;
use std::sync::LazyLock;

use regex::Regex;

use super::{Invalidity, Kind, LabelIndex, Node, Token, label_slug};

/// The tree of `raw`, before a split constrains it or anything simplifies.
pub(super) fn parse_tree(raw: &str) -> Node {
    let mut tokens: VecDeque<String> = TOKEN
        .find_iter(raw)
        .map(|token| token.as_str().to_owned())
        .collect();
    parse_more(&mut tokens, Mode::And, "", true).unwrap_or_else(|| Node::And(Vec::new(), false))
}

/// The tokens of a query: `- ( ) { } !`, a quoted phrase, a `field:` prefix or a word.
static TOKEN: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r#"[-(){}!]|"[^"]*(?:"|$)|[a-z0-9]+:|[^\s,;"\-(){}!][^\s,;"(){}]*"#)
        .expect("valid regex")
});

/// Makes a group node from its children and negation (`Node::And` or `Node::Or`).
type Group = fn(Vec<Node>, bool) -> Node;

#[derive(Clone, Copy, PartialEq, Eq)]
enum Mode {
    And,
    Or,
    /// One node, for the operand after `OR`/`AND`.
    Single,
}

/// Parses `tokens` into a group of `mode` (or one node, for [`Mode::Single`]) up to its closing
/// bracket, which the top level ignores. A value without a `field:` of its own takes
/// `parent_prefix`.
fn parse_more(
    tokens: &mut VecDeque<String>,
    mode: Mode,
    parent_prefix: &str,
    top: bool,
) -> Option<Node> {
    let mut negated = false;
    let mut prefix = String::new();
    let mut children: Vec<Node> = Vec::new();
    while let Some(token) = tokens.pop_front() {
        if token == "-" || token == "!" || token.eq_ignore_ascii_case("not") {
            negated = !negated;
        } else if token.ends_with(':') {
            prefix.push_str(&token);
        } else if token == ")" || token == "}" {
            if !top {
                break;
            }
        } else if token.eq_ignore_ascii_case("or") {
            if mode == Mode::And && !children.is_empty() {
                if !matches!(children.last(), Some(Node::Or(..))) {
                    let last = children.pop().expect("children isn't empty");
                    children.push(Node::Or(vec![last], negated));
                }
                if let Some(next) = parse_more(tokens, Mode::Single, parent_prefix, false)
                    && let Some(Node::Or(group, _)) = children.last_mut()
                {
                    group.push(next);
                }
            }
        } else if token.eq_ignore_ascii_case("and") {
            if mode == Mode::Or && !children.is_empty() {
                if !matches!(children.last(), Some(Node::And(..))) {
                    children = vec![Node::And(
                        vec![Node::Or(std::mem::take(&mut children), false)],
                        false,
                    )];
                }
                if let Some(next) = parse_more(tokens, Mode::Single, parent_prefix, false)
                    && let Some(Node::And(group, _)) = children.last_mut()
                {
                    group.push(next);
                }
            }
        } else if token == "{" || token == "(" {
            let inner_prefix = if prefix.is_empty() {
                parent_prefix.to_owned()
            } else {
                prefix.clone()
            };
            let (inner, wrap): (Mode, Group) = if token == "{" {
                (Mode::Or, Node::Or)
            } else {
                (Mode::And, Node::And)
            };
            let group = parse_more(tokens, inner, &inner_prefix, false)
                .unwrap_or_else(|| wrap(Vec::new(), false));
            children.push(if negated {
                wrap(vec![group], true)
            } else {
                group
            });
            prefix.clear();
            negated = false;
        } else {
            let mut token = token;
            if let Some(rest) = token.strip_prefix('+') {
                token = format!("\"{rest}\"");
            }
            if token.starts_with('"') && !token.ends_with('"') {
                token.push('"');
            }
            let prefix_or_parent = if prefix.is_empty() {
                parent_prefix
            } else {
                prefix.as_str()
            };
            children.push(Node::Leaf(parse_leaf(
                &format!("{prefix_or_parent}{token}"),
                negated,
            )));
            prefix.clear();
            negated = false;
        }
        if mode == Mode::Single && children.len() == 1 {
            break;
        }
    }
    if !prefix.is_empty() {
        children.push(Node::Leaf(parse_leaf(&prefix, negated)));
    }
    match mode {
        Mode::Single => children.into_iter().next(),
        Mode::And => Some(Node::And(children, false)),
        Mode::Or => Some(Node::Or(children, false)),
    }
}

/// The value with its double quotes removed.
fn unquote(value: &str) -> String {
    value.replace('"', "")
}

/// Whether the value has anything but ASCII letters and digits.
fn has_non_alphanumeric(value: &str) -> bool {
    value.chars().any(|c| !c.is_ascii_alphanumeric())
}

/// Whether a yes/no value says yes, meaning it starts with `yes` or contains `true`.
fn says_yes(value: &str) -> bool {
    let lower = value.to_ascii_lowercase();
    lower.starts_with("yes") || lower.contains("true")
}

/// Parses one token, its `field:` prefix included.
pub(super) fn parse_leaf(text: &str, negated: bool) -> Token {
    static PATTERNS: LazyLock<[Regex; 14]> = LazyLock::new(|| {
        [
            r"(?i)^has:\s*attachment$",
            r"(?i)^hasattachment:\s*(yes|no|true|false)$",
            r"(?i)^has:\s*star$",
            r"(?i)^isflagged:\s*(yes|no|true|false)",
            r"(?i)^is:\s*flagged",
            r"(?i)^(filename|attachment):\s*(.*)",
            r"(?i)^(subject|list|rfc822msgid):\s*(.*)",
            r"(?i)^(after|before|older|newer):\s*(.*)",
            r"(?i)^(to|from|cc|bcc|replyto|deliveredto):\s*(.*)",
            r"(?i)^isread:\s*(yes|no|true|false)",
            r"(?i)^(in|is|label|l):\s*(.*)",
            r"(?i)^category:\s*(.*)",
            r"(?i)^autolabel:\s*(.*)",
            r"(?i)^kind:\s*(meetings)",
        ]
        .map(|pattern| Regex::new(pattern).expect("valid regex"))
    });
    let token =
        |kind: Kind, field: &str, value: Option<String>, quoted: bool, negated: bool| Token {
            kind,
            field: field.to_owned(),
            value,
            quoted,
            negated,
            text: text.to_owned(),
        };
    let starred = |negated: bool| token(Kind::Label, "is", Some("starred".into()), false, negated);
    let [
        attachment,
        has_attachment,
        star,
        flagged_yes,
        flagged,
        filename,
        header,
        after,
        contact,
        read,
        label,
        category,
        auto_label,
        meetings,
    ] = &*PATTERNS;
    let group = |pattern: &Regex, at: usize| {
        pattern
            .captures(text)
            .and_then(|captures| captures.get(at))
            .map_or_else(String::new, |found| found.as_str().to_owned())
    };

    if attachment.is_match(text) {
        return token(Kind::Attachment, "", None, false, negated);
    }
    if has_attachment.is_match(text) {
        return token(
            Kind::Attachment,
            "",
            None,
            false,
            if says_yes(&group(has_attachment, 1)) {
                negated
            } else {
                !negated
            },
        );
    }
    if star.is_match(text) {
        return starred(negated);
    }
    if flagged_yes.is_match(text) {
        return starred(if says_yes(&group(flagged_yes, 1)) {
            negated
        } else {
            !negated
        });
    }
    if flagged.is_match(text) {
        return starred(negated);
    }
    if filename.is_match(text) {
        let value = group(filename, 2);
        return token(
            Kind::Filename,
            "",
            Some(unquote(&value)),
            has_non_alphanumeric(&value),
            negated,
        );
    }
    if header.is_match(text) {
        let field = group(header, 1).to_lowercase();
        let mut value = group(header, 2);
        if (field == "list" || field == "rfc822msgid")
            && value.starts_with('<')
            && value.ends_with('>')
            && value.len() >= 2
        {
            value = value[1..value.len() - 1].to_owned();
        }
        return token(
            Kind::Header,
            &field,
            Some(unquote(&value)),
            has_non_alphanumeric(&value),
            negated,
        );
    }
    if after.is_match(text) {
        let operator = group(after, 1).to_ascii_lowercase();
        let flipped = operator == "before" || operator == "older";
        return token(
            Kind::After,
            "",
            Some(group(after, 2)),
            false,
            negated != flipped,
        );
    }
    if contact.is_match(text) {
        let value = group(contact, 2);
        let quoted = value
            .chars()
            .any(|c| !(c.is_ascii_alphanumeric() || c == '_'))
            || value.starts_with('"');
        return token(
            Kind::Contact,
            &group(contact, 1).to_lowercase(),
            Some(unquote(&value)),
            quoted,
            negated,
        );
    }
    if read.is_match(text) {
        // `isread:yes` reads as `-is:unread`.
        let negated = if says_yes(&group(read, 1)) {
            !negated
        } else {
            negated
        };
        return token(Kind::Label, "is", Some("unread".into()), false, negated);
    }
    if label.is_match(text) {
        let field = group(label, 1).to_lowercase();
        let mut value = group(label, 2);
        let mut negated = negated;
        if value.eq_ignore_ascii_case("read") {
            negated = !negated;
            value = "unread".into();
        }
        if field == "is" && value.eq_ignore_ascii_case("shared") {
            return token(Kind::Shared, "", None, false, negated);
        }
        let lower = value.to_lowercase();
        if field == "in"
            && CATEGORIES
                .iter()
                .any(|category| lower == format!("category_{category}"))
        {
            // The prefix goes whatever its case, and the name keeps its own case.
            let name = value["category_".len()..].to_owned();
            return Token {
                kind: Kind::Category,
                field: "category".into(),
                text: format!("category:{name}"),
                value: Some(name),
                quoted: false,
                negated,
            };
        }
        return token(
            Kind::Label,
            &field,
            Some(unquote(&value)),
            value.starts_with('"'),
            negated,
        );
    }
    if category.is_match(text) {
        let value = group(category, 1);
        return token(
            Kind::Category,
            "category",
            Some(unquote(&value)),
            value.starts_with('"'),
            negated,
        );
    }
    if auto_label.is_match(text) {
        let value = group(auto_label, 1);
        return token(
            Kind::AutoLabel,
            "autolabel",
            Some(unquote(&value)),
            value.starts_with('"'),
            negated,
        );
    }
    if meetings.is_match(text) {
        return token(
            Kind::Filename,
            "",
            Some("ics".into()),
            text.starts_with('"'),
            negated,
        );
    }
    token(
        Kind::Text,
        "",
        Some(unquote(text)),
        has_non_alphanumeric(text),
        negated,
    )
}

/// Gmail's categories as `in:category_…` can name them.
const CATEGORIES: [&str; 8] = [
    "primary",
    "social",
    "forums",
    "updates",
    "promotions",
    "reservations",
    "purchases",
    "personal",
];

/// Constrains a split below its top level, where an explicit `in:inbox` is dropped because the
/// top level adds its own.
pub(super) fn constrain_to_inbox(node: Node) -> Node {
    match node {
        Node::And(children, negated) => Node::And(
            children.into_iter().map(constrain_to_inbox).collect(),
            negated,
        ),
        Node::Or(children, negated) => Node::Or(
            children.into_iter().map(constrain_to_inbox).collect(),
            negated,
        ),
        Node::Leaf(token)
            if token.kind == Kind::Label && token.value().eq_ignore_ascii_case("inbox") =>
        {
            Node::Leaf(Token::empty())
        }
        leaf => leaf,
    }
}

/// Rewrites a bare address as `from:` or `to:` that address.
pub(super) fn convert_email_addresses(node: &mut Node) {
    static ADDRESS: LazyLock<Regex> = LazyLock::new(|| {
        Regex::new(r"[^\s<@]+@[a-zA-Z0-9-]+(?:\.[a-zA-Z0-9-]+)+").expect("valid regex")
    });
    let (Node::And(children, _) | Node::Or(children, _)) = node else {
        return;
    };
    for child in children {
        match child {
            Node::Leaf(token) if token.kind == Kind::Text && ADDRESS.is_match(token.value()) => {
                let value = token.value().to_owned();
                let negated = token.negated;
                *child = Node::Or(
                    vec![
                        Node::Leaf(parse_leaf(&format!("from:{value}"), false)),
                        Node::Leaf(parse_leaf(&format!("to:{value}"), false)),
                    ],
                    negated,
                );
            }
            Node::Leaf(_) => {}
            group => convert_email_addresses(group),
        }
    }
}

/// Flattens nested groups of the same kind, drops empty text and unwraps one-child groups.
pub(super) fn simplify(node: Node) -> Node {
    let (children, negated, is_and) = match node {
        Node::And(children, negated) => (children, negated, true),
        Node::Or(children, negated) => (children, negated, false),
        leaf => return leaf,
    };
    let mut queue: VecDeque<Node> = children.into();
    let mut kept = Vec::new();
    while let Some(child) = queue.pop_front() {
        let same = matches!(
            (&child, is_and),
            (Node::And(_, false), true) | (Node::Or(_, false), false)
        );
        if same {
            if let Node::And(grandchildren, _) | Node::Or(grandchildren, _) = child {
                for grandchild in grandchildren.into_iter().rev() {
                    queue.push_front(grandchild);
                }
            }
            continue;
        }
        let child = simplify(child);
        if !matches!(&child, Node::Leaf(token) if token.is_empty_text() && !token.negated) {
            kept.push(child);
        }
    }
    match kept.len() {
        0 => Node::Leaf(Token::empty()),
        1 => {
            let mut only = kept.pop().expect("one child");
            if negated {
                only.negate();
            }
            only
        }
        _ if is_and => Node::And(kept, negated),
        _ => Node::Or(kept, negated),
    }
}

/// The tokens that can't define a split (`for_splits`) or be acted on in bulk.
pub(super) fn invalid_tokens<'a>(node: &'a Node, out: &mut Vec<&'a Token>, for_splits: bool) {
    // Exo's own lists. A split can't be defined on Unread or All either.
    const UNSEARCHABLE: [&str; 9] = [
        "done",
        "other",
        "important",
        "muted",
        "snippets",
        "reminders",
        "snoozed",
        "scheduled",
        "archived",
    ];
    const NOT_SPLITTABLE: [&str; 2] = ["unread", "all"];
    match node {
        Node::And(children, _) | Node::Or(children, _) => children
            .iter()
            .for_each(|child| invalid_tokens(child, out, for_splits)),
        Node::Leaf(token) => {
            let label = token.value().to_lowercase();
            let label_valid = !UNSEARCHABLE.contains(&label.as_str())
                && !(for_splits && NOT_SPLITTABLE.contains(&label.as_str()));
            let valid = matches!(
                token.kind,
                Kind::After
                    | Kind::Contact
                    | Kind::Attachment
                    | Kind::Filename
                    | Kind::Category
                    | Kind::AutoLabel
                    | Kind::Shared
            ) || (token.kind == Kind::Label && label_valid)
                || (token.kind == Kind::Header
                    && (token.field == "subject" || token.field == "list"));
            if !valid {
                out.push(token);
            }
        }
    }
}

/// The label (and category) names in `node` that no label answers to.
pub(super) fn missing_labels(node: &Node, labels: &LabelIndex) -> Vec<String> {
    let mut tokens = Vec::new();
    label_tokens(node, &mut tokens);
    tokens
        .iter()
        .map(|token| label_slug(token.value()))
        .filter(|slug| !labels.by_slug.contains_key(slug) && slug != "all" && slug != "unread")
        .collect()
}

/// Label tokens, with categories as the `in:category_…` labels they are. (Auto Labels aren't
/// among them, because Exo hasn't got any.)
pub(super) fn label_tokens(node: &Node, out: &mut Vec<Token>) {
    match node {
        Node::And(children, _) | Node::Or(children, _) => {
            children.iter().for_each(|child| label_tokens(child, out))
        }
        Node::Leaf(token) if token.kind == Kind::Label => out.push(token.clone()),
        Node::Leaf(token) if token.kind == Kind::Category => out.push(category_label(token)),
        Node::Leaf(_) => {}
    }
}

/// A category token as the `in:category_…` label token it stands for.
pub(super) fn category_label(token: &Token) -> Token {
    let value = format!("category_{}", token.value());
    Token {
        kind: Kind::Label,
        field: "in".into(),
        text: format!("in:{value}"),
        value: Some(value),
        quoted: false,
        negated: token.negated,
    }
}

/// Validates a split query (`raw` + `tree`) against `labels`.
pub(super) fn split_invalidity(raw: &str, tree: &Node, labels: &LabelIndex) -> Option<Invalidity> {
    static AUTO_LABEL: LazyLock<Regex> =
        LazyLock::new(|| Regex::new(r"(?i)autolabel:[a-zA-Z0-9_]*").expect("valid regex"));
    static QUOTED_SUBJECT: LazyLock<Regex> =
        LazyLock::new(|| Regex::new(r#"subject:"(.)*"(.)*""#).expect("valid regex"));
    let auto_labels: Vec<&str> = AUTO_LABEL
        .find_iter(raw)
        .map(|found| found.as_str())
        .collect();
    if !auto_labels.is_empty() {
        return Some(Invalidity {
            text: "We can't split your inbox with Auto Labels within the text definition.\n\n\n               Please use the Auto Labels section to apply an Auto Label for splitting.".into(),
            problem: auto_labels.join(","),
        });
    }
    let mut invalid = Vec::new();
    invalid_tokens(tree, &mut invalid, true);
    let missing = missing_labels(tree, labels);
    if (raw.is_empty() || invalid.is_empty()) && missing.is_empty() {
        return None;
    }
    if let [label] = missing.as_slice() {
        return Some(Invalidity {
            text: format!(
                "We can't split your inbox on labels that don't exist.\n\n            Please create label {label} first or try splitting on a different label."
            ),
            problem: label.clone(),
        });
    }
    if !missing.is_empty() {
        return Some(Invalidity {
            text: "We can't split your inbox on labels that don't exist.\n\n            Please create labels first or try splitting on a different label.".into(),
            problem: missing.join(","),
        });
    }
    if invalid.iter().all(|token| token.kind == Kind::Text) {
        if QUOTED_SUBJECT.is_match(raw) {
            return Some(Invalidity {
                text: "We can't split your inbox on subjects with quotes.\n\n              Please remove this and try again.".into(),
                problem: String::new(),
            });
        }
        let text = invalid
            .iter()
            .map(|token| token.value())
            .collect::<Vec<_>>()
            .join(" ");
        return Some(Invalidity {
            text: format!(
                "We can't split your inbox on text searches.\n\n            Please try subject:\"{text}\", or from:\"{text}\"."
            ),
            problem: text,
        });
    }
    let token = invalid.iter().find(|token| token.kind != Kind::Text)?;
    Some(Invalidity {
        text: format!(
            "We can't split your inbox on {}\n\n            Please remove this and try again.",
            token.text
        ),
        problem: token.text.clone(),
    })
}

/// Validates whether a bulk action can run on `tree`.
pub(super) fn bulk_refusal(tree: &Node, labels: &LabelIndex) -> Option<String> {
    let mut invalid = Vec::new();
    invalid_tokens(tree, &mut invalid, false);
    if invalid.is_empty() && missing_labels(tree, labels).is_empty() {
        return None;
    }
    Some(match invalid.iter().find(|token| token.kind == Kind::Text) {
        Some(word) => {
            // At most 20 characters, with whitespace collapsed and the cut made at a word.
            let value = word.value().split_whitespace().collect::<Vec<_>>().join(" ");
            let cut = if value.chars().count() <= 20 {
                value
            } else {
                let head: String = value.chars().take(20).collect();
                head.rfind(' ')
                    .map_or(head.clone(), |space| head[..space].to_owned())
            };
            format!(
                "We can't complete this action on text searches.\n\nPlease try subject:\"{cut}\" or from:\"{cut}\"."
            )
        }
        None => "We can't complete this action on this set of conversations.\n\nPlease unselect these conversations and try again.".into(),
    })
}

/// An `after:`/`before:` value in epoch milliseconds. Nine or ten digits are seconds. ISO
/// `YYYY-MM-DD` is UTC midnight, and `YYYY/M/D`, `YYYY-M-D` and `M/D/YYYY` are local midnight.
/// Anything else is an invalid date.
pub(super) fn parse_date(value: &str) -> Option<i64> {
    static SECONDS: LazyLock<Regex> =
        LazyLock::new(|| Regex::new(r"[0-9]{9,10}").expect("valid regex"));
    if SECONDS.is_match(value) {
        // Only the leading digits count.
        let digits: String = value
            .trim_start()
            .chars()
            .take_while(char::is_ascii_digit)
            .collect();
        return digits.parse::<i64>().ok().map(|seconds| seconds * 1000);
    }
    let value = value.trim();
    let separator = if value.contains('-') { '-' } else { '/' };
    let parts: Vec<&str> = value.split(separator).collect();
    let [first, second, third] = parts[..] else {
        return None;
    };
    let number = |part: &str| part.parse::<i16>().ok();
    let (year, month, day) = if first.len() == 4 {
        (number(first)?, number(second)?, number(third)?)
    } else {
        (number(third)?, number(first)?, number(second)?)
    };
    let date =
        jiff::civil::Date::new(year, i8::try_from(month).ok()?, i8::try_from(day).ok()?).ok()?;
    let iso = separator == '-' && first.len() == 4 && second.len() == 2 && third.len() == 2;
    let zone = if iso {
        jiff::tz::TimeZone::UTC
    } else {
        jiff::tz::TimeZone::system()
    };
    date.to_zoned(zone)
        .ok()
        .map(|zoned| zoned.timestamp().as_millisecond())
}

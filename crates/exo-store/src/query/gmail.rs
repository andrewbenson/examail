//! Compiler from parsed query trees to Gmail search strings (`q=`).

use super::{Kind, LabelIndex, Node, label_slug};

/// The Gmail search for `node`. `top` marks a split's top level, whose AND isn't parenthesized.
/// `important` is the Important split's definition, which `in:important` and `in:other` stand
/// for in a search.
pub(super) fn gmail_query(
    node: &Node,
    top: bool,
    labels: &LabelIndex,
    me: &[String],
    important: Option<&str>,
) -> String {
    let minus = if node.negated() { "-" } else { "" };
    let join = |children: &[Node]| {
        children
            .iter()
            .map(|child| gmail_query(child, false, labels, me, important))
            .collect::<Vec<_>>()
            .join(" ")
    };
    let token = match node {
        Node::And(children, negated) if top && !negated => return join(children),
        Node::And(children, _) => return format!("{minus}({})", join(children)),
        Node::Or(children, _) => return format!("{minus}{{{}}}", join(children)),
        Node::Leaf(token) => token,
    };
    if token.value.as_deref() == Some("") {
        return String::new();
    }
    let value = if token.quoted {
        format!("\"{}\"", token.value())
    } else {
        token.value().to_owned()
    };
    match token.kind {
        Kind::Label if important.is_some() && token.value().eq_ignore_ascii_case("important") => {
            format!("{minus}{}", important.unwrap_or_default())
        }
        Kind::Label if important.is_some() && token.value().eq_ignore_ascii_case("other") => {
            format!(
                "{}{}",
                if token.negated { "" } else { "-" },
                important.unwrap_or_default()
            )
        }
        Kind::Label => {
            let slug = label_slug(token.value());
            match labels.by_slug.get(&slug) {
                Some(label) if label_slug(&label.name) != slug => {
                    format!("{minus}label:{}", label_slug(&label.name))
                }
                _ if token
                    .value()
                    .chars()
                    .any(|c| matches!(c, '!'..='/' | '@' | ';')) =>
                {
                    format!("{minus}label:{slug}")
                }
                _ => format!("{minus}{}:{value}", token.field),
            }
        }
        // Exo has no Auto Labels to name.
        Kind::AutoLabel => String::new(),
        Kind::Contact if token.value().eq_ignore_ascii_case("me") && !token.quoted => {
            format!("{minus}{}:{{{}}}", token.field, me.join(" "))
        }
        Kind::Contact if is_dot_me(token.value()) => format!("{minus}{}:*.me", token.field),
        Kind::Contact | Kind::Header => format!("{minus}{}:{value}", token.field),
        Kind::Attachment => format!("{minus}has:attachment"),
        Kind::Shared => format!("{minus}from:\"sharing@superhuman.com\""),
        Kind::Filename => format!("{minus}filename:{value}"),
        Kind::After => format!("{minus}after:{value}"),
        Kind::Category => format!("{minus}category:{}", token.value()),
        Kind::Text => format!("{minus}{}", value.replace('\'', "")),
    }
}

/// Whether the value is `me` after nothing but punctuation (`.me`, `*.me`), ignoring case.
fn is_dot_me(value: &str) -> bool {
    let lower = value.to_lowercase();
    lower.strip_suffix("me").is_some_and(|rest| {
        rest.chars()
            .all(|c| !(c.is_ascii_alphanumeric() || c == '_'))
    })
}

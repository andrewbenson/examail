//! FTS5 query compiler, email tokenizer, and post-index highlight / recheck extraction.

use std::sync::LazyLock;

use regex::Regex;

use super::parse::{category_label, parse_date};
use super::{Kind, LabelIndex, Node, Token};

/// Words for which the index's porter stemming also matches other words (`w1`, `wedding` for
/// `wed`), so local results are checked for them as typed.
const STEMMED: [&str; 4] = ["wedding", "news", "university", "universal"];

/// A phrase or word that local results are checked for, as typed.
#[derive(Debug, Clone)]
pub(crate) struct Recheck {
    /// The text as a whole word, ignoring case. An end of the text only has to fall on a word
    /// boundary where it is a word character.
    pub regex: Regex,
    /// The text came from a `subject:` operator, so only the subject counts.
    pub subject_only: bool,
    /// Results must not have it.
    pub negated: bool,
}

/// The local index lookup for a [`super::SearchQuery`]: FTS5 expressions over the whole table, and
/// bounds on the thread's date. Every expression must hold, except a negated one, which must not.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub(crate) struct Fts {
    /// `(negated, expression)`. The first is never negated.
    pub matches: Vec<(bool, String)>,
    /// `(before, epoch ms)`: `sort < ms` when `before`, else `sort >= ms`.
    pub dates: Vec<(bool, i64)>,
}

/// A term of the full-table expression.
struct Term {
    negated: bool,
    group: bool,
    text: String,
}

/// The index's anchor term, which every row has, so an all-negative expression starts from it.
pub(crate) const FTS_ANCHOR: &str = "thread";

/// U+F8F7, a private-use character the index's tokenizer keeps inside a token.
const EMAIL_TOKENIZER: char = '\u{f8f7}';

/// An address anywhere in the value.
static EMAIL: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"[^\s<@]+@[a-zA-Z0-9-]+(?:\.[a-zA-Z0-9-]+)+").expect("valid regex")
});

/// An address as the index stores it whole, with every non-letter replaced by the token
/// character so the address is one token.
pub(crate) fn tokenize_email(email: &str) -> String {
    email
        .chars()
        .map(|c| {
            if c.is_ascii_alphabetic() {
                c
            } else {
                EMAIL_TOKENIZER
            }
        })
        .collect()
}

/// Compiles `tree` into a local FTS5 index lookup.
pub(super) fn compile(tree: &Node, labels: &LabelIndex, me: &[String]) -> Fts {
    let mut tokens: Vec<&Token> = Vec::new();
    let (mut anywhere, mut trash, mut spam, mut snippets) = (false, false, false, false);
    fn leaves<'a>(node: &'a Node, out: &mut Vec<&'a Token>) {
        match node {
            Node::And(children, _) | Node::Or(children, _) => {
                children.iter().for_each(|child| leaves(child, out))
            }
            Node::Leaf(token) if token.value.as_deref() == Some("") => {}
            Node::Leaf(token) => out.push(token),
        }
    }
    let mut all = Vec::new();
    leaves(tree, &mut all);
    for token in all {
        if token.kind == Kind::Label {
            match token.value().to_lowercase().as_str() {
                "all" | "anywhere" => {
                    anywhere = true;
                    continue;
                }
                "trash" => trash = true,
                "spam" => spam = true,
                "snippets" => snippets = true,
                _ => {}
            }
        }
        tokens.push(token);
    }
    let defaults: Vec<Token> = [
        ("trash", !anywhere && !trash),
        ("spam", !anywhere && !spam),
        ("snippets", !snippets),
    ]
    .into_iter()
    .filter(|(_, add)| *add)
    .map(|(label, _)| Token {
        kind: Kind::Label,
        field: "label".into(),
        value: Some(label.into()),
        quoted: false,
        negated: true,
        text: format!("label:{label}"),
    })
    .collect();
    let mut fts = Fts::default();
    let mut terms: Vec<Term> = Vec::new();
    for token in tokens.into_iter().chain(&defaults) {
        fts_token(token, labels, me, &mut terms, &mut fts);
    }
    fts.matches.insert(0, (false, full_table_match(terms)));
    fts
}

/// The terms joined by `AND`/`NOT`, with a positive one first.
fn full_table_match(mut terms: Vec<Term>) -> String {
    match terms.iter().position(|term| !term.negated) {
        Some(0) => {}
        Some(at) => {
            let first = terms.remove(at);
            terms.insert(0, first);
        }
        None => terms.insert(
            0,
            Term {
                negated: false,
                group: false,
                text: format!("meta:{FTS_ANCHOR}"),
            },
        ),
    }
    let many = terms.len() > 1;
    let mut out = String::new();
    for term in terms {
        let text = if term.group && many {
            format!("({})", term.text)
        } else {
            term.text
        };
        if out.is_empty() {
            out = text;
        } else {
            out = format!("{out} {} {text}", if term.negated { "NOT" } else { "AND" });
        }
    }
    out
}

/// One token's part of the lookup.
fn fts_token(
    token: &Token,
    labels: &LabelIndex,
    me: &[String],
    terms: &mut Vec<Term>,
    fts: &mut Fts,
) {
    let negated = token.negated;
    let column = |column: &str, value: &str| Term {
        negated,
        group: false,
        text: column_term(column, value),
    };
    match token.kind {
        Kind::Text if token.quoted && negated => fts
            .matches
            .push((true, fts5_prefix_or_phrase(token.value()))),
        Kind::Text if token.quoted => terms.push(Term {
            negated: false,
            group: false,
            text: fts5_prefix_or_phrase(token.value()),
        }),
        Kind::Text => terms.push(Term {
            negated,
            group: false,
            text: fts5_value(token.value()),
        }),
        Kind::Attachment => terms.push(column("meta", "attachment")),
        Kind::Shared => terms.push(column("meta", "shared")),
        Kind::Filename => column_or_phrase("attachments", token, terms, fts),
        Kind::Header => column_or_phrase(fts_column(&token.field), token, terms, fts),
        Kind::Contact => {
            let fields: &[&str] = if token.field == "to" {
                &["to", "cc", "bcc"]
            } else {
                &[fts_column(&token.field)]
            };
            let values: Option<Vec<String>> = if token.value().eq_ignore_ascii_case("me")
                && !token.quoted
            {
                Some(me.iter().map(|address| tokenize_email(address)).collect())
            } else if EMAIL.is_match(token.value()) {
                Some(vec![tokenize_email(token.value())])
            } else if !token.quoted || token.value().chars().all(|c| c.is_ascii_alphanumeric()) {
                Some(vec![token.value().to_owned()])
            } else {
                None
            };
            let Some(values) = values else {
                if fields.len() > 1 {
                    let phrase = fts5_prefix_or_phrase(token.value());
                    let expr = fields
                        .iter()
                        .map(|field| format!("{field} : {phrase}"))
                        .collect::<Vec<_>>()
                        .join(" OR ");
                    fts.matches.push((token.negated, format!("({expr})")));
                    return;
                }
                return column_or_phrase(fields[0], token, terms, fts);
            };
            let each: Vec<String> = values
                .iter()
                .flat_map(|value| fields.iter().map(move |field| column_term(field, value)))
                .collect();
            if negated {
                terms.extend(each.into_iter().map(|text| Term {
                    negated: true,
                    group: false,
                    text,
                }));
            } else if each.len() > 1 {
                terms.push(Term {
                    negated: false,
                    group: true,
                    text: each.join(" OR "),
                });
            } else if let Some(text) = each.into_iter().next() {
                terms.push(Term {
                    negated: false,
                    group: false,
                    text,
                });
            }
        }
        Kind::After => fts.dates.push(match parse_date(token.value()) {
            Some(date) => (negated, date.div_euclid(1000) * 1000),
            None => (true, 0),
        }),
        Kind::Label => terms.push(column("labels", labels.id(token.value()))),
        Kind::Category => terms.push(column("labels", labels.id(category_label(token).value()))),
        // Exo hasn't got Auto Labels, so there are none to look up.
        Kind::AutoLabel => {}
    }
}

/// The index column for a header or contact field. The parser only produces these fields.
fn fts_column(field: &str) -> &'static str {
    match field {
        "subject" => "subject",
        "list" => "list",
        "rfc822msgid" => "rfc822msgid",
        "from" => "from",
        "cc" => "cc",
        "bcc" => "bcc",
        "replyto" => "replyto",
        "deliveredto" => "deliveredto",
        _ => "to",
    }
}

/// A quoted value is a phrase looked up on its own, and any other value is a column term.
fn column_or_phrase(column: &str, token: &Token, terms: &mut Vec<Term>, fts: &mut Fts) {
    if token.quoted {
        fts.matches.push((
            token.negated,
            format!("{column} : {}", fts5_prefix_or_phrase(token.value())),
        ));
    } else {
        terms.push(Term {
            negated: token.negated,
            group: false,
            text: column_term(column, token.value()),
        });
    }
}

/// `column:value`, with the value as [`fts5_value`] writes it.
fn column_term(column: &str, value: &str) -> String {
    format!("{column}:{}", fts5_value(value))
}

/// A bare term when FTS5 reads it as one, else a quoted phrase. User text only ever reaches FTS5
/// through here or [`fts5_phrase`].
fn fts5_value(value: &str) -> String {
    let bare = value.strip_suffix('*').unwrap_or(value);
    let plain = !bare.is_empty()
        && bare
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == EMAIL_TOKENIZER);
    let keyword = ["AND", "OR", "NOT", "NEAR"]
        .iter()
        .any(|keyword| bare.eq_ignore_ascii_case(keyword));
    if plain && !keyword {
        value.to_owned()
    } else {
        fts5_phrase(value)
    }
}

/// `"word"*` for a trailing star, else a phrase.
fn fts5_prefix_or_phrase(value: &str) -> String {
    match value.strip_suffix('*') {
        Some(head) if !head.ends_with('*') && !head.contains(['*', '"']) && !head.is_empty() => {
            format!("{}*", fts5_phrase(head))
        }
        _ => fts5_phrase(value),
    }
}

/// An FTS5 string literal, with its double quotes doubled.
fn fts5_phrase(value: &str) -> String {
    format!("\"{}\"", value.replace('"', "\"\""))
}

/// Words and subject phrases that show in bold in a result's subject and snippet.
pub(super) fn highlightable_tokens(tree: &Node) -> Vec<String> {
    fn walk(node: &Node, out: &mut Vec<String>) {
        match node {
            Node::And(children, _) | Node::Or(children, _) => {
                children.iter().for_each(|child| walk(child, out))
            }
            Node::Leaf(token)
                if (token.kind == Kind::Text
                    || (token.kind == Kind::Header && token.field == "subject"))
                    && !token.value().is_empty() =>
            {
                out.push(token.value().to_owned())
            }
            Node::Leaf(_) => {}
        }
    }
    let mut out = Vec::new();
    walk(tree, &mut out);
    out
}

/// Phrases and stemmed words that local results are rechecked against.
pub(super) fn rechecks(tree: &Node) -> (Vec<Recheck>, bool) {
    fn walk<'a>(node: &'a Node, out: &mut Vec<&'a Token>) {
        match node {
            Node::And(children, _) | Node::Or(children, _) => {
                children.iter().for_each(|child| walk(child, out))
            }
            Node::Leaf(token)
                if (token.kind == Kind::Text
                    || (token.kind == Kind::Header && token.field == "subject"))
                    && !token.value().is_empty() =>
            {
                out.push(token)
            }
            Node::Leaf(_) => {}
        }
    }
    static PUNCTUATION: LazyLock<Regex> =
        LazyLock::new(|| Regex::new(r"[~!@#$%^*()+={}\\:;'<>,.?]").expect("valid regex"));
    let mut words = Vec::new();
    walk(tree, &mut words);
    let rechecks: Vec<Recheck> = words
        .iter()
        .filter(|token| {
            (token.quoted && token.text.matches(['"', '\'']).count() >= 2)
                || STEMMED.contains(&token.value().to_lowercase().as_str())
        })
        .filter_map(|token| {
            let term = PUNCTUATION
                .replace_all(&token.value().to_lowercase(), "")
                .into_owned();
            let edge = |c: Option<char>| {
                if c.is_some_and(|c| c.is_ascii_alphanumeric() || c == '_') {
                    r"(?-u:\b)"
                } else {
                    ""
                }
            };
            let pattern = format!(
                "(?i){}{}{}",
                edge(term.chars().next()),
                regex::escape(&term),
                edge(term.chars().last())
            );
            Some(Recheck {
                regex: Regex::new(&pattern).ok()?,
                subject_only: token.kind == Kind::Header,
                negated: token.negated,
            })
        })
        .collect();
    let all = !rechecks.is_empty() && rechecks.len() == words.len();
    (rechecks, all)
}

/// Contact values and text tokens highlighted in search results' participant names.
pub(super) fn highlightable_contact_tokens(tree: &Node) -> Vec<String> {
    fn walk(node: &Node, out: &mut Vec<String>) {
        match node {
            Node::And(children, _) | Node::Or(children, _) => {
                children.iter().for_each(|child| walk(child, out))
            }
            Node::Leaf(token)
                if matches!(token.kind, Kind::Contact | Kind::Text)
                    && !token.value().is_empty() =>
            {
                out.push(token.value().to_owned())
            }
            Node::Leaf(_) => {}
        }
    }
    let mut out = Vec::new();
    walk(tree, &mut out);
    out
}

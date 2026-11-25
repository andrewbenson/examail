//! `$search` on messages: the part of Keyword Query Language Outlook clients use. Words and
//! `"phrases"` (searched in from, subject and body), `property:value` for the message's searchable
//! properties, date comparisons (`received>=2026-11-01`), `AND`, `OR`, `NOT` and parentheses. A
//! value matches a run of the property's words, the last one by prefix when it ends in `*`.

use jiff::Timestamp;
use jiff::tz::TimeZone;

use super::error::{GraphError, Result};
use crate::mail::StoredMessage;
use crate::mail::mime::tokenize;

#[derive(Debug, Clone, PartialEq)]
pub enum Kql {
    All(Vec<Kql>),
    Any(Vec<Kql>),
    Not(Box<Kql>),
    /// Words in a property, or in from, subject and body when `property` is `None`.
    Words {
        property: Option<String>,
        words: Vec<String>,
        prefix: bool,
    },
    /// `received>=…`, `sent<…`, `received:…` (the whole day).
    Date {
        property: String,
        op: String,
        value: String,
    },
}

/// What search reads of a message.
pub struct Doc<'a> {
    pub message: &'a StoredMessage,
    pub importance: &'a str,
    pub tz: &'a TimeZone,
}

impl Kql {
    /// Parses `$search`'s value, which Graph wants in double quotes.
    pub fn parse(text: &str) -> Result<Self> {
        let trimmed = text.trim();
        let inner = trimmed
            .strip_prefix('"')
            .and_then(|rest| rest.strip_suffix('"'))
            .unwrap_or(trimmed)
            .replace("\\\"", "\"");
        let tokens = lex(&inner);
        let mut parser = Parser { tokens, at: 0 };
        let query = parser.any()?;
        if parser.at != parser.tokens.len() || matches!(&query, Kql::All(parts) if parts.is_empty())
        {
            return Err(GraphError::bad_request(format!(
                "Syntax error: the search query '{text}' is not valid."
            )));
        }
        Ok(query)
    }

    pub fn matches(&self, doc: &Doc<'_>) -> bool {
        match self {
            Self::All(parts) => parts.iter().all(|part| part.matches(doc)),
            Self::Any(parts) => parts.iter().any(|part| part.matches(doc)),
            Self::Not(part) => !part.matches(doc),
            Self::Words {
                property,
                words,
                prefix,
            } => {
                let search = &doc.message.mime.search;
                let fields: Vec<&[String]> = match property.as_deref() {
                    None => vec![&search.from, &search.subject, &search.body],
                    Some("from") => vec![&search.from],
                    Some("to") => vec![&search.to],
                    Some("cc") => vec![&search.cc],
                    Some("bcc") => vec![&search.bcc],
                    Some("subject") => vec![&search.subject],
                    Some("body") => vec![&search.body],
                    Some("attachment") => vec![&search.filenames],
                    Some("participants") => vec![&search.from, &search.to, &search.cc, &search.bcc],
                    Some("recipients") => vec![&search.to, &search.cc, &search.bcc],
                    Some("hasattachment" | "hasattachments") => {
                        return words.first().map(String::as_str)
                            == Some(if search.has_attachment {
                                "true"
                            } else {
                                "false"
                            });
                    }
                    Some("importance") => {
                        return words.first().is_some_and(|word| word == doc.importance);
                    }
                    Some("kind") => return words.first().map(String::as_str) == Some("email"),
                    Some(_) => return false,
                };
                fields
                    .iter()
                    .any(|field| contains_run(field, words, *prefix))
            }
            Self::Date {
                property,
                op,
                value,
            } => {
                let at = match property.as_str() {
                    "received" => Timestamp::from_millisecond(doc.message.internal_date).ok(),
                    _ => doc
                        .message
                        .mime
                        .date
                        .or(Some(doc.message.internal_date))
                        .and_then(|ms| Timestamp::from_millisecond(ms).ok()),
                };
                let (Some(at), Some((start, end))) = (at, day_or_instant(value, doc.tz)) else {
                    return false;
                };
                match op.as_str() {
                    ">=" => at >= start,
                    ">" => at >= end,
                    "<=" => at < end,
                    "<" => at < start,
                    _ => at >= start && at < end,
                }
            }
        }
    }
}

/// A KQL date as the span it covers: a whole day (`11/17/2026`, `2026-11-17`) in the account's
/// zone, or an instant (`2026-11-17T14:00:00Z`) as a span of no length.
fn day_or_instant(value: &str, tz: &TimeZone) -> Option<(Timestamp, Timestamp)> {
    if let Ok(at) = value.parse::<Timestamp>() {
        let next = at.checked_add(jiff::SignedDuration::from_secs(1)).ok()?;
        return Some((at, next));
    }
    let date = value.parse::<jiff::civil::Date>().ok().or_else(|| {
        let mut parts = value.split('/');
        let month = parts.next()?.parse().ok()?;
        let day = parts.next()?.parse().ok()?;
        let year = parts.next()?.parse().ok()?;
        jiff::civil::Date::new(year, month, day).ok()
    })?;
    let start = date.to_zoned(tz.clone()).ok()?;
    let end = start.tomorrow().ok()?;
    Some((start.timestamp(), end.timestamp()))
}

/// Whether `words` appear as a run in `field`.
fn contains_run(field: &[String], words: &[String], prefix: bool) -> bool {
    if words.is_empty() {
        return false;
    }
    field.windows(words.len()).any(|window| {
        window
            .iter()
            .zip(words)
            .enumerate()
            .all(|(index, (have, want))| {
                if prefix && index == words.len() - 1 {
                    have.starts_with(want.as_str())
                } else {
                    have == want
                }
            })
    })
}

#[derive(Debug, Clone, PartialEq)]
enum Token {
    Open,
    Close,
    And,
    Or,
    Not,
    /// A term: `word`, `"a phrase"`, `property:value` or `property>=value`.
    Term(String),
}

fn lex(text: &str) -> Vec<Token> {
    let mut tokens = Vec::new();
    let mut chars = text.chars().peekable();
    while let Some(&ch) = chars.peek() {
        if ch.is_whitespace() {
            chars.next();
        } else if ch == '(' {
            chars.next();
            tokens.push(Token::Open);
        } else if ch == ')' {
            chars.next();
            tokens.push(Token::Close);
        } else {
            let mut term = String::new();
            let mut quoted = false;
            while let Some(&ch) = chars.peek() {
                if ch == '"' {
                    quoted = !quoted;
                } else if !quoted && (ch.is_whitespace() || ch == '(' || ch == ')') {
                    break;
                }
                term.push(ch);
                chars.next();
            }
            tokens.push(match term.as_str() {
                "AND" => Token::And,
                "OR" => Token::Or,
                "NOT" => Token::Not,
                _ => Token::Term(term),
            });
        }
    }
    tokens
}

struct Parser {
    tokens: Vec<Token>,
    at: usize,
}

impl Parser {
    fn any(&mut self) -> Result<Kql> {
        let mut parts = vec![self.all()?];
        while self.tokens.get(self.at) == Some(&Token::Or) {
            self.at += 1;
            parts.push(self.all()?);
        }
        Ok(if parts.len() == 1 {
            parts.remove(0)
        } else {
            Kql::Any(parts)
        })
    }

    fn all(&mut self) -> Result<Kql> {
        let mut parts = Vec::new();
        loop {
            match self.tokens.get(self.at) {
                Some(Token::And) => self.at += 1,
                Some(Token::Or | Token::Close) | None => break,
                Some(_) => parts.push(self.unary()?),
            }
        }
        Ok(if parts.len() == 1 {
            parts.remove(0)
        } else {
            Kql::All(parts)
        })
    }

    fn unary(&mut self) -> Result<Kql> {
        match self.tokens.get(self.at).cloned() {
            Some(Token::Not) => {
                self.at += 1;
                Ok(Kql::Not(Box::new(self.unary()?)))
            }
            Some(Token::Open) => {
                self.at += 1;
                let inner = self.any()?;
                if self.tokens.get(self.at) != Some(&Token::Close) {
                    return Err(GraphError::bad_request(
                        "Syntax error: unbalanced parentheses.",
                    ));
                }
                self.at += 1;
                Ok(inner)
            }
            Some(Token::Term(term)) => {
                self.at += 1;
                Ok(term_query(&term))
            }
            _ => Err(GraphError::bad_request("Syntax error in the search query.")),
        }
    }
}

const KNOWN_PROPERTIES: &[&str] = &[
    "from",
    "to",
    "cc",
    "bcc",
    "subject",
    "body",
    "attachment",
    "participants",
    "recipients",
    "hasattachment",
    "hasattachments",
    "importance",
    "kind",
    "received",
    "sent",
];

fn first_operator(term: &str) -> Option<(usize, &'static str)> {
    let mut quoted = false;
    for (idx, ch) in term.char_indices() {
        if ch == '"' {
            quoted = !quoted;
            continue;
        }
        if quoted {
            continue;
        }
        let rest = &term[idx..];
        let op = if rest.starts_with(">=") {
            Some(">=")
        } else if rest.starts_with("<=") {
            Some("<=")
        } else if ch == '>' {
            Some(">")
        } else if ch == '<' {
            Some("<")
        } else if ch == ':' {
            Some(":")
        } else if ch == '=' {
            Some("=")
        } else {
            None
        };
        if let Some(op) = op {
            return Some((idx, op));
        }
    }
    None
}

fn term_query(term: &str) -> Kql {
    if let Some((idx, op)) = first_operator(term) {
        let property = term[..idx].to_ascii_lowercase();
        if KNOWN_PROPERTIES.contains(&property.as_str()) {
            let value = term[idx + op.len()..].trim_matches('"');
            if property == "received" || property == "sent" {
                return Kql::Date {
                    property,
                    op: op.to_owned(),
                    value: value.to_owned(),
                };
            }
            return words(Some(property), value);
        }
    }
    words(None, term.trim_matches('"'))
}

fn words(property: Option<String>, value: &str) -> Kql {
    Kql::Words {
        property,
        words: tokenize(value),
        prefix: value.ends_with('*'),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_client_queries() {
        let query = Kql::parse("\"(atlas launch) AND received<=2026-11-17T14:00:01Z\"").unwrap();
        let Kql::All(parts) = query else {
            panic!("{query:?}")
        };
        assert_eq!(parts.len(), 2);
        assert!(matches!(&parts[1], Kql::Date { op, .. } if op == "<="));
        assert_eq!(
            Kql::parse("\"from:daniel OR subject:\\\"q4 planning\\\"\"").unwrap(),
            Kql::Any(vec![
                Kql::Words {
                    property: Some("from".into()),
                    words: vec!["daniel".into()],
                    prefix: false
                },
                Kql::Words {
                    property: Some("subject".into()),
                    words: vec!["q4".into(), "planning".into()],
                    prefix: false
                },
            ])
        );
        assert!(Kql::parse("\"\"").is_err());
    }

    #[test]
    fn runs_and_prefixes() {
        let field: Vec<String> = ["q4", "planning", "doc"].map(String::from).to_vec();
        assert!(contains_run(
            &field,
            &["planning".into(), "doc".into()],
            false
        ));
        assert!(!contains_run(
            &field,
            &["doc".into(), "planning".into()],
            false
        ));
        assert!(contains_run(&field, &["plan".into()], true));
        assert!(!contains_run(&field, &["plan".into()], false));
    }

    #[test]
    fn handles_urls_and_operators_inside_quoted_values() {
        // Unknown scheme/prefix like `https:` is searched as free text, not an unknown property.
        assert_eq!(
            Kql::parse("\"https://example.com\"").unwrap(),
            Kql::Words {
                property: None,
                words: vec!["https".into(), "example".into(), "com".into()],
                prefix: false,
            }
        );
        // Earliest operator outside quotes wins over `>=` inside quotes.
        assert_eq!(
            Kql::parse("\"subject:\\\"a >= b\\\"\"").unwrap(),
            Kql::Words {
                property: Some("subject".into()),
                words: vec!["a".into(), "b".into()],
                prefix: false,
            }
        );
        // Earliest operator outside quotes wins even when unquoted value contains `>=`.
        assert_eq!(
            Kql::parse("\"subject:alpha>=beta\"").unwrap(),
            Kql::Words {
                property: Some("subject".into()),
                words: vec!["alpha".into(), "beta".into()],
                prefix: false,
            }
        );
        // Quoted phrase containing a known property name stays free text.
        assert_eq!(
            Kql::parse("\"\\\"subject:hello\\\"\"").unwrap(),
            Kql::Words {
                property: None,
                words: vec!["subject".into(), "hello".into()],
                prefix: false,
            }
        );
    }
}

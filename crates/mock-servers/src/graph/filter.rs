//! `$filter`: the OData expressions Outlook evaluates — comparisons (`eq`, `ne`, `gt`, `ge`, `lt`,
//! `le`), `and`/`or`/`not`, `startswith`/`endswith`/`contains`, and `any` over collections —
//! evaluated against a resource's JSON. Strings compare without regard to case, as Exchange's do,
//! and timestamps as instants.

use jiff::Timestamp;
use serde_json::Value;

use super::error::{GraphError, Result};

#[derive(Debug, Clone, PartialEq)]
pub enum Expr {
    And(Box<Expr>, Box<Expr>),
    Or(Box<Expr>, Box<Expr>),
    Not(Box<Expr>),
    Compare(Path, Op, Literal),
    Function(Function, Path, String),
    /// `collection/any(var: body)`.
    Any(Path, String, Box<Expr>),
    /// A boolean property on its own (`isRead`).
    Truthy(Path),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Op {
    Eq,
    Ne,
    Gt,
    Ge,
    Lt,
    Le,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Function {
    StartsWith,
    EndsWith,
    Contains,
}

#[derive(Debug, Clone, PartialEq)]
pub enum Literal {
    Text(String),
    Bool(bool),
    Null,
    Number(f64),
    Time(Timestamp),
}

/// `from/emailAddress/address`, as segments.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Path(pub Vec<String>);

impl Path {
    /// The top-level property the path starts at.
    pub fn property(&self) -> &str {
        &self.0[0]
    }
}

impl Expr {
    pub fn parse(text: &str) -> Result<Self> {
        let tokens = tokenize(text)?;
        let mut parser = Parser { tokens, at: 0 };
        let expr = parser.or()?;
        if parser.at != parser.tokens.len() {
            return Err(syntax(text));
        }
        Ok(expr)
    }

    /// The top-level properties the expression reads, in the order they appear.
    pub fn properties(&self) -> Vec<String> {
        let mut out = Vec::new();
        self.collect(&mut out, None);
        out
    }

    fn collect(&self, out: &mut Vec<String>, var: Option<&str>) {
        let mut push = |path: &Path| {
            if Some(path.property()) != var && !out.iter().any(|p| p == path.property()) {
                out.push(path.property().to_owned());
            }
        };
        match self {
            Self::And(a, b) | Self::Or(a, b) => {
                a.collect(out, var);
                b.collect(out, var);
            }
            Self::Not(a) => a.collect(out, var),
            Self::Compare(path, ..) | Self::Function(_, path, _) | Self::Truthy(path) => push(path),
            Self::Any(path, name, body) => {
                push(path);
                body.collect(out, Some(name));
            }
        }
    }

    /// Whether `resource` matches.
    pub fn matches(&self, resource: &Value) -> bool {
        self.eval(resource, None)
    }

    fn eval(&self, root: &Value, var: Option<(&str, &Value)>) -> bool {
        let resolve = |path: &Path| -> Value {
            let (start, segments) = match var {
                Some((name, value)) if path.0[0] == name => (value, &path.0[1..]),
                _ => (root, &path.0[..]),
            };
            segments
                .iter()
                .try_fold(start, |value, segment| value.get(segment))
                .cloned()
                .unwrap_or(Value::Null)
        };
        match self {
            Self::And(a, b) => a.eval(root, var) && b.eval(root, var),
            Self::Or(a, b) => a.eval(root, var) || b.eval(root, var),
            Self::Not(a) => !a.eval(root, var),
            Self::Truthy(path) => resolve(path) == Value::Bool(true),
            Self::Compare(path, op, literal) => compare(&resolve(path), *op, literal),
            Self::Function(function, path, needle) => {
                let Some(text) = resolve(path).as_str().map(str::to_lowercase) else {
                    return false;
                };
                let needle = needle.to_lowercase();
                match function {
                    Function::StartsWith => text.starts_with(&needle),
                    Function::EndsWith => text.ends_with(&needle),
                    Function::Contains => text.contains(&needle),
                }
            }
            Self::Any(path, name, body) => resolve(path)
                .as_array()
                .is_some_and(|items| items.iter().any(|item| body.eval(root, Some((name, item))))),
        }
    }
}

fn compare(value: &Value, op: Op, literal: &Literal) -> bool {
    use std::cmp::Ordering;
    let ordering: Option<Ordering> = match (value, literal) {
        (Value::Null, Literal::Null) => Some(Ordering::Equal),
        (_, Literal::Null) | (Value::Null, _) => None,
        (Value::Bool(a), Literal::Bool(b)) => Some(a.cmp(b)),
        (Value::Number(a), Literal::Number(b)) => a.as_f64().and_then(|a| a.partial_cmp(b)),
        (Value::String(text), Literal::Time(at)) => {
            super::calendar::parse_instant(text).map(|t| t.cmp(at))
        }
        (Value::String(text), Literal::Text(other)) => {
            match (
                super::calendar::parse_instant(text),
                super::calendar::parse_instant(other),
            ) {
                (Some(a), Some(b)) => Some(a.cmp(&b)),
                _ => Some(text.to_lowercase().cmp(&other.to_lowercase())),
            }
        }
        _ => None,
    };
    match (op, ordering) {
        (Op::Ne, None) => !matches!((value, literal), (Value::Null, Literal::Null)),
        (_, None) => false,
        (Op::Eq, Some(o)) => o == Ordering::Equal,
        (Op::Ne, Some(o)) => o != Ordering::Equal,
        (Op::Gt, Some(o)) => o == Ordering::Greater,
        (Op::Ge, Some(o)) => o != Ordering::Less,
        (Op::Lt, Some(o)) => o == Ordering::Less,
        (Op::Le, Some(o)) => o != Ordering::Greater,
    }
}

#[derive(Debug, Clone, PartialEq)]
enum Token {
    Word(String),
    Text(String),
    Open,
    Close,
    Comma,
    Colon,
}

fn tokenize(text: &str) -> Result<Vec<Token>> {
    let mut tokens = Vec::new();
    let mut chars = text.chars().peekable();
    while let Some(&ch) = chars.peek() {
        match ch {
            ' ' | '\t' | '\n' | '\r' => {
                chars.next();
            }
            '(' => {
                chars.next();
                tokens.push(Token::Open);
            }
            ')' => {
                chars.next();
                tokens.push(Token::Close);
            }
            ',' => {
                chars.next();
                tokens.push(Token::Comma);
            }
            ':' => {
                chars.next();
                tokens.push(Token::Colon);
            }
            '\'' => {
                chars.next();
                let mut value = String::new();
                loop {
                    match chars.next() {
                        Some('\'') if chars.peek() == Some(&'\'') => {
                            chars.next();
                            value.push('\'');
                        }
                        Some('\'') => break,
                        Some(ch) => value.push(ch),
                        None => return Err(syntax(text)),
                    }
                }
                tokens.push(Token::Text(value));
            }
            _ => {
                let mut word = String::new();
                while let Some(&ch) = chars.peek() {
                    // A colon inside a timestamp (`14:00:00Z`) belongs to the word.
                    let in_time =
                        ch == ':' && word.chars().next().is_some_and(|c| c.is_ascii_digit());
                    if ch.is_whitespace() || "(),'".contains(ch) || (ch == ':' && !in_time) {
                        break;
                    }
                    word.push(ch);
                    chars.next();
                }
                tokens.push(Token::Word(word));
            }
        }
    }
    Ok(tokens)
}

struct Parser {
    tokens: Vec<Token>,
    at: usize,
}

impl Parser {
    fn peek_word(&self) -> Option<String> {
        match self.tokens.get(self.at) {
            Some(Token::Word(word)) => Some(word.to_ascii_lowercase()),
            _ => None,
        }
    }

    fn next(&mut self) -> Option<Token> {
        let token = self.tokens.get(self.at).cloned();
        self.at += 1;
        token
    }

    fn expect(&mut self, token: &Token) -> Result<()> {
        if self.next().as_ref() == Some(token) {
            Ok(())
        } else {
            Err(GraphError::bad_request("Invalid filter clause"))
        }
    }

    fn or(&mut self) -> Result<Expr> {
        let mut left = self.and()?;
        while self.peek_word().as_deref() == Some("or") {
            self.at += 1;
            left = Expr::Or(Box::new(left), Box::new(self.and()?));
        }
        Ok(left)
    }

    fn and(&mut self) -> Result<Expr> {
        let mut left = self.not()?;
        while self.peek_word().as_deref() == Some("and") {
            self.at += 1;
            left = Expr::And(Box::new(left), Box::new(self.not()?));
        }
        Ok(left)
    }

    fn not(&mut self) -> Result<Expr> {
        if self.peek_word().as_deref() == Some("not") {
            self.at += 1;
            return Ok(Expr::Not(Box::new(self.not()?)));
        }
        self.primary()
    }

    fn primary(&mut self) -> Result<Expr> {
        match self.next() {
            Some(Token::Open) => {
                let expr = self.or()?;
                self.expect(&Token::Close)?;
                Ok(expr)
            }
            Some(Token::Word(word)) => {
                let lower = word.to_ascii_lowercase();
                let function = match lower.as_str() {
                    "startswith" => Some(Function::StartsWith),
                    "endswith" => Some(Function::EndsWith),
                    "contains" => Some(Function::Contains),
                    _ => None,
                };
                if let Some(function) = function {
                    self.expect(&Token::Open)?;
                    let path = self.path()?;
                    self.expect(&Token::Comma)?;
                    let Some(Token::Text(needle)) = self.next() else {
                        return Err(GraphError::bad_request("Invalid filter clause"));
                    };
                    self.expect(&Token::Close)?;
                    return Ok(Expr::Function(function, path, needle));
                }
                if let Some(collection) = lower.strip_suffix("/any") {
                    let path = Path(
                        word[..collection.len()]
                            .split('/')
                            .map(str::to_owned)
                            .collect(),
                    );
                    self.expect(&Token::Open)?;
                    let Some(Token::Word(name)) = self.next() else {
                        return Err(GraphError::bad_request("Invalid filter clause"));
                    };
                    self.expect(&Token::Colon)?;
                    let body = self.or()?;
                    self.expect(&Token::Close)?;
                    return Ok(Expr::Any(path, name, Box::new(body)));
                }
                let path = Path(word.split('/').map(str::to_owned).collect());
                let op = match self.peek_word().as_deref() {
                    Some("eq") => Op::Eq,
                    Some("ne") => Op::Ne,
                    Some("gt") => Op::Gt,
                    Some("ge") => Op::Ge,
                    Some("lt") => Op::Lt,
                    Some("le") => Op::Le,
                    _ => return Ok(Expr::Truthy(path)),
                };
                self.at += 1;
                Ok(Expr::Compare(path, op, self.literal()?))
            }
            _ => Err(GraphError::bad_request("Invalid filter clause")),
        }
    }

    fn path(&mut self) -> Result<Path> {
        match self.next() {
            Some(Token::Word(word)) => Ok(Path(word.split('/').map(str::to_owned).collect())),
            _ => Err(GraphError::bad_request("Invalid filter clause")),
        }
    }

    fn literal(&mut self) -> Result<Literal> {
        match self.next() {
            Some(Token::Text(text)) => Ok(Literal::Text(text)),
            Some(Token::Word(word)) => match word.to_ascii_lowercase().as_str() {
                "true" => Ok(Literal::Bool(true)),
                "false" => Ok(Literal::Bool(false)),
                "null" => Ok(Literal::Null),
                _ => time_literal(&word)
                    .map(Literal::Time)
                    .or_else(|| word.parse().ok().map(Literal::Number))
                    .ok_or_else(|| GraphError::bad_request("Invalid filter clause")),
            },
            _ => Err(GraphError::bad_request("Invalid filter clause")),
        }
    }
}

/// `2026-11-17T14:00:00Z`, a zone-less date-time (`2026-11-17T14:00:00`), or a date (`2026-11-17`,
/// midnight UTC).
fn time_literal(word: &str) -> Option<Timestamp> {
    super::calendar::parse_instant(word)
}

fn syntax(text: &str) -> GraphError {
    GraphError::bad_request(format!("Invalid filter clause: {text}"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn message() -> Value {
        json!({
            "isRead": false,
            "subject": "Q4 planning doc",
            "conversationId": "AAQk1",
            "receivedDateTime": "2026-11-16T13:47:00Z",
            "from": {"emailAddress": {"name": "Daniel Reyes", "address": "daniel@brightmoor.co"}},
            "categories": ["Projects/Atlas Launch"],
            "toRecipients": [{"emailAddress": {"address": "maya@brightmoor.co"}}],
            "flag": {"flagStatus": "flagged"},
        })
    }

    fn matches(filter: &str) -> bool {
        Expr::parse(filter).unwrap().matches(&message())
    }

    #[test]
    fn evaluates_outlook_filters() {
        assert!(matches("isRead eq false"));
        assert!(matches(
            "receivedDateTime gt 1970-01-01 and conversationId eq 'AAQk1'"
        ));
        assert!(matches("receivedDateTime ge 2026-11-16T13:00:00Z"));
        assert!(!matches("receivedDateTime lt 2026-11-16T13:00:00Z"));
        assert!(matches(
            "from/emailAddress/address eq 'DANIEL@brightmoor.co'"
        ));
        assert!(matches("startswith(subject, 'q4')"));
        assert!(matches("categories/any(c:c eq 'Projects/Atlas Launch')"));
        assert!(matches(
            "toRecipients/any(r: r/emailAddress/address eq 'maya@brightmoor.co')"
        ));
        assert!(matches(
            "flag/flagStatus eq 'flagged' and not (isRead eq true)"
        ));
        assert!(matches("isRead eq true or subject eq 'q4 planning doc'"));
        assert!(!matches("hasAttachments eq true"));
    }

    #[test]
    fn lists_properties_in_order() {
        let expr = Expr::parse("receivedDateTime gt 1970-01-01 and conversationId eq 'x' and categories/any(c:c eq 'y')").unwrap();
        assert_eq!(
            expr.properties(),
            ["receivedDateTime", "conversationId", "categories"]
        );
    }

    #[test]
    fn compares_zoneless_date_time_time_zone_strings() {
        let event = json!({
            "start": { "dateTime": "2026-11-16T15:00:00.0000000", "timeZone": "UTC" },
            "end": { "dateTime": "2026-11-16T16:00:00.0000000", "timeZone": "UTC" },
        });
        let eval = |f: &str| Expr::parse(f).unwrap().matches(&event);
        assert!(eval("start/dateTime ge 2026-11-16T14:00:00Z"));
        assert!(eval("start/dateTime eq 2026-11-16T15:00:00Z"));
        assert!(!eval("start/dateTime lt 2026-11-16T15:00:00Z"));
        assert!(eval("start/dateTime ge '2026-11-16T14:00:00Z'"));
        assert!(eval("start/dateTime eq '2026-11-16T15:00:00Z'"));
        assert!(eval("start/dateTime ge '2026-11-16T15:00:00'"));
        assert!(eval("end/dateTime le 2026-11-16T16:00:00"));
        assert!(!eval("end/dateTime lt '2026-11-16T16:00:00Z'"));
    }

    #[test]
    fn rejects_broken_clauses() {
        for filter in [
            "isRead eq",
            "(isRead eq true",
            "subject eq 'open",
            "startswith(subject)",
        ] {
            assert!(Expr::parse(filter).is_err(), "{filter}");
        }
    }
}

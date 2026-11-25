//! Local [`ThreadView`] matcher for splits (message-level) and searches (thread-level).

use std::collections::BTreeSet;

use super::parse::{category_label, parse_date};
use super::{Kind, LabelIndex, Node, Token, label_slug};
use crate::db::{AttachmentRow, Mailbox};
use crate::view::{ThreadView, ViewMessage};

/// A thread as a query matches it. A split queries the messages that aren't drafts one by one.
/// A search queries every message, and a group matches when its parts match anywhere in the
/// thread.
pub(super) struct Matcher<'a> {
    thread: &'a ThreadView,
    messages: Vec<&'a ViewMessage>,
    attachments: &'a [AttachmentRow],
    labels: &'a LabelIndex,
    me: &'a [String],
    message_level: bool,
}

/// Indexes into [`Matcher::messages`].
type Found = BTreeSet<usize>;

impl<'a> Matcher<'a> {
    pub(super) fn split(
        thread: &'a ThreadView,
        attachments: &'a [AttachmentRow],
        labels: &'a LabelIndex,
        me: &'a [String],
    ) -> Self {
        Self {
            messages: thread.messages.iter().filter(|m| !m.is_draft()).collect(),
            thread,
            attachments,
            labels,
            me,
            message_level: true,
        }
    }

    pub(super) fn search(
        thread: &'a ThreadView,
        attachments: &'a [AttachmentRow],
        labels: &'a LabelIndex,
        me: &'a [String],
    ) -> Self {
        Self {
            messages: thread.messages.iter().collect(),
            thread,
            attachments,
            labels,
            me,
            message_level: false,
        }
    }

    fn all(&self) -> Found {
        (0..self.messages.len()).collect()
    }

    fn attachments_of(&self, message: &ViewMessage) -> impl Iterator<Item = &'a AttachmentRow> {
        let id = message.row.id.clone();
        self.attachments
            .iter()
            .filter(move |row| row.message_id == id)
    }

    /// Whether `message` has an attachment that shows, meaning one that isn't inline or a
    /// calendar file.
    fn has_visible_attachment(&self, message: &ViewMessage) -> bool {
        self.attachments_of(message).any(|row| {
            !row.inline
                && !matches!(
                    row.mime_type.to_ascii_lowercase().as_str(),
                    "text/calendar" | "application/ics"
                )
        })
    }

    /// The thread's time for `after:`, which is the time of its latest message that isn't a
    /// draft.
    fn thread_time(&self) -> i64 {
        self.thread
            .messages
            .iter()
            .filter(|m| !m.is_draft())
            .map(|m| m.row.internal_date)
            .max()
            .unwrap_or(i64::MIN)
    }

    /// A quick rejection before matching message by message.
    pub(super) fn excludes(&self, node: &Node) -> bool {
        let token = match node {
            Node::And(children, _) => return children.iter().any(|child| self.excludes(child)),
            Node::Or(children, _) => return children.iter().all(|child| self.excludes(child)),
            Node::Leaf(token) => token,
        };
        match token.kind {
            Kind::After => {
                let Some(date) = parse_date(token.value()) else {
                    return true;
                };
                let time = self.thread_time();
                time != date && (time >= date) == token.negated
            }
            Kind::Attachment => {
                self.thread
                    .messages
                    .iter()
                    .any(|m| self.has_visible_attachment(m))
                    == token.negated
            }
            Kind::Label if token.negated && !token.value().is_empty() => {
                let id = self.labels.id(token.value());
                self.messages.iter().any(|m| m.has(id))
            }
            // Auto Labels are unknown here, so they're excluded.
            Kind::AutoLabel => true,
            // No thread is shared without teams.
            Kind::Shared => !token.negated,
            _ => false,
        }
    }

    /// The messages `node` matches, message by message for a split. `None` means `node` has a
    /// token that can't be matched locally.
    pub(super) fn find(&self, node: &Node) -> Option<Found> {
        let found = match node {
            Node::Leaf(token) if token.value.as_deref() == Some("") => return Some(self.all()),
            Node::And(children, _) => {
                let sets = children
                    .iter()
                    .map(|child| self.find(child))
                    .collect::<Option<Vec<_>>>()?;
                if !self.message_level {
                    return Some(
                        self.whole(sets.iter().all(|set| !set.is_empty()), node.negated()),
                    );
                }
                let mut sets = sets.into_iter();
                let first = sets.next().unwrap_or_default();
                sets.fold(first, |acc, set| acc.intersection(&set).copied().collect())
            }
            Node::Or(children, _) => {
                let sets = children
                    .iter()
                    .map(|child| self.find(child))
                    .collect::<Option<Vec<_>>>()?;
                if !self.message_level {
                    return Some(
                        self.whole(sets.iter().any(|set| !set.is_empty()), node.negated()),
                    );
                }
                sets.into_iter().flatten().collect()
            }
            Node::Leaf(token) => self.find_token(token)?,
        };
        Some(match (node.negated(), self.message_level) {
            (false, _) => found,
            (true, true) => self.all().difference(&found).copied().collect(),
            (true, false) => self.whole(!found.is_empty(), true),
        })
    }

    /// Thread-level matching, which gives every message when the node matched (or, negated, when
    /// it didn't) and none otherwise.
    fn whole(&self, matched: bool, negated: bool) -> Found {
        if matched != negated {
            self.all()
        } else {
            Found::new()
        }
    }

    fn find_token(&self, token: &Token) -> Option<Found> {
        let indexes = |keep: &dyn Fn(&ViewMessage) -> bool| -> Found {
            self.messages
                .iter()
                .enumerate()
                .filter(|(_, m)| keep(m))
                .map(|(at, _)| at)
                .collect()
        };
        Some(match token.kind {
            Kind::After => match parse_date(token.value()) {
                Some(date) => indexes(&|m| {
                    if token.negated {
                        date < m.row.internal_date
                    } else {
                        date <= m.row.internal_date
                    }
                }),
                None => Found::new(),
            },
            Kind::Label => self.label(token.value()),
            Kind::Category => self.label(category_label(token).value()),
            Kind::AutoLabel | Kind::Shared => Found::new(),
            Kind::Attachment => indexes(&|m| self.has_visible_attachment(m)),
            Kind::Filename => {
                let wanted = token.value().to_lowercase();
                indexes(&|m| {
                    self.attachments_of(m)
                        .any(|row| filename_matches(&wanted, row))
                })
            }
            Kind::Contact => indexes(&|m| self.contact_matches(token, m)),
            Kind::Header if token.field == "subject" => {
                if word_match(
                    &normalize_subject(token.value()),
                    &normalize_subject(&self.thread.subject()),
                ) {
                    self.all()
                } else {
                    Found::new()
                }
            }
            Kind::Header if token.field == "list" => {
                let wanted = collapse_whitespace(token.value());
                if self.messages.iter().any(|m| {
                    m.row
                        .list_id
                        .as_deref()
                        .is_some_and(|list| word_match(&wanted, list))
                }) {
                    self.all()
                } else {
                    Found::new()
                }
            }
            Kind::Header | Kind::Text => return None,
        })
    }

    /// The whole thread when any message carries the label.
    fn label(&self, value: &str) -> Found {
        let wanted = label_slug(self.labels.id(value));
        if self
            .messages
            .iter()
            .any(|m| m.labels.iter().any(|label| label_slug(label) == wanted))
        {
            self.all()
        } else {
            Found::new()
        }
    }

    /// Whether an address of `message`, in the field `token` names, matches the token's value.
    fn contact_matches(&self, token: &Token, message: &ViewMessage) -> bool {
        let values: Vec<String> = if token.value().eq_ignore_ascii_case("me") && !token.quoted {
            self.me.to_vec()
        } else {
            vec![token.value().to_owned()]
        };
        let values: Vec<String> = values
            .iter()
            .map(|value| {
                value
                    .trim_start_matches([
                        '*', '.', '^', '/', '@', ',', '#', '$', '%', '\\', '|', ';',
                    ])
                    .to_lowercase()
            })
            .collect();
        let row = &message.row;
        let delivered: Vec<Mailbox>;
        let mailboxes: Vec<&Mailbox> = match token.field.as_str() {
            "from" => row.sender.0.iter().collect(),
            "to" => row
                .to
                .iter()
                .chain(row.cc.iter())
                .chain(row.bcc.iter())
                .collect(),
            "cc" => row.cc.iter().collect(),
            "bcc" => row.bcc.iter().collect(),
            "replyto" => row.reply_to.iter().collect(),
            "deliveredto" => {
                delivered = row
                    .headers
                    .iter()
                    .filter(|header| header.name.eq_ignore_ascii_case("Delivered-To"))
                    .map(|header| Mailbox {
                        name: None,
                        email: header.value.trim().to_owned(),
                    })
                    .collect();
                delivered.iter().collect()
            }
            _ => Vec::new(),
        };
        mailboxes
            .into_iter()
            .any(|mailbox| contact_matches_field(&values, mailbox))
    }
}

/// Whether `mailbox` matches one of `values` by its whole name or address, a part of its address,
/// or words of its name.
fn contact_matches_field(values: &[String], mailbox: &Mailbox) -> bool {
    let name = mailbox.name.as_deref().unwrap_or_default().to_lowercase();
    let email = mailbox.email.to_lowercase();
    let canonical = canonical_gmail_address(&email);
    if !values.iter().any(|value| {
        name.contains(value.as_str())
            || email.contains(value.as_str())
            || canonical.contains(value.as_str())
    }) {
        return false;
    }
    let name_words = words(&name);
    let at_parts: Vec<&str> = email.split('@').collect();
    let dot_parts: Vec<&str> = email.split(['.', '@']).collect();
    values.iter().any(|value| {
        name == *value
            || email == *value
            || canonical == *value
            || (!value.contains('@') && email.ends_with(&format!(".{value}")))
            || at_parts.contains(&value.as_str())
            || dot_parts.contains(&value.as_str())
            || words(value).iter().all(|word| name_words.contains(word))
    })
}

/// The canonical Gmail address: no `+tag`, no dots before the `@`, lower-cased.
fn canonical_gmail_address(email: &str) -> String {
    let mut parts: Vec<String> = email.split('@').map(str::to_owned).collect();
    if let Some(local) = parts.first_mut() {
        if let Some(plus) = local.find('+') {
            local.truncate(plus);
        }
        *local = local.replace('.', "");
    }
    parts.join("@").to_lowercase()
}

/// Runs of ASCII characters other than letters and digits separate words, and a run at either
/// end leaves an empty word there. Non-ASCII characters are part of a word.
pub(super) fn words(text: &str) -> Vec<&str> {
    let mut words = Vec::new();
    let mut start = 0;
    let mut in_separator = false;
    for (at, c) in text.char_indices() {
        let separator = c.is_ascii() && !c.is_ascii_alphanumeric();
        if separator && !in_separator {
            words.push(&text[start..at]);
        }
        if !separator && in_separator {
            start = at;
        }
        in_separator = separator;
    }
    words.push(if in_separator { "" } else { &text[start..] });
    words
}

/// Whether attachment `row` is the one `wanted` names, by its whole filename or MIME type or by
/// words of them.
fn filename_matches(wanted: &str, row: &AttachmentRow) -> bool {
    let name = row.filename.to_lowercase();
    let mime = row.mime_type.to_lowercase();
    if name == wanted || mime == wanted {
        return true;
    }
    let (name_words, mime_words) = (words(&name), words(&mime));
    words(wanted)
        .iter()
        .all(|word| name_words.contains(word) || mime_words.contains(word))
}

/// Collapses whitespace runs to one space and trims.
fn collapse_whitespace(text: &str) -> String {
    text.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// A subject as `subject:` compares it, with punctuation turned into spaces and whitespace
/// collapsed.
fn normalize_subject(text: &str) -> String {
    const PUNCTUATION: &str = "`~!@#$%^*)(+-={\\:;'<>,.?/";
    collapse_whitespace(
        &text
            .chars()
            .map(|c| if PUNCTUATION.contains(c) { ' ' } else { c })
            .collect::<String>(),
    )
}

fn is_ascii_word(c: char) -> bool {
    c.is_ascii_alphanumeric() || c == '_'
}

/// Whether `needle` is in `haystack` between word boundaries, ignoring case.
///
/// Matches the exact boundary semantics of `(?i)(?:^|(?-u:\b)|\s|$)needle(?:^|(?-u:\b)|\s|$)`
/// without compiling a regex per call.
pub(super) fn word_match(needle: &str, haystack: &str) -> bool {
    if needle.is_empty() {
        return true;
    }
    let needle = needle.to_lowercase();
    let haystack = haystack.to_lowercase();
    let (Some(first), Some(last)) = (needle.chars().next(), needle.chars().next_back()) else {
        return true;
    };
    let first_word = is_ascii_word(first);
    let last_word = is_ascii_word(last);
    let mut offset = 0;
    while let Some(rel) = haystack[offset..].find(&needle) {
        let start = offset + rel;
        let end = start + needle.len();
        let left_ok = match haystack[..start].chars().next_back() {
            None => true,
            Some(prev) => prev.is_whitespace() || is_ascii_word(prev) != first_word,
        };
        let right_ok = match haystack[end..].chars().next() {
            None => true,
            Some(next) => next.is_whitespace() || is_ascii_word(next) != last_word,
        };
        if left_ok && right_ok {
            return true;
        }
        let step = haystack[start..].chars().next().map_or(1, char::len_utf8);
        offset = start + step;
    }
    false
}

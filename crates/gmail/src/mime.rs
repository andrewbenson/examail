//! Building outgoing RFC 822 messages: plain text with an HTML alternative, attachments, inline
//! images and threading headers.

use std::fmt;

use mail_builder::MessageBuilder;
use mail_builder::headers::address::Address;
use mail_builder::headers::content_type::ContentType;
use mail_builder::headers::message_id::MessageId;
use mail_builder::headers::raw::Raw;
use mail_builder::mime::{BodyPart, MimePart};
use serde::{Deserialize, Serialize};

use crate::model::Message;

/// A name and address, as in `From: Ada Lovelace <ada@example.com>`.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct Mailbox {
    /// Display name.
    pub name: Option<String>,
    /// Address.
    pub email: String,
}

impl Mailbox {
    /// A named mailbox.
    pub fn new(name: impl Into<String>, email: impl Into<String>) -> Self {
        Self {
            name: Some(name.into()),
            email: email.into(),
        }
    }

    /// A bare address.
    pub fn address(email: impl Into<String>) -> Self {
        Self {
            name: None,
            email: email.into(),
        }
    }

    /// Parses `Ada Lovelace <ada@example.com>`, `"Lovelace, Ada" <ada@example.com>`,
    /// `"Ada" <ada@example.com> (via Docs)`, or a bare `ada@example.com`, as found in
    /// `From:`/`To:` header values.
    #[must_use]
    pub fn parse(text: &str) -> Self {
        if let Some((angle_start, angle_end)) = find_angle_addr(text) {
            Self {
                name: parse_display_name(&text[..angle_start]),
                email: text[angle_start + 1..angle_end].trim().to_owned(),
            }
        } else {
            Self::address(strip_comments(text).trim())
        }
    }

    /// Splits an RFC 5322 address-list header (`To:`, `Cc:`, ...) on commas outside quoted
    /// strings `"..."`, nested comments `(...)` and angle brackets `<...>`, parses each entry
    /// with [`Mailbox::parse`], and drops entries whose address contains no `@` (such as
    /// `undisclosed-recipients:;`).
    #[must_use]
    pub fn parse_list(text: &str) -> Vec<Self> {
        let mut mailboxes = Vec::new();
        let mut start = 0;
        let (mut quoted, mut escaped, mut angle) = (false, false, false);
        let mut comment_depth = 0_usize;

        for (idx, ch) in text.char_indices() {
            if escaped {
                escaped = false;
                continue;
            }
            if ch == '\\' && (quoted || comment_depth > 0) {
                escaped = true;
                continue;
            }
            if comment_depth > 0 {
                match ch {
                    '(' => comment_depth += 1,
                    ')' => comment_depth -= 1,
                    _ => {}
                }
                continue;
            }
            match ch {
                '"' => quoted = !quoted,
                '(' if !quoted && !angle => comment_depth = 1,
                '<' if !quoted => angle = true,
                '>' if !quoted => angle = false,
                ',' if !quoted && !angle => {
                    let mailbox = Self::parse(text[start..idx].trim());
                    if mailbox.email.contains('@') {
                        mailboxes.push(mailbox);
                    }
                    start = idx + 1;
                }
                _ => {}
            }
        }
        let mailbox = Self::parse(text[start..].trim());
        if mailbox.email.contains('@') {
            mailboxes.push(mailbox);
        }
        mailboxes
    }

    /// mail-builder encodes the name but writes the address verbatim, and addresses come from
    /// `mailto:` links and other people's headers. So control characters are dropped, and a line
    /// break can't start a header (say, `Bcc:`) of the sender's choosing.
    fn to_builder(&self) -> Address<'_> {
        Address::new_address(self.name.as_deref(), one_line(&self.email))
    }
}

impl fmt::Display for Mailbox {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        const SPECIALS: &[char] = &[
            '(', ')', '<', '>', '@', ',', ';', ':', '\\', '"', '.', '[', ']',
        ];
        match &self.name {
            Some(name) if name.is_empty() => f.write_str(&self.email),
            Some(name)
                if name.contains(SPECIALS)
                    || name.starts_with(char::is_whitespace)
                    || name.ends_with(char::is_whitespace) =>
            {
                let escaped = name.replace('\\', "\\\\").replace('"', "\\\"");
                write!(f, "\"{escaped}\" <{}>", self.email)
            }
            Some(name) => write!(f, "{name} <{}>", self.email),
            None => f.write_str(&self.email),
        }
    }
}

/// A file attached to an [`Email`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Attachment {
    /// Filename shown to the recipient.
    pub filename: String,
    /// MIME type, e.g. `application/pdf`.
    pub content_type: String,
    /// The file's bytes.
    pub data: Vec<u8>,
    /// Set for inline images referenced from the HTML as `cid:<content_id>`.
    pub content_id: Option<String>,
}

/// An outgoing message.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Email {
    /// Sender. Gmail fills in the account's default send-as address when absent.
    pub from: Option<Mailbox>,
    /// `To:` recipients.
    pub to: Vec<Mailbox>,
    /// `Cc:` recipients.
    pub cc: Vec<Mailbox>,
    /// `Bcc:` recipients (Gmail strips the header for other recipients).
    pub bcc: Vec<Mailbox>,
    /// `Reply-To:`.
    pub reply_to: Option<Mailbox>,
    /// Subject. For replies, see [`reply_subject`]. Line breaks are dropped when it's written.
    pub subject: String,
    /// Plain-text body.
    pub text: Option<String>,
    /// HTML body, sent as an alternative to `text` when both are set.
    pub html: Option<String>,
    /// An iCalendar object (invite or RSVP) sent as a third `text/calendar` alternative.
    pub calendar: Option<String>,
    /// Attachments and inline images.
    pub attachments: Vec<Attachment>,
    /// `Message-ID:` without angle brackets, generated when absent.
    pub message_id: Option<String>,
    /// `In-Reply-To:` without angle brackets.
    pub in_reply_to: Option<String>,
    /// `References:`, oldest first, without angle brackets.
    pub references: Vec<String>,
    /// `Date:`, or the current time when absent.
    pub date: Option<jiff::Zoned>,
    /// Extra headers, written verbatim (ASCII only).
    pub headers: Vec<(String, String)>,
}

impl Email {
    /// Sets the threading headers for a reply to `parent` (fetched with a format that includes
    /// headers). Gmail also needs the parent's `threadId` on the send call and a matching
    /// `Subject`.
    pub fn reply_to_message(&mut self, parent: &Message) {
        let parent_id = parent.header("Message-ID").map(strip_angles);
        self.references = parent
            .header("References")
            .map(|refs| refs.split_whitespace().map(strip_angles).collect())
            .unwrap_or_default();
        if let Some(parent_id) = &parent_id {
            self.references.push(parent_id.clone());
        }
        self.in_reply_to = parent_id;
    }

    /// Serializes the message as RFC 822 bytes, ready for `messages.send` or `drafts.create`.
    #[must_use]
    pub fn to_rfc822(&self) -> Vec<u8> {
        // mail-builder writes the subject verbatim, so a line break in it (from a `mailto:` link,
        // say) would start a header of the sender's choosing, such as `Bcc:`. So line breaks are
        // dropped, as a browser's single-line input drops them.
        let subject: String = self
            .subject
            .chars()
            .filter(|c| !matches!(c, '\r' | '\n'))
            .collect();
        let mut builder = MessageBuilder::new().subject(subject);
        if let Some(from) = &self.from {
            builder = builder.from(from.to_builder());
        }
        for (header, list) in [("To", &self.to), ("Cc", &self.cc), ("Bcc", &self.bcc)] {
            if !list.is_empty() {
                let addresses = list.iter().map(Mailbox::to_builder).collect();
                builder = builder.header(header, Address::new_list(addresses));
            }
        }
        if let Some(reply_to) = &self.reply_to {
            builder = builder.reply_to(reply_to.to_builder());
        }
        // Message IDs are written verbatim too, and threading ones come from other people's
        // mail.
        if let Some(id) = &self.message_id {
            builder = builder.message_id(one_line(id));
        }
        if let Some(id) = &self.in_reply_to {
            builder = builder.in_reply_to(one_line(id));
        }
        if !self.references.is_empty() {
            let refs = self.references.iter().map(|id| one_line(id));
            builder = builder.header("References", MessageId::new_list(refs));
        }
        if let Some(date) = &self.date {
            let date = jiff::fmt::rfc2822::to_string(date).expect("zoned times fit RFC 2822");
            builder = builder.header("Date", Raw::new(date));
        }
        for (name, value) in &self.headers {
            if is_valid_header_name(name) {
                builder = builder.header(name.as_str(), Raw::new(one_line(value)));
            }
        }
        builder
            .body(self.body())
            .write_to_vec()
            .expect("writing to a Vec can't fail")
    }

    /// The MIME tree: alternatives, wrapped in `related` for inline images and in `mixed` for
    /// attachments. Boundaries are derived from the content, so output is deterministic.
    fn body(&self) -> MimePart<'_> {
        let mut seed_input = Vec::new();
        for part in [
            self.message_id.as_deref().unwrap_or_default(),
            self.subject.as_str(),
            self.text.as_deref().unwrap_or_default(),
            self.html.as_deref().unwrap_or_default(),
            self.calendar.as_deref().unwrap_or_default(),
        ] {
            seed_input.extend_from_slice(part.as_bytes());
        }
        for attachment in &self.attachments {
            seed_input.extend_from_slice(attachment.filename.as_bytes());
            seed_input.extend_from_slice(attachment.content_type.as_bytes());
        }
        let seed = fnv1a(&seed_input);
        let collides = |boundary: &str| {
            let needle = boundary.as_bytes();
            let in_bytes = |haystack: &[u8]| {
                haystack
                    .windows(needle.len())
                    .any(|window| window == needle)
            };
            self.text.as_deref().is_some_and(|s| in_bytes(s.as_bytes()))
                || self.html.as_deref().is_some_and(|s| in_bytes(s.as_bytes()))
                || self
                    .calendar
                    .as_deref()
                    .is_some_and(|s| in_bytes(s.as_bytes()))
                || self
                    .attachments
                    .iter()
                    .any(|attachment| in_bytes(&attachment.data))
        };
        let container = |kind: &str, depth: u64, parts: Vec<MimePart<'static>>| {
            let mut attempt = 0_u64;
            let boundary = loop {
                let candidate = if attempt == 0 {
                    format!("{:016x}{depth:04x}", seed.wrapping_add(depth))
                } else {
                    format!("{:016x}{depth:04x}{attempt:04x}", seed.wrapping_add(depth))
                };
                if !collides(&candidate) {
                    break candidate;
                }
                attempt += 1;
            };
            MimePart::new(
                ContentType::new(format!("multipart/{kind}")).attribute("boundary", boundary),
                parts,
            )
        };

        let mut alternatives = Vec::new();
        if let Some(text) = &self.text {
            alternatives.push(text_part("text/plain", text.clone(), None));
        }
        if let Some(html) = &self.html {
            alternatives.push(text_part("text/html", html.clone(), None));
        }
        if let Some(ics) = &self.calendar {
            let method = ics
                .lines()
                .find_map(|line| line.strip_prefix("METHOD:"))
                .map(|method| method.trim().to_owned());
            alternatives.push(text_part("text/calendar", ics.clone(), method));
        }
        let mut body = match alternatives.len() {
            0 => text_part("text/plain", String::new(), None),
            1 => alternatives.remove(0),
            _ => container("alternative", 1, alternatives),
        };

        let (inline, attached): (Vec<_>, Vec<_>) = self
            .attachments
            .iter()
            .partition(|attachment| attachment.content_id.is_some());
        if !inline.is_empty() {
            let mut parts = vec![body];
            parts.extend(inline.into_iter().map(|attachment| {
                part(attachment).inline().cid(one_line(
                    attachment.content_id.as_deref().unwrap_or_default(),
                ))
            }));
            body = container("related", 2, parts);
        }
        if !attached.is_empty() {
            let mut parts = vec![body];
            parts.extend(
                attached
                    .into_iter()
                    .map(|attachment| part(attachment).attachment(one_line(&attachment.filename))),
            );
            body = container("mixed", 3, parts);
        }
        body
    }
}

fn text_part(mime: &str, text: String, method: Option<String>) -> MimePart<'static> {
    let mut content_type = ContentType::new(mime.to_owned()).attribute("charset", "UTF-8");
    if let Some(method) = method {
        content_type = content_type.attribute("method", method);
    }
    MimePart::new(content_type, BodyPart::Text(text.into()))
}

/// An attachment's part. Its type, name and Content-ID come from files and from other people's
/// mail, and mail-builder writes the type and Content-ID verbatim. A line break in them would
/// start a header of the sender's choosing, so control characters are dropped.
fn part(attachment: &Attachment) -> MimePart<'static> {
    MimePart::new(
        ContentType::new(one_line(&attachment.content_type))
            .attribute("name", one_line(&attachment.filename)),
        BodyPart::Binary(attachment.data.clone().into()),
    )
}

/// `text` without control characters, which is how this module writes someone else's text in a
/// header. A line break in it then can't start a header of their choosing.
#[must_use]
pub fn one_line(text: &str) -> String {
    text.chars().filter(|c| !c.is_control()).collect()
}

/// `Re: <subject>`, without stacking prefixes.
#[must_use]
pub fn reply_subject(subject: &str) -> String {
    if subject
        .get(..3)
        .is_some_and(|prefix| prefix.eq_ignore_ascii_case("re:"))
    {
        subject.to_owned()
    } else {
        format!("Re: {subject}")
    }
}

/// Trims whitespace and enclosing `<` / `>` brackets from a Message-ID or Content-ID.
#[must_use]
pub fn strip_angles(id: &str) -> String {
    id.trim()
        .trim_start_matches('<')
        .trim_end_matches('>')
        .to_owned()
}

/// 64-bit FNV-1a, which is stable across Rust versions, unlike `DefaultHasher`.
#[must_use]
pub fn fnv1a(bytes: &[u8]) -> u64 {
    bytes.iter().fold(0xcbf2_9ce4_8422_2325, |hash, &byte| {
        (hash ^ u64::from(byte)).wrapping_mul(0x0100_0000_01b3)
    })
}

/// RFC 5322 `ftext`: printable US-ASCII except `:`.
fn is_valid_header_name(name: &str) -> bool {
    !name.is_empty() && name.bytes().all(|b| b.is_ascii_graphic() && b != b':')
}

/// Finds the top-level `<addr-spec>` outside quotes and RFC 5322 comments.
fn find_angle_addr(text: &str) -> Option<(usize, usize)> {
    let mut in_quotes = false;
    let mut escaped = false;
    let mut comment_depth = 0_usize;
    let mut angle_start: Option<usize> = None;
    let mut found: Option<(usize, usize)> = None;

    for (idx, ch) in text.char_indices() {
        if escaped {
            escaped = false;
            continue;
        }
        if ch == '\\' && (in_quotes || comment_depth > 0) {
            escaped = true;
            continue;
        }
        if comment_depth > 0 {
            match ch {
                '(' => comment_depth += 1,
                ')' => comment_depth -= 1,
                _ => {}
            }
            continue;
        }
        if ch == '"' {
            in_quotes = !in_quotes;
            continue;
        }
        if in_quotes {
            continue;
        }
        match ch {
            '(' if angle_start.is_none() => comment_depth = 1,
            '<' => angle_start = Some(idx),
            '>' => {
                if let Some(start) = angle_start.take() {
                    found = Some((start, idx));
                }
            }
            _ => {}
        }
    }
    found.or_else(|| angle_start.map(|start| (start, text.len())))
}

/// Decodes an RFC 5322 display-name phrase, unquoting `\"` and `\\` and skipping comments.
fn parse_display_name(raw: &str) -> Option<String> {
    let mut out = String::new();
    let mut pending_ws = String::new();
    let mut in_quotes = false;
    let mut escaped = false;
    let mut comment_depth = 0_usize;

    for ch in raw.chars() {
        if escaped {
            escaped = false;
            if comment_depth == 0 {
                if !in_quotes && !out.is_empty() && !pending_ws.is_empty() {
                    out.push_str(&pending_ws);
                }
                pending_ws.clear();
                out.push(ch);
            }
            continue;
        }
        if ch == '\\' && (in_quotes || comment_depth > 0) {
            escaped = true;
            continue;
        }
        if comment_depth > 0 {
            match ch {
                '(' => comment_depth += 1,
                ')' => comment_depth -= 1,
                _ => {}
            }
            continue;
        }
        if in_quotes {
            if ch == '"' {
                in_quotes = false;
            } else {
                out.push(ch);
            }
            continue;
        }
        match ch {
            '"' => {
                if !out.is_empty() && !pending_ws.is_empty() {
                    out.push_str(&pending_ws);
                }
                pending_ws.clear();
                in_quotes = true;
            }
            '(' => {
                comment_depth = 1;
                if !out.is_empty() && pending_ws.is_empty() {
                    pending_ws.push(' ');
                }
            }
            c if c.is_whitespace() => {
                if !out.is_empty() {
                    pending_ws.push(c);
                }
            }
            c => {
                if !out.is_empty() && !pending_ws.is_empty() {
                    out.push_str(&pending_ws);
                }
                pending_ws.clear();
                out.push(c);
            }
        }
    }
    if escaped && comment_depth == 0 {
        out.push('\\');
    }
    (!out.is_empty()).then_some(out)
}

/// Strips top-level RFC 5322 `(...)` comments outside quoted strings.
fn strip_comments(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut in_quotes = false;
    let mut escaped = false;
    let mut comment_depth = 0_usize;
    for ch in text.chars() {
        if escaped {
            escaped = false;
            if comment_depth == 0 {
                out.push(ch);
            }
            continue;
        }
        if ch == '\\' && (in_quotes || comment_depth > 0) {
            escaped = true;
            if comment_depth == 0 {
                out.push(ch);
            }
            continue;
        }
        if comment_depth > 0 {
            match ch {
                '(' => comment_depth += 1,
                ')' => comment_depth -= 1,
                _ => {}
            }
            continue;
        }
        match ch {
            '"' => {
                in_quotes = !in_quotes;
                out.push(ch);
            }
            '(' if !in_quotes => comment_depth = 1,
            c => out.push(c),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{MessagePart, MessagePartHeader};

    #[test]
    fn builds_alternative_with_attachment_and_threading() {
        let parent = Message {
            payload: Some(MessagePart {
                headers: vec![
                    MessagePartHeader {
                        name: "Message-ID".into(),
                        value: "<b@x>".into(),
                    },
                    MessagePartHeader {
                        name: "References".into(),
                        value: "<a@x>".into(),
                    },
                ],
                ..MessagePart::default()
            }),
            ..Message::default()
        };
        let mut email = Email {
            from: Some(Mailbox::new("Ada Lovelace", "ada@example.com")),
            to: vec![Mailbox::address("bob@example.com")],
            subject: reply_subject("Café plans ☕"),
            text: Some("See attached.".into()),
            html: Some("<p>See attached.</p>".into()),
            attachments: vec![Attachment {
                filename: "plan.csv".into(),
                content_type: "text/csv".into(),
                data: b"a,b\n1,2\n".to_vec(),
                content_id: None,
            }],
            message_id: Some("c@x".into()),
            date: Some(
                "2026-11-17T09:00:00-05:00[America/New_York]"
                    .parse()
                    .unwrap(),
            ),
            ..Email::default()
        };
        email.reply_to_message(&parent);
        let raw = String::from_utf8(email.to_rfc822()).unwrap();
        assert!(raw.contains("In-Reply-To: <b@x>"), "{raw}");
        assert!(raw.contains("References: <a@x> <b@x>"), "{raw}");
        assert!(raw.contains("multipart/mixed"), "{raw}");
        assert!(raw.contains("multipart/alternative"), "{raw}");
        assert!(raw.contains("filename=\"plan.csv\"") || raw.contains("filename=plan.csv"));
        assert!(raw.contains("Subject: =?utf-8?"), "{raw}");
        assert!(
            raw.contains("Date: Tue, 17 Nov 2026 09:00:00 -0500"),
            "{raw}"
        );
        assert_eq!(email.to_rfc822(), raw.as_bytes(), "output is deterministic");
    }

    #[test]
    fn mailbox_display_and_parse_round_trip() {
        for mailbox in [
            Mailbox::new("Ada Lovelace", "ada@example.com"),
            Mailbox::new("Lovelace, Ada", "ada@example.com"),
            Mailbox::new("Ada. L.", "ada@example.com"),
            Mailbox::new("Ada (Countess) [Eng]", "ada@example.com"),
            Mailbox::new(r#"Ada "Countess" \ Lovelace"#, "ada@example.com"),
            Mailbox::new(r"Ada\", "ada@example.com"),
            Mailbox::address("ada@example.com"),
        ] {
            let formatted = mailbox.to_string();
            assert_eq!(Mailbox::parse(&formatted), mailbox, "{formatted}");
        }
        assert_eq!(
            Mailbox::parse(r#""Ada. L." <ada@example.com>"#).to_string(),
            r#""Ada. L." <ada@example.com>"#
        );
        assert_eq!(
            Mailbox::parse(r#""Ada" <ada@x.com> (via Docs)"#),
            Mailbox::new("Ada", "ada@x.com")
        );
        assert_eq!(
            Mailbox::parse(r#"ada@x.com (via Docs)"#),
            Mailbox::address("ada@x.com")
        );
        assert_eq!(
            Mailbox::parse(r#""Ada \"The\" \\ Boss" <ada@x.com> (via <Docs>)"#),
            Mailbox::new(r#"Ada "The" \ Boss"#, "ada@x.com")
        );
    }

    #[test]
    fn mailbox_parse_list_handles_quotes_comments_angles_and_groups() {
        let header = concat!(
            r#""Lovelace, Ada" <ada@example.com>, "#,
            r#""Ada \"The, Boss\" \\" <boss@example.com> (Team, (Lead, Core\))), "#,
            r#"bob@example.com (via Docs, (internal, note\))), "#,
            r#"<"first,last"@example.com>, "#,
            r#"undisclosed-recipients:;, "#,
            r#""fake@name" <>, "#,
            r#" , "#,
            r#"Carol <carol@example.com>,"#
        );
        assert_eq!(
            Mailbox::parse_list(header),
            vec![
                Mailbox::new("Lovelace, Ada", "ada@example.com"),
                Mailbox::new(r#"Ada "The, Boss" \"#, "boss@example.com"),
                Mailbox::address("bob@example.com"),
                Mailbox::address(r#""first,last"@example.com"#),
                Mailbox::new("Carol", "carol@example.com"),
            ]
        );
        assert!(Mailbox::parse_list("").is_empty());
        assert!(Mailbox::parse_list("undisclosed-recipients:;").is_empty());
    }

    #[test]
    fn reply_subject_does_not_stack() {
        assert_eq!(reply_subject("Lunch"), "Re: Lunch");
        assert_eq!(reply_subject("RE: Lunch"), "RE: Lunch");
    }

    #[test]
    fn a_line_break_in_the_subject_adds_no_header() {
        let email = Email {
            subject: "Hi\r\nBcc: x@evil.com\nX-A: 1".into(),
            text: Some("Body".into()),
            ..Email::default()
        };
        let raw = String::from_utf8(email.to_rfc822()).expect("7-bit");
        let head = raw.split("\r\n\r\n").next().expect("a header block");
        assert!(
            head.lines()
                .any(|line| line == "Subject: HiBcc: x@evil.comX-A: 1"),
            "{head}"
        );
        assert!(
            !head
                .lines()
                .any(|line| line.starts_with("Bcc:") || line.starts_with("X-A:")),
            "{head}"
        );
    }

    #[test]
    fn a_line_break_in_an_address_or_message_id_adds_no_header() {
        // Addresses come from `mailto:` links and other people's headers.
        let email = Email {
            from: Some(Mailbox::new("Maya", "maya@x.com\r\nBcc: eve@evil.com")),
            to: vec![Mailbox::address("ann@x.com\nBcc: eve@evil.com")],
            cc: vec![Mailbox::new("Bob", "bob@x.com\rX-A: 1")],
            subject: "Hi".into(),
            text: Some("Body".into()),
            message_id: Some("a@x\r\nX-B: 1".into()),
            in_reply_to: Some("b@x\r\nX-C: 1".into()),
            references: vec!["c@x\nX-D: 1".into()],
            ..Email::default()
        };
        let raw = String::from_utf8(email.to_rfc822()).expect("7-bit");
        let head = raw.split("\r\n\r\n").next().expect("a header block");
        let injected = ["Bcc:", "X-A:", "X-B:", "X-C:", "X-D:"];
        assert!(
            !head.split(['\r', '\n']).any(|line| injected
                .iter()
                .any(|header| line.trim_start().starts_with(header))),
            "{head}"
        );
        assert!(head.contains("<ann@x.comBcc: eve@evil.com>"), "{head}");
    }

    #[test]
    fn a_line_break_in_an_attachment_adds_no_header() {
        let file = |content_id: Option<&str>| Attachment {
            filename: "notes\r\nBcc: x@evil.com\r\n.txt".into(),
            content_type: "text/plain\r\nX-A: 1".into(),
            data: b"hello".to_vec(),
            content_id: content_id.map(str::to_owned),
        };
        let email = Email {
            subject: "Files".into(),
            html: Some("<img src=\"cid:pic\">".into()),
            attachments: vec![file(None), file(Some("pic\r\nX-B: 1"))],
            ..Email::default()
        };
        let raw = String::from_utf8(email.to_rfc822()).expect("7-bit");
        assert!(
            !raw.lines().any(|line| ["Bcc:", "X-A:", "X-B:"]
                .iter()
                .any(|header| line.trim_start().starts_with(header))),
            "{raw}"
        );
        assert!(raw.contains("notesBcc: x@evil.com.txt"), "{raw}");
    }

    #[test]
    fn custom_headers_reject_invalid_names_and_strip_control_chars() {
        let email = Email {
            subject: "Hi".into(),
            text: Some("Body".into()),
            headers: vec![
                (
                    "X-Valid".into(),
                    "ok\r\nBcc: eve@evil.com\n\r\nFakeBody".into(),
                ),
                ("Bad Name".into(), "1".into()),
                ("Bad:Name".into(), "1".into()),
                ("Bad\r\nBcc".into(), "eve@evil.com".into()),
                (String::new(), "1".into()),
            ],
            ..Email::default()
        };
        let raw = String::from_utf8(email.to_rfc822()).expect("7-bit");
        let head = raw.split("\r\n\r\n").next().expect("a header block");
        assert!(
            head.lines()
                .any(|line| line == "X-Valid: okBcc: eve@evil.comFakeBody"),
            "{head}"
        );
        assert!(
            !head.split(['\r', '\n']).any(|line| {
                ["Bcc:", "Bad", "FakeBody"]
                    .iter()
                    .any(|bad| line.trim_start().starts_with(bad))
            }),
            "{head}"
        );
    }

    #[test]
    fn boundary_avoids_collisions_in_body_and_attachments() {
        let first = Email {
            subject: "Test".into(),
            text: Some("plain".into()),
            html: Some("<p>html</p>".into()),
            ..Email::default()
        };
        let first_raw = String::from_utf8(first.to_rfc822()).expect("utf8");
        let extract_boundary = |raw: &str| -> String {
            let after = raw.split("boundary=").nth(1).expect("boundary");
            let line = after.lines().next().expect("line").trim();
            line.trim_matches('"').to_owned()
        };
        let b1 = extract_boundary(&first_raw);

        // Changing HTML changes the boundary seed even when text is identical.
        let different_html = Email {
            html: Some("<p>different</p>".into()),
            ..first.clone()
        };
        assert_ne!(
            extract_boundary(&String::from_utf8(different_html.to_rfc822()).expect("utf8")),
            b1
        );

        // Compute the exact seed for a body containing a specific string doesn't collide,
        // and test direct collision with an attachment's bytes (which don't change the seed!).
        let with_att = Email {
            subject: "Att".into(),
            text: Some("body".into()),
            attachments: vec![Attachment {
                filename: "a.bin".into(),
                content_type: "application/octet-stream".into(),
                data: b"initial".to_vec(),
                content_id: None,
            }],
            ..Email::default()
        };
        let default_boundary =
            extract_boundary(&String::from_utf8(with_att.to_rfc822()).expect("utf8"));
        let colliding = Email {
            attachments: vec![Attachment {
                filename: "a.bin".into(),
                content_type: "application/octet-stream".into(),
                data: format!("--{default_boundary}--").into_bytes(),
                content_id: None,
            }],
            ..with_att
        };
        let colliding_raw = String::from_utf8(colliding.to_rfc822()).expect("utf8");
        let chosen = extract_boundary(&colliding_raw);
        assert_ne!(chosen, default_boundary, "stepped past colliding boundary");
        assert!(chosen.starts_with(&default_boundary));
    }
}

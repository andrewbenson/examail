//! Turning Gmail's messages into the rows the store keeps.

use std::collections::HashSet;

use gmail::mime::strip_angles;
use gmail::{Message, MessagePart};
use toasty::Json;

use crate::db::{AttachmentRow, BodyRow, Mailbox, MessageRow};
use crate::ids::labels::CHAT;
use crate::model::Header;

/// A message flattened into its rows.
pub(crate) struct Parsed {
    pub message: MessageRow,
    pub attachments: Vec<AttachmentRow>,
    /// `None` for a message fetched for its row alone.
    pub body: Option<BodyRow>,
}

/// Flattens a message fetched with `format=full`. When not `full`, the message was fetched with
/// `format=metadata`, and this returns its row alone, built from the headers Gmail sent, with no
/// body or attachments. `None` for a chat message (Exo ignores chats), or for one without an id
/// or thread id.
pub(crate) fn parse(message: &Message, full: bool) -> Option<Parsed> {
    if message.label_ids.iter().any(|label| label == CHAT) {
        return None;
    }
    let id = message.id.clone()?;
    let thread_id = message.thread_id.clone()?;
    let empty = MessagePart::default();
    let payload = message.payload.as_ref().unwrap_or(&empty);
    let header = |name: &str| payload.header(name).map(str::to_owned);
    let mailboxes = |name: &str| {
        payload
            .header(name)
            .map(Mailbox::parse_list)
            .unwrap_or_default()
    };

    let mut attachments = Vec::new();
    let mut calendar = false;
    let (mut html, mut text) = (Vec::new(), Vec::new());
    // A message fetched for its row comes with headers alone, so there are no parts to walk.
    for part in payload.walk().filter(|_| full) {
        let mime_type = part.mime_type.as_deref().unwrap_or_default();
        calendar |= mime_type.eq_ignore_ascii_case("text/calendar");
        let filename = part.filename.as_deref().unwrap_or_default();
        let body = part.body.as_ref();
        let as_attachment = part.header("Content-Disposition").is_some_and(|value| {
            value
                .trim_start()
                .to_ascii_lowercase()
                .starts_with("attachment")
        });
        let attachment_id = body.and_then(|body| body.attachment_id.clone());
        // Any part with a name or an attachment id is a file, because inline images often come
        // without a name. A big HTML or text body that Gmail moved out of the message (sync puts
        // it back) is the body, not a file.
        let is_body = !as_attachment
            && (mime_type.eq_ignore_ascii_case("text/html")
                || mime_type.eq_ignore_ascii_case("text/plain"));
        if !filename.is_empty() || (attachment_id.is_some() && !is_body) {
            let content_id = part.header("Content-ID").map(strip_angles);
            attachments.push(AttachmentRow {
                message_id: id.clone(),
                part_id: part.part_id.clone().unwrap_or_default(),
                attachment_id,
                filename: if filename.is_empty() {
                    default_name(mime_type)
                } else {
                    filename.to_owned()
                },
                mime_type: mime_type.to_owned(),
                size: body.and_then(|body| body.size).unwrap_or_default(),
                inline: false,
                content_id,
            });
            continue;
        }
        if as_attachment {
            continue;
        }
        let Some(data) = body.and_then(|body| body.data.as_deref()) else {
            continue;
        };
        if mime_type.eq_ignore_ascii_case("text/html") {
            html.push(String::from_utf8_lossy(data).into_owned());
        } else if mime_type.eq_ignore_ascii_case("text/plain") {
            text.push(String::from_utf8_lossy(data).into_owned());
        }
    }
    // A file is inline when the HTML shows it (`cid:` + its Content-ID) or it's a Gmail emoji,
    // whatever its Content-Disposition says, because senders often mark the images they show as
    // attachments.
    let shown = shown_content_ids(&html);
    for attachment in &mut attachments {
        attachment.inline = attachment.content_id.as_deref().is_some_and(|cid| {
            let lower = cid.to_ascii_lowercase();
            shown.contains(&lower)
                || lower
                    .strip_suffix("@goomoji.gmail")
                    .is_some_and(is_lower_hex)
        });
    }
    let joined = |parts: Vec<String>| (!parts.is_empty()).then(|| parts.join("\n"));

    Some(Parsed {
        message: MessageRow {
            id: id.clone(),
            thread_id,
            history_id: message.history_id.unwrap_or_default(),
            internal_date: message.internal_date.unwrap_or_default(),
            label_ids: message.label_ids.clone(),
            subject: header("Subject").unwrap_or_default(),
            snippet: message.snippet.clone().unwrap_or_default(),
            sender: Json(mailboxes("From").into_iter().next()),
            sender_raw: header("From").unwrap_or_default(),
            to: Json(mailboxes("To")),
            cc: Json(mailboxes("Cc")),
            bcc: Json(mailboxes("Bcc")),
            reply_to: Json(mailboxes("Reply-To")),
            rfc822_id: payload.header("Message-ID").map(strip_angles),
            in_reply_to: payload.header("In-Reply-To").map(strip_angles),
            references: payload
                .header("References")
                .map(|value| value.split_whitespace().map(strip_angles).collect())
                .unwrap_or_default(),
            list_id: header("List-Id"),
            list_unsubscribe: header("List-Unsubscribe"),
            list_unsubscribe_post: header("List-Unsubscribe-Post"),
            headers: Json(
                payload
                    .headers
                    .iter()
                    .map(|header| Header {
                        name: header.name.clone(),
                        value: header.value.clone(),
                    })
                    .collect(),
            ),
            size: message.size_estimate.unwrap_or_default(),
            has_attachments: calendar || attachments.iter().any(|attachment| !attachment.inline),
        },
        attachments,
        body: full.then(|| BodyRow {
            message_id: id,
            html: joined(html),
            text: joined(text),
            visible: None,
        }),
    })
}

/// The Content-IDs the HTML references, normalized to ASCII lowercase with `%HH` escapes decoded.
/// Each is what follows an ASCII-case-insensitive `cid:`, up to a quote, parenthesis, `>` or
/// whitespace.
fn shown_content_ids(html: &[String]) -> HashSet<String> {
    let mut out = HashSet::new();
    for part in html {
        let bytes = part.as_bytes();
        let mut i = 0;
        while i + 4 <= bytes.len() {
            if bytes[i..i + 4].eq_ignore_ascii_case(b"cid:") {
                let rest = &part[i + 4..];
                let end = rest
                    .find(|c: char| matches!(c, '(' | '\'' | '"' | ')' | '>') || c.is_whitespace())
                    .unwrap_or(rest.len());
                let raw_cid = &rest[..end];
                if !raw_cid.is_empty() {
                    out.insert(percent_decode(raw_cid).to_ascii_lowercase());
                }
                i += 4 + end;
            } else {
                i += 1;
            }
        }
    }
    out
}

fn percent_decode(input: &str) -> String {
    let bytes = input.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%'
            && let Some(hex) = bytes.get(i + 1..i + 3)
            && hex.iter().all(u8::is_ascii_hexdigit)
            && let Ok(hex_str) = std::str::from_utf8(hex)
            && let Ok(byte) = u8::from_str_radix(hex_str, 16)
        {
            out.push(byte);
            i += 3;
        } else {
            out.push(bytes[i]);
            i += 1;
        }
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// The name of a file sent without one, from its MIME type.
fn default_name(mime_type: &str) -> String {
    let extension = [
        ("json", ".json"),
        ("pdf", ".pdf"),
        ("html", ".html"),
        ("jpeg", ".jpg"),
        ("gif", ".gif"),
        ("plain", ".txt"),
        ("png", ".png"),
        ("message/rfc822", ".eml"),
    ]
    .into_iter()
    .find(|(kind, _)| mime_type.contains(kind))
    .map_or("", |(_, extension)| extension);
    format!("untitled{extension}")
}

fn is_lower_hex(text: &str) -> bool {
    !text.is_empty()
        && text
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}

/// Decodes HTML entities left in snippets and HTML bodies (`&#39;`, `&amp;`, `&mdash;`, ...).
pub(crate) fn unescape_html(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(start) = rest.find('&') {
        out.push_str(&rest[..start]);
        rest = &rest[start..];
        let decoded = rest.find(';').filter(|&end| end <= 10).and_then(|end| {
            let entity = &rest[1..end];
            let ch = named_html_entity(entity).or_else(|| {
                let digits = entity.strip_prefix('#')?;
                let code = if let Some(hex) = digits
                    .strip_prefix('x')
                    .or_else(|| digits.strip_prefix('X'))
                {
                    if hex.is_empty() || !hex.bytes().all(|b| b.is_ascii_hexdigit()) {
                        return None;
                    }
                    u32::from_str_radix(hex, 16).ok()?
                } else {
                    if digits.is_empty() || !digits.bytes().all(|b| b.is_ascii_digit()) {
                        return None;
                    }
                    digits.parse::<u32>().ok()?
                };
                char::from_u32(code)
            })?;
            Some((ch, end))
        });
        match decoded {
            Some((ch, end)) => {
                out.push(ch);
                rest = &rest[end + 1..];
            }
            None => {
                out.push('&');
                rest = &rest[1..];
            }
        }
    }
    out.push_str(rest);
    out
}

fn named_html_entity(entity: &str) -> Option<char> {
    Some(match entity {
        "amp" => '&',
        "lt" => '<',
        "gt" => '>',
        "quot" => '"',
        "apos" => '\'',
        "nbsp" => '\u{00A0}',
        "mdash" => '\u{2014}',
        "ndash" => '\u{2013}',
        "lsquo" => '\u{2018}',
        "rsquo" => '\u{2019}',
        "ldquo" => '\u{201C}',
        "rdquo" => '\u{201D}',
        "hellip" => '\u{2026}',
        "bull" => '\u{2022}',
        "middot" => '\u{00B7}',
        "copy" => '\u{00A9}',
        "reg" => '\u{00AE}',
        "trade" => '\u{2122}',
        "euro" => '\u{20AC}',
        "pound" => '\u{00A3}',
        "yen" => '\u{00A5}',
        "cent" => '\u{00A2}',
        "sect" => '\u{00A7}',
        "para" => '\u{00B6}',
        "deg" => '\u{00B0}',
        "plusmn" => '\u{00B1}',
        "times" => '\u{00D7}',
        "divide" => '\u{00F7}',
        "laquo" => '\u{00AB}',
        "raquo" => '\u{00BB}',
        "lsaquo" => '\u{2039}',
        "rsaquo" => '\u{203A}',
        "sbquo" => '\u{201A}',
        "bdquo" => '\u{201E}',
        "dagger" => '\u{2020}',
        "Dagger" => '\u{2021}',
        "permil" => '\u{2030}',
        "prime" => '\u{2032}',
        "Prime" => '\u{2033}',
        "larr" => '\u{2190}',
        "uarr" => '\u{2191}',
        "rarr" => '\u{2192}',
        "darr" => '\u{2193}',
        "harr" => '\u{2194}',
        "ensp" => '\u{2002}',
        "emsp" => '\u{2003}',
        "thinsp" => '\u{2009}',
        "zwnj" => '\u{200C}',
        "zwj" => '\u{200D}',
        "shy" => '\u{00AD}',
        "frac14" => '\u{00BC}',
        "frac12" => '\u{00BD}',
        "frac34" => '\u{00BE}',
        "sup1" => '\u{00B9}',
        "sup2" => '\u{00B2}',
        "sup3" => '\u{00B3}',
        "iexcl" => '\u{00A1}',
        "iquest" => '\u{00BF}',
        "brvbar" => '\u{00A6}',
        "macr" => '\u{00AF}',
        "acute" => '\u{00B4}',
        "cedil" => '\u{00B8}',
        "uml" => '\u{00A8}',
        "ordf" => '\u{00AA}',
        "ordm" => '\u{00BA}',
        "not" => '\u{00AC}',
        "micro" => '\u{00B5}',
        "Agrave" => '\u{00C0}',
        "Aacute" => '\u{00C1}',
        "Acirc" => '\u{00C2}',
        "Atilde" => '\u{00C3}',
        "Auml" => '\u{00C4}',
        "Aring" => '\u{00C5}',
        "AElig" => '\u{00C6}',
        "Ccedil" => '\u{00C7}',
        "Egrave" => '\u{00C8}',
        "Eacute" => '\u{00C9}',
        "Ecirc" => '\u{00CA}',
        "Euml" => '\u{00CB}',
        "Igrave" => '\u{00CC}',
        "Iacute" => '\u{00CD}',
        "Icirc" => '\u{00CE}',
        "Iuml" => '\u{00CF}',
        "ETH" => '\u{00D0}',
        "Ntilde" => '\u{00D1}',
        "Ograve" => '\u{00D2}',
        "Oacute" => '\u{00D3}',
        "Ocirc" => '\u{00D4}',
        "Otilde" => '\u{00D5}',
        "Ouml" => '\u{00D6}',
        "Oslash" => '\u{00D8}',
        "Ugrave" => '\u{00D9}',
        "Uacute" => '\u{00DA}',
        "Ucirc" => '\u{00DB}',
        "Uuml" => '\u{00DC}',
        "Yacute" => '\u{00DD}',
        "THORN" => '\u{00DE}',
        "szlig" => '\u{00DF}',
        "agrave" => '\u{00E0}',
        "aacute" => '\u{00E1}',
        "acirc" => '\u{00E2}',
        "atilde" => '\u{00E3}',
        "auml" => '\u{00E4}',
        "aring" => '\u{00E5}',
        "aelig" => '\u{00E6}',
        "ccedil" => '\u{00E7}',
        "egrave" => '\u{00E8}',
        "eacute" => '\u{00E9}',
        "ecirc" => '\u{00EA}',
        "euml" => '\u{00EB}',
        "igrave" => '\u{00EC}',
        "iacute" => '\u{00ED}',
        "icirc" => '\u{00EE}',
        "iuml" => '\u{00EF}',
        "eth" => '\u{00F0}',
        "ntilde" => '\u{00F1}',
        "ograve" => '\u{00F2}',
        "oacute" => '\u{00F3}',
        "ocirc" => '\u{00F4}',
        "otilde" => '\u{00F5}',
        "ouml" => '\u{00F6}',
        "oslash" => '\u{00F8}',
        "ugrave" => '\u{00F9}',
        "uacute" => '\u{00FA}',
        "ucirc" => '\u{00FB}',
        "uuml" => '\u{00FC}',
        "yacute" => '\u{00FD}',
        "thorn" => '\u{00FE}',
        "yuml" => '\u{00FF}',
        "OElig" => '\u{0152}',
        "oelig" => '\u{0153}',
        "Scaron" => '\u{0160}',
        "scaron" => '\u{0161}',
        "Yuml" => '\u{0178}',
        _ => return None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn splits_address_lists_outside_quotes() {
        let list = Mailbox::parse_list(
            r#""Lovelace, Ada" <ada@example.com>, "Ada \"The, Boss\" \\" <boss@example.com> (Team, Lead), bob@example.com, undisclosed-recipients:;"#,
        );
        assert_eq!(
            list,
            vec![
                Mailbox {
                    name: Some("Lovelace, Ada".into()),
                    email: "ada@example.com".into()
                },
                Mailbox {
                    name: Some(r#"Ada "The, Boss" \"#.into()),
                    email: "boss@example.com".into()
                },
                Mailbox {
                    name: None,
                    email: "bob@example.com".into()
                },
            ]
        );
    }

    #[test]
    fn outlook_uppercase_and_percent_encoded_cids_are_inline() {
        let part = |mime: &str, filename: &str, headers: &[(&str, &str)], data: &str| MessagePart {
            mime_type: Some(mime.into()),
            filename: Some(filename.into()),
            headers: headers
                .iter()
                .map(|(name, value)| gmail::MessagePartHeader {
                    name: (*name).into(),
                    value: (*value).into(),
                })
                .collect(),
            body: Some(gmail::MessagePartBody {
                data: (!data.is_empty()).then(|| data.as_bytes().to_vec()),
                ..Default::default()
            }),
            ..Default::default()
        };
        let message = Message {
            id: Some("m".into()),
            thread_id: Some("t".into()),
            payload: Some(MessagePart {
                mime_type: Some("multipart/related".into()),
                parts: vec![
                    part(
                        "text/html",
                        "",
                        &[],
                        r#"<img src="CID:Banner.PNG@01DB.A0"><img src="cid:image001.png%4001DB3A10.12345670">"#,
                    ),
                    part(
                        "image/png",
                        "banner.png",
                        &[
                            ("Content-Disposition", "attachment"),
                            ("Content-ID", "<banner.png@01db.a0>"),
                        ],
                        "",
                    ),
                    part(
                        "image/png",
                        "image001.png",
                        &[
                            ("Content-Disposition", "inline"),
                            ("Content-ID", "<Image001.png@01DB3A10.12345670>"),
                        ],
                        "",
                    ),
                ],
                ..Default::default()
            }),
            ..Default::default()
        };
        let parsed = parse(&message, true).unwrap();
        assert!(parsed.attachments.iter().all(|a| a.inline));
        assert!(!parsed.message.has_attachments);
    }

    #[test]
    fn files_the_html_shows_are_inline_whatever_their_disposition() {
        let part = |mime: &str, filename: &str, headers: &[(&str, &str)], data: &str| MessagePart {
            mime_type: Some(mime.into()),
            filename: Some(filename.into()),
            headers: headers
                .iter()
                .map(|(name, value)| gmail::MessagePartHeader {
                    name: (*name).into(),
                    value: (*value).into(),
                })
                .collect(),
            body: Some(gmail::MessagePartBody {
                data: (!data.is_empty()).then(|| data.as_bytes().to_vec()),
                ..Default::default()
            }),
            ..Default::default()
        };
        let message = Message {
            id: Some("m".into()),
            thread_id: Some("t".into()),
            payload: Some(MessagePart {
                mime_type: Some("multipart/mixed".into()),
                parts: vec![
                    part(
                        "text/html",
                        "",
                        &[],
                        "<img src=\"cid:comic=\"><img src='cid:1f600@goomoji.gmail'>",
                    ),
                    part(
                        "image/png",
                        "comic.png",
                        &[
                            ("Content-Disposition", "attachment"),
                            ("Content-ID", "<comic=>"),
                        ],
                        "",
                    ),
                    part(
                        "image/png",
                        "logo.png",
                        &[("Content-Disposition", "inline"), ("Content-ID", "<logo>")],
                        "",
                    ),
                    part(
                        "image/png",
                        "emoji.png",
                        &[("Content-ID", "<1f600@goomoji.gmail>")],
                        "",
                    ),
                ],
                ..Default::default()
            }),
            ..Default::default()
        };
        let parsed = parse(&message, true).unwrap();
        let inline: Vec<_> = parsed
            .attachments
            .iter()
            .map(|attachment| (attachment.filename.as_str(), attachment.inline))
            .collect();
        assert_eq!(
            inline,
            [
                ("comic.png", true),
                ("logo.png", false),
                ("emoji.png", true)
            ]
        );
        assert!(
            parsed.message.has_attachments,
            "the unreferenced logo is a file"
        );
    }

    #[test]
    fn nameless_parts_gmail_stores_apart_are_files() {
        let part =
            |mime: &str, headers: &[(&str, &str)], attachment_id: Option<&str>| MessagePart {
                mime_type: Some(mime.into()),
                filename: Some(String::new()),
                headers: headers
                    .iter()
                    .map(|(name, value)| gmail::MessagePartHeader {
                        name: (*name).into(),
                        value: (*value).into(),
                    })
                    .collect(),
                body: Some(gmail::MessagePartBody {
                    attachment_id: attachment_id.map(str::to_owned),
                    data: attachment_id
                        .is_none()
                        .then(|| b"<img src=\"cid:chart\">".to_vec()),
                    ..Default::default()
                }),
                ..Default::default()
            };
        let message = Message {
            id: Some("m".into()),
            thread_id: Some("t".into()),
            payload: Some(MessagePart {
                mime_type: Some("multipart/related".into()),
                parts: vec![
                    part("text/html", &[], None),
                    part("image/png", &[("Content-ID", "<chart>")], Some("A1")),
                    part("text/html", &[], Some("A2")),
                ],
                ..Default::default()
            }),
            ..Default::default()
        };
        let parsed = parse(&message, true).unwrap();
        let files: Vec<_> = parsed
            .attachments
            .iter()
            .map(|attachment| (attachment.filename.as_str(), attachment.inline))
            .collect();
        assert_eq!(
            files,
            [("untitled.png", true)],
            "a big HTML body isn't a file"
        );
        assert_eq!(default_name("application/octet-stream"), "untitled");
        assert_eq!(default_name("image/jpeg"), "untitled.jpg");
    }

    #[test]
    fn a_row_alone_has_no_body_or_files() {
        let message = Message {
            id: Some("m".into()),
            thread_id: Some("t".into()),
            payload: Some(MessagePart {
                mime_type: Some("multipart/mixed".into()),
                headers: vec![gmail::MessagePartHeader {
                    name: "Subject".into(),
                    value: "Q4".into(),
                }],
                ..Default::default()
            }),
            ..Default::default()
        };
        let parsed = parse(&message, false).unwrap();
        assert!(parsed.body.is_none() && parsed.attachments.is_empty());
        assert!(!parsed.message.has_attachments);
        assert_eq!(parsed.message.subject, "Q4");
        assert!(parse(&message, true).unwrap().body.is_some());
    }

    #[test]
    fn unescapes_snippets() {
        assert_eq!(
            unescape_html("it&#39;s &lt;b&gt; &amp; &#x263A; &bogus; & done"),
            "it's <b> & ☺ &bogus; & done"
        );
        assert_eq!(
            unescape_html(
                "&ldquo;caf&eacute;&rdquo; &mdash; &euro;5 &frac12; &copy; &rarr; &hellip;"
            ),
            "\u{201C}caf\u{00E9}\u{201D} \u{2014} \u{20AC}5 \u{00BD} \u{00A9} \u{2192} \u{2026}"
        );
        assert_eq!(
            unescape_html("&Aacute;&ntilde;&Ccedil;&szlig;&OElig;&Yuml;"),
            "\u{00C1}\u{00F1}\u{00C7}\u{00DF}\u{0152}\u{0178}"
        );
        // Leading signs in numeric entities must not be accepted.
        assert_eq!(
            unescape_html("&#+39; &#-39; &#x+27; &#X-27; &#; &#x;"),
            "&#+39; &#-39; &#x+27; &#X-27; &#; &#x;"
        );
    }

    #[test]
    fn percent_decode_requires_hex_digits() {
        assert_eq!(percent_decode("image%40host%2f1"), "image@host/1");
        assert_eq!(percent_decode("%+a%-a%0g"), "%+a%-a%0g");
    }
}

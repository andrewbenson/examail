//! Drafts and sending: drafts made from JSON or MIME, replies and forwards, attachments (a small
//! `POST`, or an upload session for large files), `send` and `sendMail`. A draft is a Gmail draft,
//! so saving one makes a new Gmail message; its Outlook key, and so its IDs, stay.

use std::sync::Arc;

use axum::extract::{Path, State};
use axum::http::{HeaderMap, HeaderValue, StatusCode, header};
use axum::response::{IntoResponse, Response};
use base64::Engine;
use base64::engine::general_purpose::{STANDARD, STANDARD_NO_PAD};
use gmail::Format;
use gmail::mime::{Attachment as MimeAttachment, Email, Mailbox as Address};
use jiff::{SignedDuration, Timestamp};
use serde_json::{Map, Value, json};

use super::error::{GraphError, Result};
use super::folders::body_json;
use super::ids::{self, Id};
use super::messages::{apply_patch_locked, entity, key_of, validate_patch};
use super::project;
use super::state::Upload;
use super::{Call, odata, origin, reply_map};
use crate::mail::{Mailbox, StoredMessage};
use crate::world::{App, World};

/// The largest attachment a single `POST` takes; larger ones need an upload session.
const SMALL_ATTACHMENT: usize = 3 * 1024 * 1024;
/// The largest attachment an upload session takes.
const LARGE_ATTACHMENT: usize = 150 * 1024 * 1024;
/// The largest range one `PUT` to an upload session carries.
const UPLOAD_CHUNK: usize = 4 * 1024 * 1024;

/// What a new message's JSON may carry.
const CREATABLE: &[&str] = &[
    "subject",
    "body",
    "toRecipients",
    "ccRecipients",
    "bccRecipients",
    "replyTo",
    "from",
    "sender",
    "importance",
    "isReadReceiptRequested",
    "isDeliveryReceiptRequested",
    "internetMessageHeaders",
    "internetMessageId",
    "attachments",
    "categories",
    "flag",
    "inferenceClassification",
    "singleValueExtendedProperties",
    "multiValueExtendedProperties",
];
/// Of those, the ones Outlook keeps beside the MIME content.
const OUTLOOK_ONLY: &[&str] = &["categories", "flag", "inferenceClassification"];

/// Whether a request body is MIME (`text/plain`, base64) rather than JSON.
fn is_mime(headers: &HeaderMap) -> bool {
    headers
        .get(header::CONTENT_TYPE)
        .and_then(|value| value.to_str().ok())
        .is_some_and(|value| value.starts_with("text/plain"))
}

/// A MIME body: RFC 822 bytes, base64-encoded.
fn decode_mime(body: &[u8]) -> Result<Vec<u8>> {
    let text: Vec<u8> = body
        .iter()
        .copied()
        .filter(|byte| !byte.is_ascii_whitespace())
        .collect();
    let invalid = || {
        GraphError::new(
            400,
            "ErrorMimeContentInvalidBase64String",
            "Invalid base64 string for MIME content.",
        )
    };
    let raw = STANDARD
        .decode(&text)
        .or_else(|_| STANDARD_NO_PAD.decode(&text))
        .map_err(|_| invalid())?;
    if crate::mail::mime::parse(&raw, "probe").is_none() || raw.is_empty() {
        return Err(invalid());
    }
    Ok(raw)
}

/// `POST /me/messages`: a draft, from JSON or from MIME.
pub async fn create(call: Call, headers: HeaderMap, body: axum::body::Bytes) -> Result<Response> {
    if is_mime(&headers) {
        let raw = decode_mime(&body)?;
        let mut world = call.world();
        let thread = thread_for(&world.mail, &raw);
        let key = new_draft_locked(&mut world, raw, thread)?;
        return draft_response(&call, &world, key);
    }
    let message: Map<String, Value> = body_json(&body, "message")?;
    check_keys(&message, CREATABLE)?;
    let mut world = call.world();
    let mut email = blank(&world);
    apply_json(&mut email, &message)?;
    let outlook_only: Map<String, Value> = message
        .into_iter()
        .filter(|(name, _)| OUTLOOK_ONLY.contains(&name.as_str()))
        .collect();
    if !outlook_only.is_empty() {
        validate_patch(&world, &outlook_only, true)?;
    }
    let raw = email.to_rfc822();
    let thread = thread_for(&world.mail, &raw);
    let key = new_draft_locked(&mut world, raw, thread)?;
    if !outlook_only.is_empty() {
        apply_patch_locked(&mut world, key, outlook_only)?;
    }
    draft_response(&call, &world, key)
}

fn draft_response(call: &Call, world: &World, key: u64) -> Result<Response> {
    let origin = origin(world);
    let view = call.view(world, &origin);
    let json = view.message_json(key, None)?;
    Ok(reply_map(
        201,
        entity(view.context("messages/$entity"), json),
    ))
}

/// Saves `raw` as a Gmail draft in a locked world and returns its Outlook key.
fn new_draft_locked(world: &mut World, raw: Vec<u8>, thread: Option<String>) -> Result<u64> {
    let now = world.clock.now();
    let draft = world.mail.create_draft(raw, thread, now.as_millisecond())?;
    let gmail = world.mail.draft(&draft)?.id.clone();
    world.graph.catch_up(&mut world.mail, now);
    world.graph.key_of(&gmail).ok_or_else(GraphError::not_found)
}

/// The thread a message joins: the one holding a message its threading headers name.
fn thread_for(mail: &Mailbox, raw: &[u8]) -> Option<String> {
    let parsed = crate::mail::mime::parse(raw, "probe")?;
    let all = mail.select_messages(&crate::mail::Selection {
        query: None,
        label_ids: Vec::new(),
        include_spam_trash: true,
    });
    parsed.ancestors.iter().rev().find_map(|ancestor| {
        all.iter()
            .find(|message| message.mime.rfc822_id.as_deref() == Some(ancestor))
            .map(|message| message.thread_id.clone())
    })
}

fn check_keys(json: &Map<String, Value>, allowed: &[&str]) -> Result<()> {
    match json
        .keys()
        .find(|name| !name.starts_with('@') && !allowed.contains(&name.as_str()))
    {
        Some(unknown) => Err(GraphError::unknown_body_property(unknown, "message")),
        None => Ok(()),
    }
}

/// An empty message from the mailbox's owner, dated now.
fn blank(world: &World) -> Email {
    Email {
        from: Some(Address::new(world.account.name(), &world.account.email)),
        date: Some(world.clock.now().to_zoned(world.account.tz.clone())),
        ..Email::default()
    }
}

/// A draft's content as an [`Email`], to change and save again.
fn email_of(message: &StoredMessage, world: &World) -> Email {
    let header = |name: &str| message.mime.payload.header(name);
    let ids = |name: &str| -> Vec<String> {
        header(name)
            .map(|value| {
                value
                    .split_whitespace()
                    .map(|id| id.trim_matches(['<', '>']).to_owned())
                    .collect()
            })
            .unwrap_or_default()
    };
    let (html, text) = project::bodies(&message.mime.payload);
    let kept = [
        "Importance",
        "Disposition-Notification-To",
        "Return-Receipt-To",
    ];
    Email {
        from: header("From").map(Address::parse),
        to: Address::parse_list(header("To").unwrap_or_default()),
        cc: Address::parse_list(header("Cc").unwrap_or_default()),
        bcc: Address::parse_list(header("Bcc").unwrap_or_default()),
        reply_to: Address::parse_list(header("Reply-To").unwrap_or_default())
            .into_iter()
            .next(),
        subject: message.mime.subject.clone(),
        text,
        html,
        attachments: project::attachments(message)
            .into_iter()
            .map(|attachment| MimeAttachment {
                filename: attachment.name,
                content_type: attachment.content_type,
                data: attachment.bytes,
                content_id: attachment.content_id.filter(|_| attachment.inline),
            })
            .collect(),
        message_id: message.mime.rfc822_id.clone(),
        in_reply_to: ids("In-Reply-To").into_iter().next(),
        references: ids("References"),
        date: Some(world.clock.now().to_zoned(world.account.tz.clone())),
        headers: message
            .mime
            .payload
            .headers
            .iter()
            .filter(|h| {
                kept.iter().any(|k| k.eq_ignore_ascii_case(&h.name))
                    || h.name.to_ascii_lowercase().starts_with("x-")
            })
            .map(|h| (h.name.clone(), h.value.clone()))
            .collect(),
        ..Email::default()
    }
}

/// Sets what a message's JSON says on `email`.
fn apply_json(email: &mut Email, message: &Map<String, Value>) -> Result<()> {
    let recipients = |value: &Value| -> Result<Vec<Address>> {
        value
            .as_array()
            .ok_or_else(|| GraphError::unreadable_body("recipients must be an array"))?
            .iter()
            .map(|recipient| {
                let address = &recipient["emailAddress"];
                let email = address["address"]
                    .as_str()
                    .filter(|a| !a.is_empty())
                    .ok_or_else(|| {
                        GraphError::new(
                            400,
                            "ErrorInvalidRecipients",
                            "At least one recipient isn't valid.",
                        )
                    })?;
                Ok(match address["name"].as_str() {
                    Some(name) if !name.is_empty() => Address::new(name, email),
                    _ => Address::address(email),
                })
            })
            .collect()
    };
    let set_header = |email: &mut Email, name: &str, value: Option<String>| {
        email
            .headers
            .retain(|(existing, _)| !existing.eq_ignore_ascii_case(name));
        if let Some(value) = value {
            email.headers.push((name.to_owned(), value));
        }
    };
    for (name, value) in message {
        match name.as_str() {
            "subject" => email.subject = value.as_str().unwrap_or_default().to_owned(),
            "body" => {
                let content = value["content"].as_str().unwrap_or_default().to_owned();
                if value["contentType"]
                    .as_str()
                    .is_some_and(|t| t.eq_ignore_ascii_case("html"))
                {
                    email.text = Some(mail_parser::decoders::html::html_to_text(&content));
                    email.html = Some(content);
                } else {
                    email.text = Some(content);
                    email.html = None;
                }
            }
            "toRecipients" => email.to = recipients(value)?,
            "ccRecipients" => email.cc = recipients(value)?,
            "bccRecipients" => email.bcc = recipients(value)?,
            "replyTo" => email.reply_to = recipients(value)?.into_iter().next(),
            "importance" => {
                let importance = value.as_str().unwrap_or("normal").to_ascii_lowercase();
                let header = match importance.as_str() {
                    "high" => Some("high".to_owned()),
                    "low" => Some("low".to_owned()),
                    _ => None,
                };
                set_header(email, "Importance", header);
            }
            "isReadReceiptRequested" | "isDeliveryReceiptRequested" => {
                let header = if name == "isReadReceiptRequested" {
                    "Disposition-Notification-To"
                } else {
                    "Return-Receipt-To"
                };
                let from = email.from.as_ref().map(|from| from.email.clone());
                set_header(
                    email,
                    header,
                    from.filter(|_| value.as_bool() == Some(true)),
                );
            }
            "internetMessageHeaders" => {
                for pair in value.as_array().into_iter().flatten() {
                    let (Some(name), Some(text)) = (pair["name"].as_str(), pair["value"].as_str())
                    else {
                        continue;
                    };
                    if !name.to_ascii_lowercase().starts_with("x-") {
                        return Err(GraphError::new(
                            400,
                            "InvalidInternetMessageHeader",
                            format!(
                                "The internet message header name '{name}' should start with 'x-' or 'X-'."
                            ),
                        ));
                    }
                    set_header(email, name, Some(text.to_owned()));
                }
            }
            "internetMessageId" => {
                email.message_id = value
                    .as_str()
                    .map(|id| id.trim_matches(['<', '>']).to_owned())
            }
            "attachments" => {
                for attachment in value.as_array().into_iter().flatten() {
                    email.attachments.push(file_attachment(attachment)?);
                }
            }
            _ => {}
        }
    }
    Ok(())
}

/// A `fileAttachment` from JSON.
fn file_attachment(json: &Value) -> Result<MimeAttachment> {
    let kind = json["@odata.type"]
        .as_str()
        .unwrap_or("#microsoft.graph.fileAttachment");
    if !kind.eq_ignore_ascii_case("#microsoft.graph.fileAttachment") {
        return Err(GraphError::new(
            400,
            "ErrorInvalidRequest",
            format!("Attachments of type '{kind}' aren't supported."),
        ));
    }
    let data = STANDARD
        .decode(json["contentBytes"].as_str().unwrap_or_default())
        .map_err(|_| GraphError::unreadable_body("contentBytes isn't base64"))?;
    let name = json["name"].as_str().unwrap_or("attachment").to_owned();
    let inline = json["isInline"].as_bool().unwrap_or(false);
    Ok(MimeAttachment {
        content_type: json["contentType"]
            .as_str()
            .map_or_else(|| guess_type(&name), str::to_owned),
        filename: name,
        data,
        content_id: json["contentId"]
            .as_str()
            .map(str::to_owned)
            .filter(|_| inline),
    })
}

fn guess_type(name: &str) -> String {
    match name
        .rsplit('.')
        .next()
        .map(str::to_ascii_lowercase)
        .as_deref()
    {
        Some("pdf") => "application/pdf",
        Some("png") => "image/png",
        Some("jpg" | "jpeg") => "image/jpeg",
        Some("txt") => "text/plain",
        Some("csv") => "text/csv",
        _ => "application/octet-stream",
    }
    .to_owned()
}

/// The Gmail draft holding a message, by its message ID.
fn draft_id(mail: &Mailbox, gmail: &str) -> Option<String> {
    mail.drafts()
        .into_iter()
        .find(|(_, message)| message.id == gmail)
        .map(|(draft, _)| draft.to_owned())
}

/// Saves `change`'s edit of draft `key`. Gmail makes a new message; the item follows it, with
/// the labels the old one had.
pub fn rewrite(call: &Call, key: u64, change: impl FnOnce(&mut Email) -> Result<()>) -> Result<()> {
    let mut world = call.world();
    save_draft(&mut world, key, change)
}

/// [`rewrite`] in a world already locked.
fn save_draft(
    world: &mut World,
    key: u64,
    change: impl FnOnce(&mut Email) -> Result<()>,
) -> Result<()> {
    let item = world
        .graph
        .items
        .get(&key)
        .ok_or_else(GraphError::not_found)?;
    let draft = draft_id(&world.mail, &item.gmail).ok_or_else(|| {
        GraphError::new(
            400,
            "ErrorInvalidRequest",
            "Only drafts can be changed this way.",
        )
    })?;
    let mut email = email_of(world.mail.message(&item.gmail)?, world);
    change(&mut email)?;
    commit_draft(world, key, &draft, email.to_rfc822())
}

/// Validates and renders `changes` against draft `key` without mutating `world`.
pub fn prepare_draft(
    world: &World,
    key: u64,
    changes: &Map<String, Value>,
) -> Result<(String, Vec<u8>)> {
    let item = world
        .graph
        .items
        .get(&key)
        .ok_or_else(GraphError::not_found)?;
    let draft = draft_id(&world.mail, &item.gmail).ok_or_else(|| {
        GraphError::new(
            400,
            "ErrorInvalidRequest",
            "Only drafts can be changed this way.",
        )
    })?;
    let mut email = email_of(world.mail.message(&item.gmail)?, world);
    apply_json(&mut email, changes)?;
    Ok((draft, email.to_rfc822()))
}

/// Persists a draft prepared by [`prepare_draft`], keeping its labels and Outlook key.
pub fn commit_draft(world: &mut World, key: u64, draft: &str, raw: Vec<u8>) -> Result<()> {
    let now = world.clock.now();
    let item = world
        .graph
        .items
        .get(&key)
        .ok_or_else(GraphError::not_found)?
        .clone();
    world
        .mail
        .update_draft(draft, raw, None, now.as_millisecond())?;
    let gmail = world.mail.draft(draft)?.id.clone();
    let kept: Vec<String> = item
        .labels
        .iter()
        .filter(|label| *label != "DRAFT")
        .cloned()
        .collect();
    world.mail.apply_labels(&gmail, &kept, &[]);
    world.graph.repoint(key, &gmail);
    world.graph.catch_up(&mut world.mail, now);
    world.graph.touch(key, now);
    Ok(())
}

/// `POST /me/messages/{id}/send`: sends a draft. Its copy in Sent Items keeps its immutable ID.
pub async fn send(call: Call, Path(params): super::Params) -> Result<Response> {
    let mut world = call.world();
    let now = world.clock.now();
    let key = {
        let origin = origin(&world);
        let view = call.view(&world, &origin);
        key_of(&view, &params["id"])?
    };
    let gmail = world
        .graph
        .items
        .get(&key)
        .map(|item| item.gmail.clone())
        .ok_or_else(GraphError::not_found)?;
    let draft = draft_id(&world.mail, &gmail)
        .ok_or_else(|| GraphError::new(400, "ErrorInvalidRequest", "Only drafts can be sent."))?;
    let sent = world
        .mail
        .send_draft(&draft, now.as_millisecond())
        .map_err(recipient_error)?;
    world.graph.repoint(key, &sent);
    let world_ref = &mut *world;
    world_ref.graph.catch_up(&mut world_ref.mail, now);
    Ok(StatusCode::ACCEPTED.into_response())
}

fn recipient_error(failure: crate::error::Failure) -> GraphError {
    if failure.error.message.contains("Recipient") {
        GraphError::new(
            400,
            "ErrorInvalidRecipients",
            "At least one recipient isn't valid.",
        )
    } else {
        failure.into()
    }
}

/// `POST /me/sendMail`: sends a message given as JSON (`{"message", "saveToSentItems"}`) or MIME.
/// Without `saveToSentItems`, no copy stays in Sent Items.
pub async fn send_mail(
    call: Call,
    headers: HeaderMap,
    body: axum::body::Bytes,
) -> Result<Response> {
    let (raw, save) = if is_mime(&headers) {
        (decode_mime(&body)?, true)
    } else {
        let request: Map<String, Value> = body_json(&body, "message")?;
        if let Some(unknown) = request
            .keys()
            .find(|k| *k != "message" && *k != "saveToSentItems")
        {
            return Err(GraphError::unknown_body_property(unknown, "sendMail"));
        }
        let message = request
            .get("message")
            .and_then(Value::as_object)
            .ok_or_else(|| GraphError::invalid_request("The message parameter is missing."))?;
        check_keys(message, CREATABLE)?;
        let save = match request.get("saveToSentItems") {
            Some(Value::Bool(save)) => *save,
            Some(Value::String(save)) => !save.eq_ignore_ascii_case("false"),
            _ => true,
        };
        let world = call.world();
        let mut email = blank(&world);
        apply_json(&mut email, message)?;
        (email.to_rfc822(), save)
    };
    let mut world = call.world();
    let now = world.clock.now();
    let thread = thread_for(&world.mail, &raw);
    let sent = world
        .mail
        .send(raw, thread, now.as_millisecond())
        .map_err(recipient_error)?;
    if !save {
        let to_self = world
            .mail
            .message(&sent)
            .is_ok_and(|m| m.labels.contains("INBOX"));
        if to_self {
            world.mail.apply_labels(&sent, &[], &["SENT".into()]);
        } else {
            world.mail.delete(&sent)?;
        }
    }
    let world_ref = &mut *world;
    world_ref.graph.catch_up(&mut world_ref.mail, now);
    Ok(StatusCode::ACCEPTED.into_response())
}

/// `POST /me/messages/{id}/createReply`.
pub async fn create_reply(
    call: Call,
    Path(params): super::Params,
    body: axum::body::Bytes,
) -> Result<Response> {
    respond(call, &params["id"], &body, Kind::Reply)
}

/// `POST /me/messages/{id}/createReplyAll`.
pub async fn create_reply_all(
    call: Call,
    Path(params): super::Params,
    body: axum::body::Bytes,
) -> Result<Response> {
    respond(call, &params["id"], &body, Kind::ReplyAll)
}

/// `POST /me/messages/{id}/createForward`.
pub async fn create_forward(
    call: Call,
    Path(params): super::Params,
    body: axum::body::Bytes,
) -> Result<Response> {
    respond(call, &params["id"], &body, Kind::Forward)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Kind {
    Reply,
    ReplyAll,
    Forward,
}

/// A reply or forward draft in the original's conversation, quoting it below Outlook's header.
fn respond(call: Call, id: &str, body: &[u8], kind: Kind) -> Result<Response> {
    let (raw, thread, overrides) = response_mail(&call, id, body, kind)?;
    let outlook_only: Map<String, Value> = overrides
        .into_iter()
        .filter(|(name, _)| OUTLOOK_ONLY.contains(&name.as_str()))
        .collect();
    let mut world = call.world();
    if !outlook_only.is_empty() {
        validate_patch(&world, &outlook_only, true)?;
    }
    let key = new_draft_locked(&mut world, raw, Some(thread))?;
    if !outlook_only.is_empty() {
        apply_patch_locked(&mut world, key, outlook_only)?;
    }
    draft_response(&call, &world, key)
}

/// `POST /me/messages/{id}/reply`.
pub async fn reply(
    call: Call,
    Path(params): super::Params,
    headers: HeaderMap,
    body: axum::body::Bytes,
) -> Result<Response> {
    send_response(call, &params["id"], &headers, &body, Kind::Reply)
}

/// `POST /me/messages/{id}/replyAll`.
pub async fn reply_all(
    call: Call,
    Path(params): super::Params,
    headers: HeaderMap,
    body: axum::body::Bytes,
) -> Result<Response> {
    send_response(call, &params["id"], &headers, &body, Kind::ReplyAll)
}

/// `POST /me/messages/{id}/forward`.
pub async fn forward(
    call: Call,
    Path(params): super::Params,
    headers: HeaderMap,
    body: axum::body::Bytes,
) -> Result<Response> {
    send_response(call, &params["id"], &headers, &body, Kind::Forward)
}

/// Sends a reply or forward at once (202), from JSON (a `comment`, a `message`'s changes,
/// `toRecipients`) or from MIME, in the original's conversation. A copy stays in Sent Items.
fn send_response(
    call: Call,
    id: &str,
    headers: &HeaderMap,
    body: &[u8],
    kind: Kind,
) -> Result<Response> {
    let (raw, thread) = if is_mime(headers) {
        let mut raw = decode_mime(body)?;
        let world = call.world();
        let origin = origin(&world);
        let view = call.view(&world, &origin);
        let key = key_of(&view, id)?;
        let (_, parent) = view.message(key)?;
        // A reply action answers the original, so MIME without threading headers gets them.
        let threaded =
            crate::mail::mime::parse(&raw, "probe").is_some_and(|m| !m.ancestors.is_empty());
        if let (false, Some(parent_id)) = (threaded, &parent.mime.rfc822_id) {
            let mut references: Vec<String> = parent
                .mime
                .ancestors
                .iter()
                .map(|id| format!("<{id}>"))
                .collect();
            references.push(format!("<{parent_id}>"));
            let headers = format!(
                "In-Reply-To: <{parent_id}>\r\nReferences: {}\r\n",
                references.join(" ")
            );
            raw.splice(0..0, headers.into_bytes());
        }
        (raw, parent.thread_id.clone())
    } else {
        let (raw, thread, _) = response_mail(&call, id, body, kind)?;
        (raw, thread)
    };
    let mut world = call.world();
    let now = world.clock.now();
    world
        .mail
        .send(raw, Some(thread), now.as_millisecond())
        .map_err(recipient_error)?;
    let world_ref = &mut *world;
    world_ref.graph.catch_up(&mut world_ref.mail, now);
    Ok(StatusCode::ACCEPTED.into_response())
}

/// The reply or forward to message `id` a request asks for: its MIME, its thread, and the
/// message properties the request set.
fn response_mail(
    call: &Call,
    id: &str,
    body: &[u8],
    kind: Kind,
) -> Result<(Vec<u8>, String, Map<String, Value>)> {
    let request: Map<String, Value> = body_json(body, "message")?;
    if let Some(unknown) = request
        .keys()
        .find(|k| !matches!(k.as_str(), "comment" | "message" | "toRecipients"))
    {
        return Err(GraphError::unknown_body_property(unknown, "message"));
    }
    let message = request.get("message");
    if request.contains_key("comment") && message.is_some_and(|m| m.get("body").is_some()) {
        return Err(GraphError::invalid_request(
            "Specify either a comment or the message's body, not both.",
        ));
    }
    let to = (
        request.contains_key("toRecipients"),
        message.is_some_and(|m| m.get("toRecipients").is_some()),
    );
    if to == (true, true) {
        return Err(GraphError::invalid_request(
            "Specify either toRecipients or the message's toRecipients, not both.",
        ));
    }
    let (raw, thread, overrides) = {
        let world = call.world();
        let origin = origin(&world);
        let view = call.view(&world, &origin);
        let key = key_of(&view, id)?;
        let (_, parent) = view.message(key)?;
        let me = world.account.email.to_lowercase();
        let header = |name: &str| parent.mime.payload.header(name).unwrap_or_default();
        let mut email = blank(&world);
        let prefix = if kind == Kind::Forward {
            "FW: "
        } else {
            "RE: "
        };
        let has_prefix = parent
            .mime
            .subject
            .get(..prefix.len())
            .is_some_and(|p| p.eq_ignore_ascii_case(prefix));
        email.subject = if has_prefix {
            parent.mime.subject.clone()
        } else {
            format!("{prefix}{}", parent.mime.subject)
        };
        let not_me = |address: &Address| !address.email.eq_ignore_ascii_case(&me);
        match kind {
            Kind::Reply | Kind::ReplyAll => {
                let reply_to = Address::parse_list(header("Reply-To"));
                email.to = if reply_to.is_empty() {
                    Address::parse_list(header("From"))
                } else {
                    reply_to
                };
                if kind == Kind::ReplyAll {
                    email
                        .to
                        .extend(Address::parse_list(header("To")).into_iter().filter(not_me));
                    email.cc = Address::parse_list(header("Cc"))
                        .into_iter()
                        .filter(not_me)
                        .collect();
                }
                email.to.retain(not_me);
                if email.to.is_empty() {
                    email.to = Address::parse_list(header("To"));
                }
            }
            Kind::Forward => {
                email.attachments = email_of(parent, &world).attachments;
            }
        }
        email.reply_to_message(&parent.to_wire(Format::Metadata, &[]));
        let comment = request
            .get("comment")
            .and_then(Value::as_str)
            .unwrap_or_default();
        let (html, text) = project::bodies(&parent.mime.payload);
        let quoted = html.unwrap_or_else(|| project::text_to_html(&text.unwrap_or_default()));
        let sent = parent
            .mime
            .date
            .and_then(|ms| Timestamp::from_millisecond(ms).ok())
            .map(|at| {
                at.to_zoned(world.account.tz.clone())
                    .strftime("%A, %B %-d, %Y %-I:%M %p")
                    .to_string()
            })
            .unwrap_or_default();
        let escape = crate::mail::mime::escape_html_text;
        let html = format!(
            "<html><head>\r\n<meta http-equiv=\"Content-Type\" content=\"text/html; charset=utf-8\"></head><body><div>{}</div><hr style=\"display:inline-block;width:98%\" tabindex=\"-1\"><div id=\"divRplyFwdMsg\" dir=\"ltr\"><font face=\"Calibri, sans-serif\" style=\"font-size:11pt\" color=\"#000000\"><b>From:</b> {}<br><b>Sent:</b> {sent}<br><b>To:</b> {}<br><b>Subject:</b> {}</font><div>&nbsp;</div></div>{}</body></html>",
            escape(comment),
            escape(header("From")),
            escape(header("To")),
            escape(&parent.mime.subject),
            body_of(&quoted),
        );
        email.text = Some(mail_parser::decoders::html::html_to_text(&html));
        email.html = Some(html);
        let mut overrides = request
            .get("message")
            .and_then(Value::as_object)
            .cloned()
            .unwrap_or_default();
        if let Some(to) = request.get("toRecipients") {
            overrides.insert("toRecipients".into(), to.clone());
        }
        check_keys(&overrides, CREATABLE)?;
        apply_json(&mut email, &overrides)?;
        (email.to_rfc822(), parent.thread_id.clone(), overrides)
    };
    Ok((raw, thread, overrides))
}

/// The inside of an HTML document's `<body>`, to quote it.
fn body_of(html: &str) -> &str {
    let lower = html.to_ascii_lowercase();
    let start = lower
        .find("<body")
        .and_then(|at| lower[at..].find('>').map(|end| at + end + 1));
    let end = lower.rfind("</body>");
    match (start, end) {
        (Some(start), Some(end)) if start <= end => &html[start..end],
        _ => html,
    }
}

/// The attachment an ID names on message `key`.
fn attachment_of(view: &project::View<'_>, key: u64, id: &str) -> Result<project::Attachment> {
    let Some(Id::Attachment {
        key: owner,
        generation,
        index,
    }) = ids::decode(id)
    else {
        return Err(GraphError::malformed_id());
    };
    let (item, message) = view.message(key)?;
    if owner != key || generation.is_some_and(|g| g != item.generation) {
        return Err(GraphError::not_found());
    }
    project::attachments(message)
        .into_iter()
        .find(|attachment| attachment.index == index)
        .ok_or_else(GraphError::not_found)
}

/// `GET /me/messages/{id}/attachments`.
pub async fn list_attachments(call: Call, Path(params): super::Params) -> Result<Response> {
    let world = call.world();
    let origin = origin(&world);
    let view = call.view(&world, &origin);
    let key = key_of(&view, &params["id"])?;
    let (item, message) = view.message(key)?;
    let value = project::attachments(message)
        .iter()
        .map(|attachment| {
            odata::select(
                view.attachment_json(key, item, attachment, true),
                call.options.select.as_deref(),
                project::ATTACHMENT_PROPERTIES,
                "attachment",
            )
            .map(Value::Object)
        })
        .collect::<Result<Vec<_>>>()?;
    let context = view.context(&format!(
        "messages('{}')/attachments",
        view.message_id(key, item)
    ));
    Ok(super::reply(
        200,
        odata::collection(context, value, None, None),
    ))
}

/// `GET /me/messages/{id}/attachments/{id}`.
pub async fn get_attachment(call: Call, Path(params): super::Params) -> Result<Response> {
    let world = call.world();
    let origin = origin(&world);
    let view = call.view(&world, &origin);
    let key = key_of(&view, &params["id"])?;
    let attachment = attachment_of(&view, key, &params["attachment"])?;
    let (item, _) = view.message(key)?;
    let json = odata::select(
        view.attachment_json(key, item, &attachment, true),
        call.options.select.as_deref(),
        project::ATTACHMENT_PROPERTIES,
        "attachment",
    )?;
    let context = view.context(&format!(
        "messages('{}')/attachments/$entity",
        view.message_id(key, item)
    ));
    Ok(reply_map(200, entity(context, json)))
}

/// `GET /me/messages/{id}/attachments/{id}/$value`: the file's bytes.
pub async fn attachment_value(call: Call, Path(params): super::Params) -> Result<Response> {
    let world = call.world();
    let origin = origin(&world);
    let view = call.view(&world, &origin);
    let key = key_of(&view, &params["id"])?;
    let attachment = attachment_of(&view, key, &params["attachment"])?;
    let content_type = HeaderValue::from_str(&attachment.content_type)
        .unwrap_or(HeaderValue::from_static("application/octet-stream"));
    Ok(([(header::CONTENT_TYPE, content_type)], attachment.bytes).into_response())
}

/// `POST /me/messages/{id}/attachments`: a file attachment of up to 3 MB on a draft.
pub async fn add_attachment(
    call: Call,
    Path(params): super::Params,
    body: axum::body::Bytes,
) -> Result<Response> {
    let json: Value = serde_json::from_slice(&body).map_err(GraphError::unreadable_body)?;
    let attachment = file_attachment(&json)?;
    if attachment.data.len() > SMALL_ATTACHMENT {
        return Err(GraphError::new(
            413,
            "ErrorRequestEntityTooLarge",
            "The attachment is too large for a single request. Create an upload session for files over 3 MB.",
        ));
    }
    let key = {
        let world = call.world();
        let origin = origin(&world);
        let view = call.view(&world, &origin);
        key_of(&view, &params["id"])?
    };
    attach(&call, key, attachment)
}

/// Adds `attachment` to draft `key`, answering with it.
fn attach(call: &Call, key: u64, attachment: MimeAttachment) -> Result<Response> {
    rewrite(call, key, |email| {
        email.attachments.push(attachment);
        Ok(())
    })?;
    let world = call.world();
    let origin = origin(&world);
    let view = call.view(&world, &origin);
    let (item, message) = view.message(key)?;
    let added = project::attachments(message)
        .pop()
        .ok_or_else(GraphError::not_found)?;
    let context = view.context(&format!(
        "messages('{}')/attachments/$entity",
        view.message_id(key, item)
    ));
    Ok(reply_map(
        201,
        entity(context, view.attachment_json(key, item, &added, true)),
    ))
}

/// `DELETE /me/messages/{id}/attachments/{id}`.
pub async fn delete_attachment(call: Call, Path(params): super::Params) -> Result<Response> {
    let (key, index) = {
        let world = call.world();
        let origin = origin(&world);
        let view = call.view(&world, &origin);
        let key = key_of(&view, &params["id"])?;
        (key, attachment_of(&view, key, &params["attachment"])?.index)
    };
    rewrite(&call, key, |email| {
        if usize::from(index) < email.attachments.len() {
            email.attachments.remove(usize::from(index));
        }
        Ok(())
    })?;
    Ok(StatusCode::NO_CONTENT.into_response())
}

/// `POST /me/messages/{id}/attachments/createUploadSession`: a session for a file of 3 MB to
/// 150 MB, uploaded in ranges to its own pre-authenticated URL.
pub async fn create_upload_session(
    call: Call,
    Path(params): super::Params,
    body: axum::body::Bytes,
) -> Result<Response> {
    let request: Map<String, Value> = body_json(&body, "attachmentItem")?;
    let item = request
        .iter()
        .find(|(name, _)| name.eq_ignore_ascii_case("attachmentItem"))
        .map(|(_, value)| value.clone())
        .ok_or_else(|| GraphError::invalid_request("The AttachmentItem parameter is missing."))?;
    let size = item["size"]
        .as_u64()
        .and_then(|s| usize::try_from(s).ok())
        .unwrap_or_default();
    if size < SMALL_ATTACHMENT {
        return Err(GraphError::new(
            400,
            "ErrorAttachmentSizeShouldNotBeLessThanMinimumSize",
            "Attachment size must be greater than the minimum size.",
        ));
    }
    if size > LARGE_ATTACHMENT {
        return Err(GraphError::new(
            400,
            "ErrorAttachmentSizeShouldNotBeMoreThanMaximumSize",
            "Attachment size must not be more than 150 MB.",
        ));
    }
    let mut world = call.world();
    let now = world.clock.now();
    let (key, message_id) = {
        let origin = origin(&world);
        let view = call.view(&world, &origin);
        let key = key_of(&view, &params["id"])?;
        let (item, _) = view.message(key)?;
        if !item.labels.contains("DRAFT") {
            return Err(GraphError::new(
                400,
                "ErrorInvalidRequest",
                "Attachments can only be uploaded to drafts.",
            ));
        }
        (key, params["id"].clone())
    };
    world.graph.next_upload += 1;
    let number = world.graph.next_upload;
    let name = item["name"].as_str().unwrap_or("attachment").to_owned();
    let upload = Upload {
        key,
        user: format!("{}@{}", call.who.id, call.who.tenant),
        message_id: message_id.clone(),
        token: STANDARD_NO_PAD
            .encode(format!("upload {number} {}", now.as_millisecond()))
            .replace(['+', '/'], "_"),
        content_type: item["contentType"]
            .as_str()
            .map_or_else(|| guess_type(&name), str::to_owned),
        name,
        inline: item["isInline"].as_bool().unwrap_or(false),
        content_id: item["contentId"].as_str().map(str::to_owned),
        size,
        bytes: Vec::new(),
        expires: now
            .checked_add(SignedDuration::from_mins(15))
            .unwrap_or(now),
    };
    let origin = origin(&world);
    let url = upload_url(&origin, number, &upload);
    let expires = upload.expires;
    world.graph.uploads.insert(number, upload);
    Ok(super::reply(
        201,
        json!({
            "@odata.context": format!("{origin}v1.0/$metadata#microsoft.graph.uploadSession"),
            "uploadUrl": url,
            "expirationDateTime": odata::datetime(expires),
            "nextExpectedRanges": ["0-"],
        }),
    ))
}

fn upload_url(origin: &url::Url, number: u64, upload: &Upload) -> String {
    format!(
        "{origin}api/v2.0/Users('{}')/Messages('{}')/AttachmentSessions('AAMkAGI2U{number}')?authtoken={}",
        upload.user, upload.message_id, upload.token
    )
}

/// The upload session a pre-authenticated URL names.
fn session_of(world: &World, path: &str, query: Option<&str>) -> Result<u64> {
    let number: u64 = path
        .split("AttachmentSessions('AAMkAGI2U")
        .nth(1)
        .and_then(|rest| rest.split('\'').next())
        .and_then(|n| n.parse().ok())
        .ok_or_else(GraphError::not_found)?;
    let upload = world
        .graph
        .uploads
        .get(&number)
        .ok_or_else(GraphError::not_found)?;
    let token = query
        .and_then(|q| {
            url::form_urlencoded::parse(q.as_bytes()).find(|(name, _)| name == "authtoken")
        })
        .map(|(_, token)| token.into_owned());
    if token.as_deref() != Some(upload.token.as_str()) {
        return Err(GraphError::new(
            401,
            "InvalidAuthenticationToken",
            "The upload session's token is invalid.",
        ));
    }
    if world.clock.now() > upload.expires {
        return Err(GraphError::new(
            404,
            "ErrorItemNotFound",
            "The upload session has expired.",
        ));
    }
    Ok(number)
}

/// `PUT` to an upload session's URL: the next range of the file, in order, at most 4 MB. The
/// last one attaches the file to the draft and answers `201` with the attachment's URL.
pub async fn upload(
    State(app): State<Arc<App>>,
    uri: axum::http::Uri,
    headers: HeaderMap,
    body: axum::body::Bytes,
) -> Response {
    match upload_range(&app, &uri, &headers, &body) {
        Ok(response) => response,
        Err(error) => error.into_response(),
    }
}

fn upload_range(
    app: &Arc<App>,
    uri: &axum::http::Uri,
    headers: &HeaderMap,
    body: &[u8],
) -> Result<Response> {
    let decoded = super::percent(uri.path());
    let mut world = super::locked(app);
    let number = session_of(&world, &decoded, uri.query())?;
    let range = headers
        .get(header::CONTENT_RANGE)
        .and_then(|value| value.to_str().ok())
        .and_then(parse_range)
        .ok_or_else(|| {
            GraphError::bad_request("The Content-Range header is missing or malformed.")
        })?;
    let origin = super::origin(&world);
    let upload = world
        .graph
        .uploads
        .get_mut(&number)
        .ok_or_else(GraphError::not_found)?;
    let (start, end, total) = range;
    let bad_range = || GraphError {
        inner_code: Some("invalidRange"),
        ..GraphError::new(
            400,
            "badRequest",
            "Uploaded fragment overlaps with existing data.",
        )
    };
    let range_len = end.checked_sub(start).and_then(|diff| diff.checked_add(1));
    if total != upload.size
        || start != upload.bytes.len()
        || end >= total
        || range_len != Some(body.len())
    {
        return Err(bad_range());
    }
    if body.len() > UPLOAD_CHUNK {
        return Err(GraphError::new(
            413,
            "ErrorRequestEntityTooLarge",
            "Upload each range in at most 4 MB.",
        ));
    }
    upload.bytes.extend_from_slice(body);
    if upload.bytes.len() < upload.size {
        let next = upload.bytes.len();
        let context = format!(
            "{origin}api/v2.0/$metadata#Users('{}')/Messages('{}')/AttachmentSessions/$entity",
            upload.user, upload.message_id
        );
        return Ok(super::reply(
            200,
            json!({
                "@odata.context": context,
                "ExpirationDateTime": odata::datetime(upload.expires),
                "nextExpectedRanges": [format!("{next}-")],
            }),
        ));
    }
    let upload = world
        .graph
        .uploads
        .remove(&number)
        .ok_or_else(GraphError::not_found)?;
    let mut index = 0;
    save_draft(&mut world, upload.key, |email| {
        email.attachments.push(MimeAttachment {
            filename: upload.name.clone(),
            content_type: upload.content_type.clone(),
            data: upload.bytes,
            content_id: upload.content_id.filter(|_| upload.inline),
        });
        index = u16::try_from(email.attachments.len() - 1).unwrap_or(u16::MAX);
        Ok(())
    })?;
    let generation = world
        .graph
        .items
        .get(&upload.key)
        .map_or(0, |item| item.generation);
    let attachment = ids::attachment(upload.key, generation, index, false);
    let location = format!(
        "{origin}api/v2.0/Users('{}')/Messages('{}')/Attachments('{attachment}')",
        upload.user, upload.message_id
    );
    let mut response = StatusCode::CREATED.into_response();
    if let Ok(location) = HeaderValue::from_str(&location) {
        response.headers_mut().insert(header::LOCATION, location);
    }
    Ok(response)
}

/// `bytes 0-4194303/10485760` → (0, 4194303, 10485760).
fn parse_range(value: &str) -> Option<(usize, usize, usize)> {
    let rest = value.trim().strip_prefix("bytes ")?;
    let (span, total) = rest.split_once('/')?;
    let (start, end) = span.split_once('-')?;
    Some((
        start.trim().parse().ok()?,
        end.trim().parse().ok()?,
        total.trim().parse().ok()?,
    ))
}

/// `DELETE` on an upload session's URL: cancels it.
pub async fn cancel_upload(State(app): State<Arc<App>>, uri: axum::http::Uri) -> Response {
    let decoded = super::percent(uri.path());
    let mut world = super::locked(&app);
    match session_of(&world, &decoded, uri.query()) {
        Ok(number) => {
            world.graph.uploads.remove(&number);
            StatusCode::NO_CONTENT.into_response()
        }
        Err(error) => error.into_response(),
    }
}

#[cfg(test)]
mod tests {
    use axum::body::{Body, to_bytes};
    use axum::http::{Method, Request};
    use serde_json::{Value, json};
    use tower::ServiceExt;

    async fn call(
        router: &axum::Router,
        method: Method,
        uri: &str,
        headers: &[(&str, &str)],
        body: Vec<u8>,
    ) -> (u16, Value) {
        let mut req = Request::builder().method(method).uri(uri);
        for (name, value) in headers {
            req = req.header(*name, *value);
        }
        let response = router
            .clone()
            .oneshot(req.body(Body::from(body)).unwrap())
            .await
            .unwrap();
        let status = response.status().as_u16();
        let bytes = to_bytes(response.into_body(), usize::MAX).await.unwrap();
        let json = serde_json::from_slice(&bytes).unwrap_or(Value::Null);
        (status, json)
    }

    #[tokio::test]
    async fn rejected_draft_create_leaves_no_ghost_draft() {
        let router = crate::router(&crate::Config::default());
        let auth = format!("Bearer {}", crate::DEV_ACCESS_TOKEN);
        let (_, before) = call(
            &router,
            Method::GET,
            "/v1.0/me/mailFolders/drafts/messages?$top=100",
            &[("Authorization", &auth)],
            Vec::new(),
        )
        .await;
        let count_before = before["value"].as_array().unwrap().len();

        let (status, err) = call(
            &router,
            Method::POST,
            "/v1.0/me/messages",
            &[
                ("Authorization", &auth),
                ("Content-Type", "application/json"),
            ],
            json!({
                "subject": "Ghost draft",
                "flag": { "flagStatus": "bogus" }
            })
            .to_string()
            .into_bytes(),
        )
        .await;
        assert_eq!(status, 400, "{err}");

        let (_, after) = call(
            &router,
            Method::GET,
            "/v1.0/me/mailFolders/drafts/messages?$top=100",
            &[("Authorization", &auth)],
            Vec::new(),
        )
        .await;
        assert_eq!(after["value"].as_array().unwrap().len(), count_before);
        assert!(
            after["value"]
                .as_array()
                .unwrap()
                .iter()
                .all(|m| m["subject"] != "Ghost draft")
        );
    }

    #[tokio::test]
    async fn upload_range_enforces_bounds_and_formats_next_expected_ranges() {
        let router = crate::router(&crate::Config::default());
        let auth = format!("Bearer {}", crate::DEV_ACCESS_TOKEN);
        let (status, draft) = call(
            &router,
            Method::POST,
            "/v1.0/me/messages",
            &[
                ("Authorization", &auth),
                ("Content-Type", "application/json"),
            ],
            json!({ "subject": "Big attachment" })
                .to_string()
                .into_bytes(),
        )
        .await;
        assert_eq!(status, 201, "{draft}");
        let id = draft["id"].as_str().unwrap();

        let total = 3 * 1024 * 1024 + 2048;
        let (status, session) = call(
            &router,
            Method::POST,
            &format!("/v1.0/me/messages/{id}/attachments/createUploadSession"),
            &[
                ("Authorization", &auth),
                ("Content-Type", "application/json"),
            ],
            json!({
                "AttachmentItem": {
                    "attachmentType": "file",
                    "name": "big.bin",
                    "size": total,
                    "contentType": "application/octet-stream"
                }
            })
            .to_string()
            .into_bytes(),
        )
        .await;
        assert_eq!(status, 201, "{session}");
        assert_eq!(session["nextExpectedRanges"], json!(["0-"]));
        let upload_path = session["uploadUrl"]
            .as_str()
            .unwrap()
            .split("/api/v2.0")
            .nth(1)
            .unwrap();
        let upload_uri = format!("/api/v2.0{upload_path}");

        // Overflowing end (usize::MAX) must return 400 invalidRange without panicking.
        let (status, err) = call(
            &router,
            Method::PUT,
            &upload_uri,
            &[("Content-Range", &format!("bytes 0-{}/{total}", usize::MAX))],
            vec![0u8; 16],
        )
        .await;
        assert_eq!(status, 400, "{err}");

        // end == total (out of bounds past total - 1) must be rejected.
        let (status, err) = call(
            &router,
            Method::PUT,
            &upload_uri,
            &[("Content-Range", &format!("bytes 0-{total}/{total}"))],
            vec![0u8; 16],
        )
        .await;
        assert_eq!(status, 400, "{err}");

        // Valid first chunk returns nextExpectedRanges formatted as "{next}-".
        let chunk = 1024;
        let (status, progress) = call(
            &router,
            Method::PUT,
            &upload_uri,
            &[("Content-Range", &format!("bytes 0-{}/{total}", chunk - 1))],
            vec![7u8; chunk],
        )
        .await;
        assert_eq!(status, 200, "{progress}");
        assert_eq!(progress["nextExpectedRanges"], json!([format!("{chunk}-")]));

        // A final range extending to end == total (1 byte past the file end) is rejected.
        let (status, err) = call(
            &router,
            Method::PUT,
            &upload_uri,
            &[("Content-Range", &format!("bytes {chunk}-{total}/{total}"))],
            vec![7u8; total - chunk + 1],
        )
        .await;
        assert_eq!(status, 400, "{err}");
    }
}

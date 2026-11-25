//! Messages: listing (with `$filter`, `$orderby`, `$search` and paging), reading (JSON and
//! MIME), changing what Outlook lets a message change, moving, deleting, and each folder's delta.

use std::cmp::Ordering;

use axum::extract::Path;
use axum::http::{StatusCode, header};
use axum::response::{IntoResponse, Response};
use base64::Engine;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value, json};

use super::error::{GraphError, Result};
use super::filter::Expr;
use super::folders::{body_json, resolve, resolve_target};
use super::ids::{self, Id};
use super::kql::{Doc, Kql};
use super::model::{self, Folder};
use super::project::{self, View};
use super::{Call, compose, odata, origin, reply, reply_map};

/// The most messages a page holds.
const MAX_PAGE: usize = 1000;
/// The most messages a search finds.
const MAX_SEARCH: usize = 1000;

/// The item a message ID names. A default ID from before the message last moved names nothing,
/// as in Outlook.
pub fn key_of(view: &View<'_>, id: &str) -> Result<u64> {
    match ids::decode(id) {
        Some(Id::Message { key, generation }) => {
            let item = view
                .outlook
                .items
                .get(&key)
                .ok_or_else(GraphError::not_found)?;
            if generation.is_some_and(|generation| generation != item.generation) {
                return Err(GraphError::not_found());
            }
            Ok(key)
        }
        _ => Err(GraphError::malformed_id()),
    }
}

/// `GET /me/messages`: every folder's messages.
pub async fn list(call: Call) -> Result<Response> {
    let world = call.world();
    let origin = origin(&world);
    let view = call.view(&world, &origin);
    list_messages(&call, &view, None)
}

/// `GET /me/mailFolders/{id}/messages`.
pub async fn list_in_folder(call: Call, Path(params): super::Params) -> Result<Response> {
    let world = call.world();
    let origin = origin(&world);
    let view = call.view(&world, &origin);
    let folder = resolve(&view, &params["folder"])?;
    list_messages(&call, &view, Some(folder))
}

fn list_messages(call: &Call, view: &View<'_>, scope: Option<Folder>) -> Result<Response> {
    let options = &call.options;
    if options.search.is_some() && (options.filter.is_some() || options.orderby.is_some()) {
        return Err(GraphError::new(
            400,
            "ErrorInvalidUrlQuery",
            "The query parameter '$search' can't be combined with '$filter' or '$orderby'.",
        ));
    }
    let filter = options.filter.as_deref().map(Expr::parse).transpose()?;
    let order = parse_orderby(
        options.orderby.as_deref(),
        project::MESSAGE_PROPERTIES,
        "message",
    )?;
    if let Some(filter) = &filter
        && !order.is_empty()
    {
        let filtered = filter.properties();
        let sorted: Vec<String> = order.iter().map(|(path, _)| path[0].clone()).collect();
        if filtered.len() < sorted.len() || filtered[..sorted.len()] != sorted[..] {
            return Err(GraphError::new(
                400,
                "InefficientFilter",
                "The restriction or sort order is too complex for this operation.",
            ));
        }
    }
    let mut needed: Vec<String> = filter.as_ref().map(Expr::properties).unwrap_or_default();
    for (path, _) in &order {
        if !needed.contains(&path[0]) {
            needed.push(path[0].clone());
        }
    }
    odata::check_properties(&needed, project::MESSAGE_PROPERTIES, "message")?;
    needed.push("receivedDateTime".into());
    needed.push("sentDateTime".into());
    let search = options.search.as_deref().map(Kql::parse).transpose()?;

    let mut rows: Vec<(u64, Map<String, Value>)> = Vec::new();
    for (&key, item) in &view.outlook.items {
        if scope.as_ref().is_some_and(|folder| folder != &item.folder) {
            continue;
        }
        if let Some(search) = &search {
            let Ok((_, message)) = view.message(key) else {
                continue;
            };
            let doc = Doc {
                message,
                importance: project::importance(message),
                tz: view.tz,
            };
            if !search.matches(&doc) {
                continue;
            }
        }
        let Ok(json) = view.message_json(key, Some(&needed)) else {
            continue;
        };
        if filter
            .as_ref()
            .is_none_or(|filter| filter.matches(&Value::Object(json.clone())))
        {
            rows.push((key, json));
        }
    }
    let order = if search.is_some() {
        vec![(vec!["sentDateTime".to_owned()], true)]
    } else if order.is_empty() {
        vec![(vec!["receivedDateTime".to_owned()], true)]
    } else {
        order
    };
    rows.sort_by(|(a_key, a), (b_key, b)| {
        order
            .iter()
            .map(|(path, descending)| {
                let ordering = compare_values(&lookup(a, path), &lookup(b, path));
                if *descending {
                    ordering.reverse()
                } else {
                    ordering
                }
            })
            .find(|ordering| *ordering != Ordering::Equal)
            .unwrap_or_else(|| b_key.cmp(a_key))
    });
    if search.is_some() {
        rows.truncate(MAX_SEARCH);
    }

    let total = rows.len();
    let top = options.top.unwrap_or(10).clamp(1, MAX_PAGE);
    let skip = options.skip;
    let expand = Expand::parse(options.expand.as_deref())?;
    let mut value = Vec::new();
    for (key, _) in rows.iter().skip(skip).take(top) {
        let mut json = view.message_json(*key, options.select.as_deref())?;
        expand.apply(view, *key, &mut json)?;
        value.push(Value::Object(json));
    }
    let path = match &scope {
        Some(folder) => format!("me/mailFolders/{}/messages", ids::folder(folder)),
        None => "me/messages".to_owned(),
    };
    let next = (skip + top < total).then(|| {
        odata::link(
            view.origin,
            &path,
            &odata::next_page(&call.query, skip + top),
        )
    });
    let fragment = match &scope {
        Some(folder) => format!("mailFolders('{}')/messages", ids::folder(folder)),
        None => "messages".to_owned(),
    };
    let fragment = match &options.select {
        Some(select) => format!("{fragment}({})", select.join(",")),
        None => fragment,
    };
    Ok(reply(
        200,
        odata::collection(
            view.context(&fragment),
            value,
            options.count.then_some(total),
            next,
        ),
    ))
}

/// `$orderby`'s clauses over `kind`'s properties (`known`): property paths and whether each is
/// descending.
pub fn parse_orderby(
    text: Option<&str>,
    known: &[&str],
    kind: &str,
) -> Result<Vec<(Vec<String>, bool)>> {
    let Some(text) = text else {
        return Ok(Vec::new());
    };
    text.split(',')
        .map(|clause| {
            let mut words = clause.split_whitespace();
            let path: Vec<String> = words
                .next()
                .ok_or_else(|| GraphError::bad_request("Invalid $orderby clause"))?
                .split('/')
                .map(str::to_owned)
                .collect();
            if !known.iter().any(|p| p.eq_ignore_ascii_case(&path[0])) {
                return Err(GraphError::unknown_property(&path[0], kind));
            }
            let descending = match words.next().map(str::to_ascii_lowercase).as_deref() {
                None | Some("asc") => false,
                Some("desc") => true,
                Some(_) => return Err(GraphError::bad_request("Invalid $orderby clause")),
            };
            Ok((path, descending))
        })
        .collect()
}

/// The value at `path` in `json`, or null.
pub fn lookup(json: &Map<String, Value>, path: &[String]) -> Value {
    let mut value = json.get(&path[0]).cloned().unwrap_or(Value::Null);
    for segment in &path[1..] {
        value = value.get(segment).cloned().unwrap_or(Value::Null);
    }
    value
}

/// How `$orderby` orders two values: strings without regard to case, nulls first.
pub fn compare_values(a: &Value, b: &Value) -> Ordering {
    match (a, b) {
        (Value::String(a), Value::String(b)) => a.to_lowercase().cmp(&b.to_lowercase()),
        (Value::Bool(a), Value::Bool(b)) => a.cmp(b),
        (Value::Number(a), Value::Number(b)) => a
            .as_f64()
            .partial_cmp(&b.as_f64())
            .unwrap_or(Ordering::Equal),
        (Value::Null, Value::Null) => Ordering::Equal,
        (Value::Null, _) => Ordering::Less,
        (_, Value::Null) => Ordering::Greater,
        _ => Ordering::Equal,
    }
}

/// `$expand`: the navigation properties a message response carries inline.
#[derive(Debug, Default)]
pub struct Expand {
    attachments: Option<Option<Vec<String>>>,
    /// A meeting message's event (`microsoft.graph.eventMessage/event`), with its `$select`.
    event: Option<Option<Vec<String>>>,
    empty: Vec<String>,
}

impl Expand {
    pub fn parse(text: Option<&str>) -> Result<Self> {
        let mut expand = Self::default();
        let Some(text) = text else {
            return Ok(expand);
        };
        let mut depth = 0;
        let mut start = 0;
        let mut items = Vec::new();
        for (at, ch) in text.char_indices() {
            match ch {
                '(' => depth += 1,
                ')' => depth -= 1,
                ',' if depth == 0 => {
                    items.push(&text[start..at]);
                    start = at + 1;
                }
                _ => {}
            }
        }
        items.push(&text[start..]);
        for item in items
            .into_iter()
            .map(str::trim)
            .filter(|item| !item.is_empty())
        {
            let (name, inner) = match item.split_once('(') {
                Some((name, inner)) => (name.trim(), Some(inner.trim_end_matches(')'))),
                None => (item, None),
            };
            let select = inner.and_then(|inner| {
                inner.split(';').find_map(|option| {
                    let (key, value) = option.split_once('=')?;
                    key.trim().eq_ignore_ascii_case("$select").then(|| {
                        value
                            .split(',')
                            .map(|v| v.trim().to_owned())
                            .collect::<Vec<_>>()
                    })
                })
            });
            match name.trim_start_matches("microsoft.graph.") {
                "eventMessage/event"
                | "eventMessageRequest/event"
                | "eventMessageResponse/event" => {
                    if let Some(select) = &select {
                        odata::check_properties(select, super::events::EVENT_PROPERTIES, "event")?;
                    }
                    expand.event = Some(select);
                }
                "attachments" => {
                    if let Some(select) = &select {
                        odata::check_properties(
                            select,
                            project::ATTACHMENT_PROPERTIES,
                            "attachment",
                        )?;
                    }
                    expand.attachments = Some(select);
                }
                "extensions" | "singleValueExtendedProperties" | "multiValueExtendedProperties" => {
                    expand.empty.push(name.to_owned());
                }
                other => return Err(GraphError::unknown_property(other, "message")),
            }
        }
        Ok(expand)
    }

    pub fn apply(&self, view: &View<'_>, key: u64, json: &mut Map<String, Value>) -> Result<()> {
        if let Some(select) = &self.attachments {
            let (item, message) = view.message(key)?;
            let attachments: Vec<Value> = project::attachments(message)
                .iter()
                .map(|attachment| {
                    let json = view.attachment_json(key, item, attachment, true);
                    odata::select(
                        json,
                        select.as_deref(),
                        project::ATTACHMENT_PROPERTIES,
                        "attachment",
                    )
                    .map(Value::Object)
                })
                .collect::<Result<_>>()?;
            json.insert("attachments".into(), attachments.into());
        }
        if let Some(select) = &self.event {
            let (_, message) = view.message(key)?;
            if let Some(itip) = super::events::meeting(message) {
                let event = match view.event_by_uid(&itip.uid) {
                    Some((calendar, event)) => {
                        Value::Object(view.event_json(&calendar, &event, select.as_deref())?)
                    }
                    None => Value::Null,
                };
                json.insert("event".into(), event);
            }
        }
        for name in &self.empty {
            json.insert(name.clone(), Value::Array(Vec::new()));
        }
        Ok(())
    }
}

/// `GET /me/messages/{id}`.
pub async fn get(call: Call, Path(params): super::Params) -> Result<Response> {
    let world = call.world();
    let origin = origin(&world);
    let view = call.view(&world, &origin);
    let scope = params
        .get("folder")
        .map(|folder| resolve(&view, folder))
        .transpose()?;
    let key = key_of(&view, &params["id"])?;
    if let Some(folder) = scope
        && view.message(key)?.0.folder != folder
    {
        return Err(GraphError::not_found());
    }
    let expand = Expand::parse(call.options.expand.as_deref())?;
    let mut json = view.message_json(key, call.options.select.as_deref())?;
    expand.apply(&view, key, &mut json)?;
    let fragment = match &call.options.select {
        Some(select) => format!("messages({})/$entity", select.join(",")),
        None => "messages/$entity".to_owned(),
    };
    Ok(reply_map(200, entity(view.context(&fragment), json)))
}

/// A single resource with its `@odata.context` first.
pub fn entity(context: String, json: Map<String, Value>) -> Map<String, Value> {
    let mut out = Map::new();
    out.insert("@odata.context".into(), context.into());
    out.extend(json);
    out
}

/// `GET /me/messages/{id}/$value`: the message as MIME.
pub async fn mime(call: Call, Path(params): super::Params) -> Result<Response> {
    let world = call.world();
    let origin = origin(&world);
    let view = call.view(&world, &origin);
    let key = key_of(&view, &params["id"])?;
    let (_, message) = view.message(key)?;
    Ok(([(header::CONTENT_TYPE, "text/plain")], message.raw.clone()).into_response())
}

/// The properties a message's PATCH may set; those after `subject` only on drafts.
const UPDATABLE: &[&str] = &[
    "categories",
    "flag",
    "importance",
    "inferenceClassification",
    "isDeliveryReceiptRequested",
    "isRead",
    "isReadReceiptRequested",
    "singleValueExtendedProperties",
    "multiValueExtendedProperties",
    "subject",
    "body",
    "toRecipients",
    "ccRecipients",
    "bccRecipients",
    "replyTo",
    "from",
    "sender",
    "internetMessageId",
];
const DRAFT_ONLY: usize = 9;

/// `PATCH /me/messages/{id}`: read state, flag, categories, Focused Inbox and importance on any
/// message; content on drafts, which only JSON can change.
pub async fn update(
    call: Call,
    Path(params): super::Params,
    headers: axum::http::HeaderMap,
    body: axum::body::Bytes,
) -> Result<Response> {
    let json_body = headers
        .get(header::CONTENT_TYPE)
        .and_then(|value| value.to_str().ok())
        .is_none_or(|value| value.starts_with("application/json"));
    if !json_body {
        return Err(GraphError::new(
            415,
            "ErrorInvalidRequest",
            "Updating a message's MIME content isn't supported. Send the changes as JSON.",
        ));
    }
    let patch: Map<String, Value> = body_json(&body, "message")?;
    let key = {
        let world = call.world();
        let origin = origin(&world);
        let view = call.view(&world, &origin);
        key_of(&view, &params["id"])?
    };
    apply_patch(&call, key, patch)?;
    let world = call.world();
    let origin = origin(&world);
    let view = call.view(&world, &origin);
    let json = view.message_json(key, None)?;
    Ok(reply_map(
        200,
        entity(view.context("messages/$entity"), json),
    ))
}

struct PreparedPatch {
    add: Vec<String>,
    remove: Vec<String>,
    flag: Option<Map<String, Value>>,
    categories: Option<Vec<String>>,
    stored: Vec<(String, Value)>,
    draft_changes: Map<String, Value>,
}

fn prepare_patch(
    world: &crate::world::World,
    labels: &std::collections::BTreeSet<String>,
    draft: bool,
    patch: &Map<String, Value>,
    now: jiff::Timestamp,
) -> Result<PreparedPatch> {
    if let Some(unknown) = patch
        .keys()
        .find(|name| !name.starts_with('@') && !UPDATABLE.contains(&name.as_str()))
    {
        return Err(GraphError::unknown_body_property(unknown, "message"));
    }
    let has_content = patch
        .keys()
        .any(|name| UPDATABLE[DRAFT_ONLY..].contains(&name.as_str()));
    if has_content && !draft {
        return Err(GraphError::new(
            400,
            "ErrorInvalidPropertyUpdateSentMessage",
            "Update operation is not allowed on a sent message.",
        ));
    }
    let (mut add, mut remove): (Vec<String>, Vec<String>) = (Vec::new(), Vec::new());
    if let Some(read) = patch.get("isRead") {
        let read = read
            .as_bool()
            .ok_or_else(|| GraphError::unreadable_body("isRead must be a boolean"))?;
        if read {
            remove.push("UNREAD".into());
        } else {
            add.push("UNREAD".into());
        }
    }
    let mut flag_stored = None;
    if let Some(flag) = patch.get("flag") {
        let flag = flag
            .as_object()
            .ok_or_else(|| GraphError::unreadable_body("flag must be an object"))?;
        let status = flag
            .get("flagStatus")
            .and_then(Value::as_str)
            .unwrap_or("notFlagged");
        match status {
            "flagged" => add.push("STARRED".into()),
            "complete" | "notFlagged" => remove.push("STARRED".into()),
            other => {
                return Err(GraphError::unreadable_body(format!(
                    "'{other}' isn't a followupFlagStatus"
                )));
            }
        }
        let mut stored = flag.clone();
        if status == "complete" && !stored.contains_key("completedDateTime") {
            stored.insert("completedDateTime".into(), odata::date_time_zone(now));
        }
        flag_stored = Some(stored);
    }
    if let Some(classification) = patch.get("inferenceClassification") {
        let focused = match classification.as_str() {
            Some("focused") => true,
            Some("other") => false,
            _ => {
                return Err(GraphError::unreadable_body(
                    "inferenceClassification must be focused or other",
                ));
            }
        };
        let (more, fewer) = model::classify(labels, focused);
        add.extend(more);
        remove.extend(fewer);
    }
    let categories = if let Some(categories) = patch.get("categories") {
        let names: Vec<String> = categories
            .as_array()
            .ok_or_else(|| GraphError::unreadable_body("categories must be an array"))?
            .iter()
            .filter_map(|name| name.as_str().map(str::to_owned))
            .collect();
        for name in &names {
            super::settings::validate_category_name(world, name)?;
        }
        Some(names)
    } else {
        None
    };
    let mut stored_props = Vec::new();
    for stored in [
        "importance",
        "isDeliveryReceiptRequested",
        "isReadReceiptRequested",
    ] {
        if let Some(value) = patch.get(stored).filter(|_| !draft) {
            let value = if stored == "importance" {
                value
                    .as_str()
                    .map(str::to_ascii_lowercase)
                    .map(Value::from)
                    .unwrap_or(Value::Null)
            } else {
                value.clone()
            };
            stored_props.push((stored.to_owned(), value));
        }
    }
    let draft_changes: Map<String, Value> = patch
        .iter()
        .filter(|(name, _)| {
            draft
                && (UPDATABLE[DRAFT_ONLY..].contains(&name.as_str())
                    || matches!(
                        name.as_str(),
                        "importance" | "isDeliveryReceiptRequested" | "isReadReceiptRequested"
                    ))
        })
        .map(|(name, value)| (name.clone(), value.clone()))
        .collect();
    Ok(PreparedPatch {
        add,
        remove,
        flag: flag_stored,
        categories,
        stored: stored_props,
        draft_changes,
    })
}

/// Validates a message patch without mutating `world`.
pub fn validate_patch(
    world: &crate::world::World,
    patch: &Map<String, Value>,
    draft: bool,
) -> Result<()> {
    let empty = std::collections::BTreeSet::new();
    prepare_patch(world, &empty, draft, patch, world.clock.now())?;
    Ok(())
}

/// Applies a message PATCH (also a new draft's Outlook-only properties) in a locked world,
/// validating every field before mutating mail or Outlook state.
pub fn apply_patch_locked(
    world: &mut crate::world::World,
    key: u64,
    patch: Map<String, Value>,
) -> Result<()> {
    let now = world.clock.now();
    let (gmail, labels) = {
        let item = world
            .graph
            .items
            .get(&key)
            .ok_or_else(GraphError::not_found)?;
        (item.gmail.clone(), item.labels.clone())
    };
    let draft = labels.contains("DRAFT");
    let mut prepared = prepare_patch(world, &labels, draft, &patch, now)?;
    let prepared_draft = if prepared.draft_changes.is_empty() {
        None
    } else {
        Some(compose::prepare_draft(world, key, &prepared.draft_changes)?)
    };

    let mut touched = false;
    if let Some(flag) = prepared.flag {
        world.graph.flags.insert(key, flag);
        touched = true;
    }
    if let Some(names) = prepared.categories {
        let (more, fewer) = category_labels(world, &labels, &names)?;
        prepared.add.extend(more);
        prepared.remove.extend(fewer);
    }
    for (name, value) in prepared.stored {
        world
            .graph
            .stored
            .entry(key)
            .or_default()
            .insert(name, value);
        touched = true;
    }
    world
        .mail
        .apply_labels(&gmail, &prepared.add, &prepared.remove);
    world.graph.catch_up(&mut world.mail, now);
    if let Some((draft_id, raw)) = prepared_draft {
        compose::commit_draft(world, key, &draft_id, raw)?;
    } else if touched {
        world.graph.touch(key, now);
    }
    Ok(())
}

/// Applies a message PATCH (also a new draft's Outlook-only properties).
pub fn apply_patch(call: &Call, key: u64, patch: Map<String, Value>) -> Result<()> {
    let mut world = call.world();
    apply_patch_locked(&mut world, key, patch)
}

/// The label changes that give a message exactly the categories `names`, creating labels for
/// names the mailbox doesn't have yet (Outlook lets a message carry any category).
fn category_labels(
    world: &mut crate::world::World,
    labels: &std::collections::BTreeSet<String>,
    names: &[String],
) -> Result<(Vec<String>, Vec<String>)> {
    let folders = world.graph.folder_labels();
    let mut wanted = Vec::new();
    for name in names {
        let id = super::settings::category_label(world, name, None)?
            .id
            .unwrap_or_default();
        wanted.push(id);
    }
    let current: Vec<String> = labels
        .iter()
        .filter(|id| id.starts_with("Label_") && !folders.contains(id))
        .cloned()
        .collect();
    let add = wanted
        .iter()
        .filter(|id| !current.contains(id))
        .cloned()
        .collect();
    let remove = current
        .into_iter()
        .filter(|id| !wanted.contains(id))
        .collect();
    Ok((add, remove))
}

#[derive(Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct MoveRequest {
    destination_id: Option<String>,
}

/// `POST /me/messages/{id}/move`: the message, in its new folder (with a new default ID).
pub async fn move_to(
    call: Call,
    Path(params): super::Params,
    body: axum::body::Bytes,
) -> Result<Response> {
    let request: MoveRequest = body_json(&body, "message")?;
    let destination = request
        .destination_id
        .ok_or_else(|| GraphError::invalid_request("The destinationId parameter is missing."))?;
    let mut world = call.world();
    let (key, target) = {
        let origin = origin(&world);
        let view = call.view(&world, &origin);
        (
            key_of(&view, &params["id"])?,
            resolve_target(&view, &destination)?,
        )
    };
    let item = world
        .graph
        .items
        .get(&key)
        .ok_or_else(GraphError::not_found)?
        .clone();
    let folders = world.graph.folder_labels();
    let refused = || {
        GraphError::new(
            400,
            "ErrorMoveCopyFailed",
            "The item can't be moved to that folder.",
        )
    };
    if item.folder != target {
        if item.labels.contains("DRAFT") && target != Folder::DeletedItems {
            return Err(refused());
        }
        let (add, remove) = model::move_into(&target, &folders).ok_or_else(refused)?;
        world.mail.apply_labels(&item.gmail, &add, &remove);
        let now = world.clock.now();
        let world_ref = &mut *world;
        world_ref.graph.catch_up(&mut world_ref.mail, now);
    }
    let origin = origin(&world);
    let view = call.view(&world, &origin);
    let json = view.message_json(key, None)?;
    Ok(reply_map(
        201,
        entity(view.context("messages/$entity"), json),
    ))
}

/// `POST /me/messages/{id}/copy`: a copy of the message in another folder, in the same
/// conversation, keeping its read state, flag and categories.
pub async fn copy_to(
    call: Call,
    Path(params): super::Params,
    body: axum::body::Bytes,
) -> Result<Response> {
    let request: MoveRequest = body_json(&body, "message")?;
    let destination = request
        .destination_id
        .ok_or_else(|| GraphError::invalid_request("The destinationId parameter is missing."))?;
    let mut world = call.world();
    let (key, target) = {
        let origin = origin(&world);
        let view = call.view(&world, &origin);
        (
            key_of(&view, &params["id"])?,
            resolve_target(&view, &destination)?,
        )
    };
    let item = world
        .graph
        .items
        .get(&key)
        .ok_or_else(GraphError::not_found)?
        .clone();
    let folders = world.graph.folder_labels();
    let refused = || {
        GraphError::new(
            400,
            "ErrorMoveCopyFailed",
            "The item can't be copied to that folder.",
        )
    };
    if item.labels.contains("DRAFT") {
        return Err(refused());
    }
    let (add, remove) = model::move_into(&target, &folders).ok_or_else(refused)?;
    let mut labels: Vec<String> = item
        .labels
        .iter()
        .filter(|label| !remove.contains(label))
        .cloned()
        .collect();
    labels.extend(add.into_iter().filter(|label| !item.labels.contains(label)));
    let (raw, thread, internal_date) = {
        let message = world.mail.message(&item.gmail)?;
        (
            message.raw.clone(),
            message.thread_id.clone(),
            message.internal_date,
        )
    };
    let copy = world.mail.insert(crate::mail::NewMessage {
        raw,
        labels,
        threading: crate::mail::Threading::Copy(thread),
        internal_date,
        apply_filters: false,
    })?;
    let now = world.clock.now();
    let world_ref = &mut *world;
    world_ref.graph.catch_up(&mut world_ref.mail, now);
    let copied = world
        .graph
        .key_of(&copy)
        .ok_or_else(GraphError::not_found)?;
    let graph = &mut world.graph;
    for stored in [&mut graph.flags, &mut graph.stored] {
        if let Some(value) = stored.get(&key).cloned() {
            stored.insert(copied, value);
        }
    }
    let origin = origin(&world);
    let view = call.view(&world, &origin);
    let json = view.message_json(copied, None)?;
    Ok(reply_map(
        201,
        entity(view.context("messages/$entity"), json),
    ))
}

/// `DELETE /me/messages/{id}`: the message leaves the mailbox's folders for the recoverable
/// items, which the mock doesn't keep.
pub async fn delete(call: Call, Path(params): super::Params) -> Result<Response> {
    remove(&call, &params["id"])
}

/// `POST /me/messages/{id}/permanentDelete`.
pub async fn permanent_delete(call: Call, Path(params): super::Params) -> Result<Response> {
    remove(&call, &params["id"])
}

fn remove(call: &Call, id: &str) -> Result<Response> {
    let mut world = call.world();
    let key = {
        let origin = origin(&world);
        let view = call.view(&world, &origin);
        key_of(&view, id)?
    };
    let gmail = world
        .graph
        .items
        .get(&key)
        .map(|item| item.gmail.clone())
        .ok_or_else(GraphError::not_found)?;
    world.mail.delete(&gmail)?;
    let now = world.clock.now();
    let world_ref = &mut *world;
    world_ref.graph.catch_up(&mut world_ref.mail, now);
    Ok(StatusCode::NO_CONTENT.into_response())
}

/// A message delta's state, carried in its links.
#[derive(Debug, Clone, Serialize, Deserialize)]
struct DeltaToken {
    /// The folder.
    f: String,
    /// Changes after this point; `None` for the initial round.
    s: Option<u64>,
    /// The round's snapshot: changes up to here.
    t: u64,
    /// The initial round's last message served (received time, key), or the next entry.
    c: Option<(i64, u64)>,
    i: usize,
    /// Messages the initial round may still serve, under `$top`.
    n: Option<usize>,
    /// The round's options: `$select`, `$expand`, `$filter`, `changeType`.
    q: String,
}

/// `GET /me/mailFolders/{id}/messages/delta`: the folder's messages, then what was added to,
/// changed in and removed from it since. A message that moves out shows up as `@removed` here
/// and as new in its new folder; with immutable IDs it keeps its ID across both.
pub async fn delta(call: Call, Path(params): super::Params) -> Result<Response> {
    let world = call.world();
    let origin = origin(&world);
    let view = call.view(&world, &origin);
    let outlook = &world.graph;
    let folder = resolve(&view, &params["folder"])?;
    let folder_id = ids::folder(&folder);
    let token = match call
        .options
        .skiptoken
        .as_deref()
        .or(call.options.deltatoken.as_deref())
    {
        Some(text) => {
            let mut token: DeltaToken =
                odata::decode_token(text).ok_or_else(GraphError::sync_state_not_found)?;
            if token.f != folder_id {
                return Err(GraphError::bad_request(
                    "The state token belongs to another folder.",
                ));
            }
            if token.s.is_some_and(|since| since < outlook.floor) {
                return Err(GraphError::sync_state_not_found());
            }
            if call.options.deltatoken.is_some() {
                // A new round: changes after the last one, up to now.
                token.t = outlook.seq;
                token.i = 0;
            }
            token
        }
        None => DeltaToken {
            f: folder_id.clone(),
            s: None,
            t: outlook.seq,
            c: None,
            i: 0,
            n: call.options.top,
            q: delta_options(&call)?,
        },
    };
    let options = odata::Options::parse(&token.q)?;
    let filter = delta_filter(options.filter.as_deref())?;
    let change_type = options.custom("changeType").map(str::to_ascii_lowercase);
    let expand = Expand::parse(options.expand.as_deref())?;
    let select = options.select.as_deref();
    let size = call.prefer.max_page.unwrap_or(10).min(MAX_PAGE);
    let render = |key: u64| -> Result<Value> {
        let mut json = view.message_json(key, select)?;
        expand.apply(&view, key, &mut json)?;
        let mut out = Map::new();
        out.insert("@odata.type".into(), "#microsoft.graph.message".into());
        out.extend(json);
        Ok(Value::Object(out))
    };
    let received = |key: u64| -> Option<i64> {
        view.message(key)
            .ok()
            .map(|(_, message)| message.internal_date)
    };

    let mut value = Vec::new();
    let next: Option<DeltaToken>;
    match token.s {
        None => {
            // The initial round: the folder's messages, newest first.
            let mut keys: Vec<(i64, u64)> = outlook
                .items
                .iter()
                .filter(|(_, item)| item.folder == folder)
                .filter_map(|(&key, _)| Some((received(key)?, key)))
                .filter(|(at, _)| {
                    filter.is_none_or(
                        |(after, inclusive)| if inclusive { *at >= after } else { *at > after },
                    )
                })
                .collect();
            keys.sort_by(|a, b| b.cmp(a));
            let rest: Vec<(i64, u64)> = keys
                .into_iter()
                .filter(|position| token.c.is_none_or(|cursor| *position < cursor))
                .collect();
            let room = token.n.map_or(size, |left| left.min(size));
            for &(_, key) in rest.iter().take(room) {
                value.push(render(key)?);
            }
            let served = value.len();
            let left = token.n.map(|n| n - served);
            next = (rest.len() > served && left != Some(0)).then(|| DeltaToken {
                c: rest.get(served.saturating_sub(1)).copied(),
                n: left,
                ..token.clone()
            });
        }
        Some(since) => {
            let mut entries: Vec<Value> = Vec::new();
            for (key, change) in outlook.changes(since, token.t) {
                let was = change
                    .before
                    .as_ref()
                    .filter(|place| place.folder == folder);
                let is = change.after.as_ref().filter(|place| place.folder == folder);
                let immutable = view.prefer.immutable_ids;
                let removed = |generation: u32| {
                    json!({
                        "@odata.type": "#microsoft.graph.message",
                        "id": ids::message(key, generation, immutable),
                        "@removed": { "reason": "deleted" },
                    })
                };
                match (was, is) {
                    (Some(was), None) => {
                        if change_type.as_deref().is_none_or(|t| t == "deleted") {
                            entries.push(removed(was.generation));
                        }
                    }
                    (was, Some(is)) => {
                        if !outlook.items.contains_key(&key) {
                            continue;
                        }
                        let renamed =
                            was.is_some_and(|was| was.generation != is.generation && !immutable);
                        if let Some(was) = was.filter(|_| renamed)
                            && change_type.as_deref().is_none_or(|t| t == "deleted")
                        {
                            entries.push(removed(was.generation));
                        }
                        let kind = if was.is_none() || renamed {
                            "created"
                        } else {
                            "updated"
                        };
                        let in_range = received(key).is_some_and(|at| {
                            filter.is_none_or(
                                |(after, inclusive)| {
                                    if inclusive { at >= after } else { at > after }
                                },
                            )
                        });
                        if in_range && change_type.as_deref().is_none_or(|t| t == kind) {
                            entries.push(render(key)?);
                        }
                    }
                    (None, None) => {}
                }
            }
            value = entries.iter().skip(token.i).take(size).cloned().collect();
            next = (token.i + size < entries.len()).then(|| DeltaToken {
                i: token.i + size,
                ..token.clone()
            });
        }
    }
    let path = format!("me/mailFolders/{folder_id}/messages/delta");
    let mut body = Map::new();
    body.insert(
        "@odata.context".into(),
        odata::context(&origin, "Collection(message)").into(),
    );
    match next {
        Some(next) => body.insert(
            "@odata.nextLink".into(),
            odata::link(
                &origin,
                &path,
                &[("$skiptoken".into(), odata::encode_token(&next))],
            )
            .into(),
        ),
        None => {
            let done = DeltaToken {
                s: Some(token.t),
                c: None,
                i: 0,
                n: None,
                ..token
            };
            body.insert(
                "@odata.deltaLink".into(),
                odata::link(
                    &origin,
                    &path,
                    &[("$deltatoken".into(), odata::encode_token(&done))],
                )
                .into(),
            )
        }
    };
    body.insert("value".into(), value.into());
    Ok(reply_map(200, body))
}

/// The initial delta request's options, which its links carry: what delta supports, checked.
fn delta_options(call: &Call) -> Result<String> {
    let options = &call.options;
    if options.search.is_some() {
        return Err(GraphError::bad_request(
            "$search isn't supported in a delta query.",
        ));
    }
    delta_filter(options.filter.as_deref())?;
    if let Some(order) = &options.orderby
        && !order.trim().eq_ignore_ascii_case("receivedDateTime desc")
    {
        return Err(GraphError::bad_request(
            "The only $orderby a delta query supports is 'receivedDateTime desc'.",
        ));
    }
    if let Some(change) = options.custom("changeType")
        && !matches!(
            change.to_ascii_lowercase().as_str(),
            "created" | "updated" | "deleted"
        )
    {
        return Err(GraphError::bad_request(
            "changeType must be created, updated or deleted.",
        ));
    }
    Expand::parse(options.expand.as_deref())?;
    if let Some(select) = &options.select {
        odata::check_properties(select, project::MESSAGE_PROPERTIES, "message")?;
    }
    let mut params: Vec<(String, String)> = Vec::new();
    if let Some(select) = &options.select {
        params.push(("$select".into(), select.join(",")));
    }
    for (name, value) in [("$expand", &options.expand), ("$filter", &options.filter)] {
        if let Some(value) = value {
            params.push((name.into(), value.clone()));
        }
    }
    if let Some(change) = options.custom("changeType") {
        params.push(("changeType".into(), change.to_owned()));
    }
    Ok(url::form_urlencoded::Serializer::new(String::new())
        .extend_pairs(params)
        .finish())
}

/// A delta's `$filter`, of which Outlook supports only `receivedDateTime ge|gt {value}`: the
/// instant (epoch ms) and whether it's inclusive.
fn delta_filter(text: Option<&str>) -> Result<Option<(i64, bool)>> {
    let Some(text) = text else {
        return Ok(None);
    };
    match Expr::parse(text)? {
        Expr::Compare(path, op, super::filter::Literal::Time(at))
            if path.0 == ["receivedDateTime"]
                && matches!(op, super::filter::Op::Ge | super::filter::Op::Gt) =>
        {
            Ok(Some((at.as_millisecond(), op == super::filter::Op::Ge)))
        }
        _ => Err(GraphError::invalid_filter()),
    }
}

/// The ID formats `translateExchangeIds` converts between.
const ID_TYPES: &[&str] = &[
    "entryId",
    "ewsId",
    "immutableEntryId",
    "restId",
    "restImmutableEntryId",
];

/// `POST /me/translateExchangeIds`: IDs from one of Exchange's formats to another. REST IDs are
/// the mock's own, EWS IDs are the same in standard base64, and the binary formats (`entryId`,
/// `immutableEntryId`) wrap them in a MAPI-style header, written as Graph documents: URL-safe
/// base64 without padding, then a digit for how much padding there was.
pub async fn translate_ids(call: Call, body: axum::body::Bytes) -> Result<Response> {
    let request: Map<String, Value> = body_json(&body, "translateExchangeIds")?;
    if let Some(unknown) = request
        .keys()
        .find(|k| !matches!(k.as_str(), "inputIds" | "sourceIdType" | "targetIdType"))
    {
        return Err(GraphError::unknown_body_property(
            unknown,
            "translateExchangeIds",
        ));
    }
    let kind = |name: &str| -> Result<&'static str> {
        let value = request
            .get(name)
            .and_then(Value::as_str)
            .unwrap_or_default();
        ID_TYPES
            .iter()
            .find(|known| known.eq_ignore_ascii_case(value))
            .copied()
            .ok_or_else(|| {
                GraphError::bad_request(format!(
                    "Cannot convert the literal '{value}' to the expected type 'microsoft.graph.exchangeIdFormat'."
                ))
            })
    };
    let (source, target) = (kind("sourceIdType")?, kind("targetIdType")?);
    let inputs: Vec<String> = request
        .get("inputIds")
        .and_then(Value::as_array)
        .map(|ids| {
            ids.iter()
                .filter_map(|id| id.as_str().map(str::to_owned))
                .collect()
        })
        .unwrap_or_default();
    if inputs.len() > 1000 {
        return Err(GraphError::bad_request("inputIds takes at most 1000 IDs."));
    }
    let world = call.world();
    let origin = origin(&world);
    let view = call.view(&world, &origin);
    let value: Vec<Value> = inputs
        .iter()
        .map(|input| match translate(&view, input, source, target) {
            Ok(id) => json!({ "targetId": id, "sourceId": input }),
            Err(error) => json!({
                "sourceId": input,
                "errorDetails": { "code": error.code, "message": error.message },
            }),
        })
        .collect();
    Ok(reply(
        200,
        json!({
            "@odata.context": odata::context(&origin, "Collection(microsoft.graph.convertIdResult)"),
            "value": value,
        }),
    ))
}

/// One ID from `source`'s format to `target`'s.
fn translate(view: &View<'_>, input: &str, source: &str, target: &str) -> Result<String> {
    use base64::engine::general_purpose::STANDARD;
    let rest = match source {
        "restId" | "restImmutableEntryId" => input.to_owned(),
        "ewsId" => input
            .replace('+', "-")
            .replace('/', "_")
            .trim_end_matches('=')
            .to_owned(),
        _ => {
            let (body, padding) = input.split_at(input.len().saturating_sub(1));
            if !matches!(padding, "0" | "1" | "2") {
                return Err(GraphError::malformed_id());
            }
            let bytes = URL_SAFE_NO_PAD
                .decode(body)
                .map_err(|_| GraphError::malformed_id())?;
            String::from_utf8(
                bytes
                    .get(4..)
                    .ok_or_else(GraphError::malformed_id)?
                    .to_vec(),
            )
            .map_err(|_| GraphError::malformed_id())?
        }
    };
    let decoded = ids::decode(&rest).ok_or_else(GraphError::malformed_id)?;
    let immutable = matches!(target, "restImmutableEntryId" | "immutableEntryId");
    let id = match decoded {
        Id::Message { key, generation } => {
            let item = view
                .outlook
                .items
                .get(&key)
                .ok_or_else(GraphError::not_found)?;
            if generation.is_some_and(|g| g != item.generation) {
                return Err(GraphError::not_found());
            }
            ids::message(key, item.generation, immutable)
        }
        Id::Attachment {
            key,
            generation,
            index,
        } => {
            let item = view
                .outlook
                .items
                .get(&key)
                .ok_or_else(GraphError::not_found)?;
            if generation.is_some_and(|g| g != item.generation) {
                return Err(GraphError::not_found());
            }
            ids::attachment(key, item.generation, index, immutable)
        }
        _ => rest,
    };
    Ok(match target {
        "restId" | "restImmutableEntryId" => id,
        "ewsId" => {
            let mut ews = id.replace('-', "+").replace('_', "/");
            while ews.len() % 4 != 0 {
                ews.push('=');
            }
            ews
        }
        _ => {
            let flag = if immutable { [0u8, 0, 0, 1] } else { [0u8; 4] };
            let padded = STANDARD.encode([&flag[..], id.as_bytes()].concat());
            let padding = padded.len() - padded.trim_end_matches('=').len();
            format!(
                "{}{padding}",
                padded
                    .trim_end_matches('=')
                    .replace('+', "-")
                    .replace('/', "_")
            )
        }
    })
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
        body: Option<Value>,
    ) -> (u16, Value) {
        let mut req = Request::builder().method(method).uri(uri).header(
            "Authorization",
            format!("Bearer {}", crate::DEV_ACCESS_TOKEN),
        );
        let payload = if let Some(json) = body {
            req = req.header("Content-Type", "application/json");
            Body::from(json.to_string())
        } else {
            Body::empty()
        };
        let response = router
            .clone()
            .oneshot(req.body(payload).unwrap())
            .await
            .unwrap();
        let status = response.status().as_u16();
        let bytes = to_bytes(response.into_body(), usize::MAX).await.unwrap();
        let json = serde_json::from_slice(&bytes).unwrap_or(Value::Null);
        (status, json)
    }

    #[tokio::test]
    async fn folder_scoped_get_enforces_message_folder() {
        let router = crate::router(&crate::Config::default());
        let (status, inbox) = call(
            &router,
            Method::GET,
            "/v1.0/me/mailFolders/inbox/messages?$top=1",
            None,
        )
        .await;
        assert_eq!(status, 200);
        let id = inbox["value"][0]["id"].as_str().unwrap();

        let (status, found) = call(
            &router,
            Method::GET,
            &format!("/v1.0/me/mailFolders/inbox/messages/{id}"),
            None,
        )
        .await;
        assert_eq!(status, 200, "{found}");

        let (status, wrong) = call(
            &router,
            Method::GET,
            &format!("/v1.0/me/mailFolders/sentitems/messages/{id}"),
            None,
        )
        .await;
        assert_eq!(status, 404, "{wrong}");
    }

    #[tokio::test]
    async fn rejected_patch_leaves_no_partial_mutations() {
        let router = crate::router(&crate::Config::default());
        let (status, draft) = call(
            &router,
            Method::POST,
            "/v1.0/me/messages",
            Some(json!({ "subject": "Atomic draft" })),
        )
        .await;
        assert_eq!(status, 201, "{draft}");
        let id = draft["id"].as_str().unwrap().to_owned();
        let original_read = draft["isRead"].clone();
        let original_flag = draft["flag"].clone();
        let original_cats = draft["categories"].clone();

        // Valid flag + new category + isRead paired with invalid recipient must fail atomically.
        let (status, err) = call(
            &router,
            Method::PATCH,
            &format!("/v1.0/me/messages/{id}"),
            Some(json!({
                "isRead": false,
                "flag": { "flagStatus": "flagged" },
                "categories": ["ShouldNotExist"],
                "toRecipients": [{ "emailAddress": { "address": "" } }]
            })),
        )
        .await;
        assert_eq!(status, 400, "{err}");

        let (status, after) = call(
            &router,
            Method::GET,
            &format!("/v1.0/me/messages/{id}"),
            None,
        )
        .await;
        assert_eq!(status, 200, "{after}");
        assert_eq!(after["isRead"], original_read);
        assert_eq!(after["flag"], original_flag);
        assert_eq!(after["categories"], original_cats);

        // No stray category label was created either.
        let (_, labels) = call(&router, Method::GET, "/gmail/v1/users/me/labels", None).await;
        assert!(
            labels["labels"]
                .as_array()
                .unwrap()
                .iter()
                .all(|l| l["name"] != "ShouldNotExist")
        );

        // On a non-draft message, valid flag paired with bad inferenceClassification leaves flag untouched.
        let (_, inbox) = call(
            &router,
            Method::GET,
            "/v1.0/me/mailFolders/inbox/messages?$top=1",
            None,
        )
        .await;
        let msg_id = inbox["value"][0]["id"].as_str().unwrap().to_owned();
        let msg_flag = inbox["value"][0]["flag"].clone();
        let (status, err) = call(
            &router,
            Method::PATCH,
            &format!("/v1.0/me/messages/{msg_id}"),
            Some(json!({
                "flag": { "flagStatus": "complete" },
                "inferenceClassification": "bogus"
            })),
        )
        .await;
        assert_eq!(status, 400, "{err}");
        let (_, after_msg) = call(
            &router,
            Method::GET,
            &format!("/v1.0/me/messages/{msg_id}"),
            None,
        )
        .await;
        assert_eq!(after_msg["flag"], msg_flag);
    }
}

//! Outlook's settings over Gmail's: master categories (user labels), mailbox settings with
//! automatic replies (Gmail's vacation responder), Inbox rules (Gmail's filters) and Focused
//! Inbox overrides.

use axum::extract::Path;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use gmail::{Filter, FilterAction, FilterCriteria, Label, LabelColor, LabelType, VacationSettings};
use jiff::Timestamp;
use serde_json::{Map, Value, json};

use super::error::{GraphError, Result};
use super::folders::{body_json, resolve, resolve_target};
use super::ids;
use super::messages::entity;
use super::model::Folder;
use super::project::{self, View};
use super::state::{Override, Rule};
use super::{Call, odata, origin, reply, reply_map};
use crate::world::World;

// Master categories

/// Outlook's 25 category colors, by approximate RGB, for matching Gmail's label colors.
const PRESETS: &[(u8, u8, u8)] = &[
    (0xe7, 0x48, 0x56), // preset0, red
    (0xff, 0x8c, 0x00), // orange
    (0xab, 0x7b, 0x51), // brown
    (0xff, 0xd1, 0x00), // yellow
    (0x4c, 0xb0, 0x50), // green
    (0x00, 0x96, 0x88), // teal
    (0x8b, 0x96, 0x2d), // olive
    (0x41, 0x7d, 0xe6), // blue
    (0x8e, 0x63, 0xce), // purple
    (0xd8, 0x3b, 0x7f), // cranberry
    (0x9d, 0xb2, 0xc4), // steel
    (0x4a, 0x5f, 0x75), // dark steel
    (0xa0, 0xa0, 0xa0), // gray
    (0x5c, 0x5c, 0x5c), // dark gray
    (0x00, 0x00, 0x00), // black
    (0xa4, 0x26, 0x2c), // dark red
    (0xc5, 0x5a, 0x11), // dark orange
    (0x7a, 0x4e, 0x2d), // dark brown
    (0xc2, 0x9b, 0x00), // dark yellow
    (0x0b, 0x6a, 0x0b), // dark green
    (0x03, 0x6c, 0x70), // dark teal
    (0x5c, 0x62, 0x1e), // dark olive
    (0x1f, 0x3a, 0x93), // dark blue
    (0x5c, 0x2e, 0x91), // dark purple
    (0x8b, 0x1f, 0x4e), // dark cranberry
];

fn rgb(hex: &str) -> Option<(u8, u8, u8)> {
    let hex = hex.strip_prefix('#').unwrap_or(hex);
    if hex.len() != 6 || !hex.bytes().all(|b| b.is_ascii_hexdigit()) {
        return None;
    }
    let channel = |at: usize| u8::from_str_radix(&hex[at..at + 2], 16).ok();
    Some((channel(0)?, channel(2)?, channel(4)?))
}

fn distance(a: (u8, u8, u8), b: (u8, u8, u8)) -> u32 {
    let d = |x: u8, y: u8| (i32::from(x) - i32::from(y)).unsigned_abs().pow(2);
    d(a.0, b.0) + d(a.1, b.1) + d(a.2, b.2)
}

/// The preset nearest a Gmail label's background, or `none` without one.
fn preset_of(label: &Label) -> String {
    let Some(background) = label
        .color
        .as_ref()
        .and_then(|c| c.background_color.as_deref())
        .and_then(rgb)
    else {
        return "none".to_owned();
    };
    let index = PRESETS
        .iter()
        .enumerate()
        .min_by_key(|(_, preset)| distance(**preset, background))
        .map_or(0, |(index, _)| index);
    format!("preset{index}")
}

/// The Gmail palette colors nearest a preset, for a label made from a category.
fn color_of(preset: &str) -> Option<LabelColor> {
    let index: usize = preset.strip_prefix("preset")?.parse().ok()?;
    let target = *PRESETS.get(index)?;
    let background = gmail::LABEL_PALETTE
        .iter()
        .min_by_key(|hex| rgb(hex).map_or(u32::MAX, |c| distance(c, target)))?;
    let (r, g, b) = rgb(background)?;
    let light = u32::from(r) * 299 + u32::from(g) * 587 + u32::from(b) * 114 > 128_000;
    Some(LabelColor {
        text_color: Some(if light { "#000000" } else { "#ffffff" }.to_owned()),
        background_color: Some((*background).to_owned()),
    })
}

/// The labels on the master category list: user labels that aren't folders or taken off it.
fn master(world: &World) -> Vec<Label> {
    let folders = world.graph.folder_labels();
    world
        .mail
        .labels()
        .iter()
        .filter(|label| label.kind == Some(LabelType::User))
        .filter(|label| {
            label
                .id
                .as_ref()
                .is_some_and(|id| !folders.contains(id) && !world.graph.off_master.contains(id))
        })
        .cloned()
        .collect()
}

/// Checks that `name` can be used as a category without mutating `world`.
pub fn validate_category_name(world: &World, name: &str) -> Result<()> {
    let existing = world.mail.labels().iter().find(|label| {
        label.kind == Some(LabelType::User)
            && label
                .name
                .as_deref()
                .is_some_and(|n| n.eq_ignore_ascii_case(name))
    });
    match existing {
        Some(label)
            if label
                .id
                .as_ref()
                .is_some_and(|id| world.graph.folder_labels().contains(id)) =>
        {
            Err(GraphError::invalid_request(format!(
                "'{name}' is a folder's name, which a category can't share here."
            )))
        }
        Some(_) => Ok(()),
        None => Ok(crate::mail::labels::validate_name(name)?),
    }
}

/// The label holding the category `name`, made (in `color`) if the mailbox has none. Folders
/// and categories share Gmail's label names, so a name a folder has is refused.
pub fn category_label(world: &mut World, name: &str, color: Option<LabelColor>) -> Result<Label> {
    let existing = world
        .mail
        .labels()
        .iter()
        .find(|label| {
            label.kind == Some(LabelType::User)
                && label
                    .name
                    .as_deref()
                    .is_some_and(|n| n.eq_ignore_ascii_case(name))
        })
        .cloned();
    match existing {
        Some(label)
            if label
                .id
                .as_ref()
                .is_some_and(|id| world.graph.folder_labels().contains(id)) =>
        {
            Err(GraphError::invalid_request(format!(
                "'{name}' is a folder's name, which a category can't share here."
            )))
        }
        Some(label) => Ok(label),
        None => Ok(world.mail.create_label(Label {
            name: Some(name.to_owned()),
            color,
            ..Label::default()
        })?),
    }
}

fn category_json(world: &World, label: &Label) -> Map<String, Value> {
    let id = label.id.clone().unwrap_or_default();
    let color = world
        .graph
        .colors
        .get(&id)
        .cloned()
        .unwrap_or_else(|| preset_of(label));
    let mut json = Map::new();
    json.insert("id".into(), ids::guid(&format!("category {id}")).into());
    json.insert(
        "displayName".into(),
        label.name.clone().unwrap_or_default().into(),
    );
    json.insert("color".into(), color.into());
    json
}

fn category_by_id(world: &World, id: &str) -> Result<Label> {
    master(world)
        .into_iter()
        .find(|label| {
            ids::guid(&format!(
                "category {}",
                label.id.as_deref().unwrap_or_default()
            )) == id
        })
        .ok_or_else(GraphError::not_found)
}

/// `GET /me/outlook/masterCategories`.
pub async fn list_categories(call: Call) -> Result<Response> {
    let world = call.world();
    let origin = origin(&world);
    let view = call.view(&world, &origin);
    let value = master(&world)
        .iter()
        .map(|label| Value::Object(category_json(&world, label)))
        .collect();
    Ok(reply(
        200,
        odata::collection(view.context("outlook/masterCategories"), value, None, None),
    ))
}

/// `GET /me/outlook/masterCategories/{id}`.
pub async fn get_category(call: Call, Path(params): super::Params) -> Result<Response> {
    let world = call.world();
    let origin = origin(&world);
    let view = call.view(&world, &origin);
    let label = category_by_id(&world, &params["id"])?;
    Ok(reply_map(
        200,
        entity(
            view.context("outlook/masterCategories/$entity"),
            category_json(&world, &label),
        ),
    ))
}

#[derive(Debug, Default, serde::Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct CategoryRequest {
    display_name: Option<String>,
    color: Option<String>,
}

fn check_color(color: Option<&str>) -> Result<()> {
    match color {
        Some(color) if color != "none" && color_of(color).is_none() => {
            Err(GraphError::bad_request(format!(
                "Cannot convert the literal '{color}' to the expected type 'microsoft.graph.categoryColor'."
            )))
        }
        _ => Ok(()),
    }
}

/// `POST /me/outlook/masterCategories`: a category, as a Gmail label in the nearest color.
pub async fn create_category(call: Call, body: axum::body::Bytes) -> Result<Response> {
    let request: CategoryRequest = body_json(&body, "outlookCategory")?;
    check_color(request.color.as_deref())?;
    let name = request
        .display_name
        .filter(|name| !name.trim().is_empty())
        .ok_or_else(|| GraphError::invalid_request("The category's displayName is missing."))?;
    let mut world = call.world();
    if master(&world).iter().any(|label| {
        label
            .name
            .as_deref()
            .is_some_and(|n| n.eq_ignore_ascii_case(&name))
    }) {
        return Err(GraphError::new(
            409,
            "Conflict",
            "A category with this name already exists.",
        ));
    }
    let label = category_label(
        &mut world,
        &name,
        request.color.as_deref().and_then(color_of),
    )?;
    world
        .graph
        .off_master
        .remove(label.id.as_deref().unwrap_or_default());
    if let (Some(id), Some(color)) = (&label.id, request.color) {
        world.graph.colors.insert(id.clone(), color);
    }
    let origin = origin(&world);
    let view = call.view(&world, &origin);
    Ok(reply_map(
        201,
        entity(
            view.context("outlook/masterCategories/$entity"),
            category_json(&world, &label),
        ),
    ))
}

/// `PATCH /me/outlook/masterCategories/{id}`: a category's color; its name is fixed.
pub async fn update_category(
    call: Call,
    Path(params): super::Params,
    body: axum::body::Bytes,
) -> Result<Response> {
    let request: CategoryRequest = body_json(&body, "outlookCategory")?;
    check_color(request.color.as_deref())?;
    let mut world = call.world();
    let label = category_by_id(&world, &params["id"])?;
    if request
        .display_name
        .is_some_and(|name| Some(name.as_str()) != label.name.as_deref())
    {
        return Err(GraphError::invalid_request(
            "A category's displayName can't be changed after it's created.",
        ));
    }
    let id = label.id.clone().unwrap_or_default();
    if let Some(color) = request.color {
        let label_color = color_of(&color);
        world.mail.update_label(
            &id,
            Label {
                color: label_color,
                ..Label::default()
            },
            true,
        )?;
        world.graph.colors.insert(id.clone(), color);
    }
    let label = category_by_id(&world, &params["id"])?;
    let origin = origin(&world);
    let view = call.view(&world, &origin);
    Ok(reply_map(
        200,
        entity(
            view.context("outlook/masterCategories/$entity"),
            category_json(&world, &label),
        ),
    ))
}

/// `DELETE /me/outlook/masterCategories/{id}`: off the list. Messages keep the category, as in
/// Outlook.
pub async fn delete_category(call: Call, Path(params): super::Params) -> Result<Response> {
    let mut world = call.world();
    let label = category_by_id(&world, &params["id"])?;
    world.graph.off_master.insert(label.id.unwrap_or_default());
    Ok(StatusCode::NO_CONTENT.into_response())
}

// Mailbox settings

fn auto_replies(world: &World) -> Value {
    let vacation = &world.mail.settings.vacation;
    let enabled = vacation.enable_auto_reply == Some(true);
    let status = if !enabled {
        "disabled"
    } else if vacation.start_time.is_some() || vacation.end_time.is_some() {
        "scheduled"
    } else {
        "alwaysEnabled"
    };
    let audience = if vacation.restrict_to_domain == Some(true) {
        "none"
    } else if vacation.restrict_to_contacts == Some(true) {
        "contactsOnly"
    } else {
        "all"
    };
    let internal = vacation
        .response_body_html
        .clone()
        .or_else(|| {
            vacation
                .response_body_plain_text
                .as_deref()
                .map(project::text_to_html)
        })
        .unwrap_or_default();
    let now = world.clock.now();
    let at = |ms: Option<i64>, fallback: Timestamp| {
        ms.and_then(|ms| Timestamp::from_millisecond(ms).ok())
            .unwrap_or(fallback)
    };
    let start = at(vacation.start_time, now);
    let end = at(
        vacation.end_time,
        start
            .checked_add(jiff::SignedDuration::from_hours(24))
            .unwrap_or(start),
    );
    json!({
        "status": status,
        "externalAudience": audience,
        "internalReplyMessage": internal,
        "externalReplyMessage": world.graph.external_reply.clone().unwrap_or_else(|| internal.clone()),
        "scheduledStartDateTime": odata::date_time_zone(start),
        "scheduledEndDateTime": odata::date_time_zone(end),
    })
}

fn settings_json(world: &World) -> Map<String, Value> {
    let zone = project::windows_zone(world.account.tz.iana_name().unwrap_or("UTC"));
    let stored = &world.graph.settings;
    let get = |name: &str, default: Value| stored.get(name).cloned().unwrap_or(default);
    let mut json = Map::new();
    json.insert("archiveFolder".into(), ids::folder(&Folder::Archive).into());
    json.insert("timeZone".into(), get("timeZone", zone.clone().into()));
    json.insert("automaticRepliesSetting".into(), auto_replies(world));
    json.insert(
        "language".into(),
        get(
            "language",
            json!({ "locale": "en-US", "displayName": "English (United States)" }),
        ),
    );
    json.insert(
        "workingHours".into(),
        get(
            "workingHours",
            json!({
                "daysOfWeek": ["monday", "tuesday", "wednesday", "thursday", "friday"],
                "startTime": "08:00:00.0000000",
                "endTime": "17:00:00.0000000",
                "timeZone": { "name": zone },
            }),
        ),
    );
    json.insert("dateFormat".into(), get("dateFormat", "M/d/yyyy".into()));
    json.insert("timeFormat".into(), get("timeFormat", "h:mm tt".into()));
    json.insert(
        "delegateMeetingMessageDeliveryOptions".into(),
        get(
            "delegateMeetingMessageDeliveryOptions",
            "sendToDelegateOnly".into(),
        ),
    );
    json.insert("userPurpose".into(), "user".into());
    json
}

/// `GET /me/mailboxSettings`.
pub async fn mailbox_settings(call: Call) -> Result<Response> {
    let world = call.world();
    let origin = origin(&world);
    let view = call.view(&world, &origin);
    let json = odata::select(
        settings_json(&world),
        call.options.select.as_deref(),
        SETTINGS,
        "mailboxSettings",
    )?;
    Ok(reply_map(
        200,
        entity(view.context("mailboxSettings"), json),
    ))
}

const SETTINGS: &[&str] = &[
    "archiveFolder",
    "timeZone",
    "automaticRepliesSetting",
    "language",
    "workingHours",
    "dateFormat",
    "timeFormat",
    "delegateMeetingMessageDeliveryOptions",
    "userPurpose",
];

/// `GET /me/mailboxSettings/{property}`, e.g. `automaticRepliesSetting` or `timeZone`.
pub async fn mailbox_setting(call: Call, Path(params): super::Params) -> Result<Response> {
    let property = SETTINGS
        .iter()
        .find(|name| name.eq_ignore_ascii_case(&params["property"]))
        .ok_or_else(|| {
            GraphError::bad_request(format!(
                "Resource not found for the segment '{}'.",
                params["property"]
            ))
        })?;
    let world = call.world();
    let origin = origin(&world);
    let view = call.view(&world, &origin);
    let value = settings_json(&world)
        .remove(*property)
        .unwrap_or(Value::Null);
    let context = view.context(&format!("mailboxSettings/{property}"));
    let body = match value {
        Value::Object(object) => entity(context, object),
        primitive => entity(context, Map::from_iter([("value".to_owned(), primitive)])),
    };
    Ok(reply_map(200, body))
}

/// `PATCH /me/mailboxSettings`: automatic replies write Gmail's vacation responder; the rest is
/// Outlook's own. Answers with the properties it changed.
pub async fn update_mailbox_settings(call: Call, body: axum::body::Bytes) -> Result<Response> {
    let patch: Map<String, Value> = body_json(&body, "mailboxSettings")?;
    let mut world = call.world();
    for (name, value) in &patch {
        match name.as_str() {
            "automaticRepliesSetting" => set_auto_replies(&mut world, value)?,
            "timeZone"
            | "language"
            | "workingHours"
            | "dateFormat"
            | "timeFormat"
            | "delegateMeetingMessageDeliveryOptions" => {
                world.graph.settings.insert(name.clone(), value.clone());
            }
            "archiveFolder" | "userPurpose" => {
                return Err(GraphError::bad_request(format!(
                    "The property '{name}' is read-only."
                )));
            }
            other if other.starts_with('@') => {}
            other => return Err(GraphError::unknown_body_property(other, "mailboxSettings")),
        }
    }
    let current = settings_json(&world);
    let changed: Map<String, Value> = current
        .into_iter()
        .filter(|(name, _)| patch.contains_key(name))
        .collect();
    let origin = origin(&world);
    let view = call.view(&world, &origin);
    Ok(reply_map(
        200,
        entity(view.context("mailboxSettings"), changed),
    ))
}

fn set_auto_replies(world: &mut World, value: &Value) -> Result<()> {
    let object = value
        .as_object()
        .ok_or_else(|| GraphError::unreadable_body("automaticRepliesSetting must be an object"))?;
    let mut status: Option<&str> = None;
    let mut audience: Option<(bool, bool)> = None;
    let mut internal_reply: Option<Option<String>> = None;
    let mut start_time: Option<Option<i64>> = None;
    let mut end_time: Option<Option<i64>> = None;
    for (name, value) in object {
        match name.as_str() {
            "status" => match value.as_str() {
                Some(s @ ("disabled" | "alwaysEnabled" | "scheduled")) => status = Some(s),
                _ => {
                    return Err(GraphError::bad_request(
                        "status must be disabled, alwaysEnabled or scheduled.",
                    ));
                }
            },
            "externalAudience" => {
                audience = Some(match value.as_str() {
                    Some("none") => (true, false),
                    Some("contactsOnly") => (false, true),
                    Some("all") => (false, false),
                    _ => {
                        return Err(GraphError::bad_request(
                            "externalAudience must be none, contactsOnly or all.",
                        ));
                    }
                });
            }
            "internalReplyMessage" => {
                internal_reply = Some(value.as_str().map(str::to_owned));
            }
            "externalReplyMessage" => {}
            "scheduledStartDateTime" => {
                start_time = Some(parse_scheduled_time(value, "scheduledStartDateTime")?);
            }
            "scheduledEndDateTime" => {
                end_time = Some(parse_scheduled_time(value, "scheduledEndDateTime")?);
            }
            other => {
                return Err(GraphError::unknown_body_property(
                    other,
                    "automaticRepliesSetting",
                ));
            }
        }
    }
    let vacation: &mut VacationSettings = &mut world.mail.settings.vacation;
    if let Some((domain, contacts)) = audience {
        vacation.restrict_to_domain = Some(domain);
        vacation.restrict_to_contacts = Some(contacts);
    }
    if let Some(reply) = internal_reply {
        vacation.response_body_html = reply;
        vacation.response_body_plain_text = None;
    }
    if let Some(start) = start_time {
        vacation.start_time = start;
    }
    if let Some(end) = end_time {
        vacation.end_time = end;
    }
    if let Some(status) = status {
        match status {
            "disabled" => vacation.enable_auto_reply = Some(false),
            "alwaysEnabled" => {
                vacation.enable_auto_reply = Some(true);
                vacation.start_time = None;
                vacation.end_time = None;
            }
            "scheduled" => vacation.enable_auto_reply = Some(true),
            _ => unreachable!("validated above"),
        }
    }
    if let Some(external) = object.get("externalReplyMessage").and_then(Value::as_str) {
        let internal = world
            .mail
            .settings
            .vacation
            .response_body_html
            .clone()
            .unwrap_or_default();
        world.graph.external_reply = (external != internal).then(|| external.to_owned());
    }
    Ok(())
}

fn parse_scheduled_time(value: &Value, name: &str) -> Result<Option<i64>> {
    if value.is_null() {
        return Ok(None);
    }
    odata::parse_date_time_zone(value)
        .map(|at| Some(at.as_millisecond()))
        .ok_or_else(|| {
            GraphError::bad_request(format!("'{name}' is not a valid dateTimeTimeZone."))
        })
}

// Inbox rules

fn rule_id(filter: &str) -> String {
    format!("AQAAA{}", odata::encode_text_token(filter))
}

fn filter_of(id: &str) -> Option<String> {
    odata::decode_text_token(id.strip_prefix("AQAAA")?)
}

/// Every rule: Gmail's filters, and those disabled through Graph, by sequence.
fn rules(world: &World) -> Vec<(Filter, bool)> {
    let mut all: Vec<(Filter, bool)> = world
        .mail
        .settings
        .filters
        .iter()
        .map(|f| (f.clone(), true))
        .collect();
    all.extend(
        world
            .graph
            .rules
            .values()
            .filter_map(|rule| rule.disabled.clone().map(|f| (f, false))),
    );
    all.sort_by_key(|(filter, _)| sequence(world, filter));
    all
}

fn sequence(world: &World, filter: &Filter) -> i64 {
    let id = filter.id.as_deref().unwrap_or_default();
    world
        .graph
        .rules
        .get(id)
        .and_then(|rule| rule.sequence)
        .unwrap_or_else(|| {
            let index = world
                .mail
                .settings
                .filters
                .iter()
                .position(|f| f.id == filter.id)
                .unwrap_or_default();
            i64::try_from(index).unwrap_or_default() + 1
        })
}

fn rule_json(world: &World, view: &View<'_>, filter: &Filter, enabled: bool) -> Map<String, Value> {
    let id = filter.id.clone().unwrap_or_default();
    let meta = world.graph.rules.get(&id).cloned().unwrap_or_default();
    let criteria = filter.criteria.clone().unwrap_or_default();
    let action = filter.action.clone().unwrap_or_default();
    let mut json = Map::new();
    json.insert("id".into(), rule_id(&id).into());
    json.insert(
        "displayName".into(),
        meta.display_name
            .clone()
            .unwrap_or_else(|| match &criteria.from {
                Some(from) => format!("From {from}"),
                None => "Rule".to_owned(),
            })
            .into(),
    );
    json.insert("sequence".into(), sequence(world, filter).into());
    json.insert("isEnabled".into(), enabled.into());
    json.insert("hasError".into(), false.into());
    json.insert("isReadOnly".into(), false.into());
    json.insert(
        "conditions".into(),
        meta.conditions
            .clone()
            .unwrap_or_else(|| conditions_of(&criteria)),
    );
    json.insert(
        "actions".into(),
        meta.actions
            .clone()
            .unwrap_or_else(|| actions_of(view, &action)),
    );
    json
}

fn conditions_of(criteria: &FilterCriteria) -> Value {
    let mut conditions = Map::new();
    let address = |text: &str| json!({ "emailAddress": { "address": text } });
    if let Some(from) = &criteria.from {
        if from.contains('@') {
            conditions.insert("fromAddresses".into(), json!([address(from)]));
        } else {
            conditions.insert("senderContains".into(), json!([from]));
        }
    }
    if let Some(to) = &criteria.to {
        if to.contains('@') {
            conditions.insert("sentToAddresses".into(), json!([address(to)]));
        } else {
            conditions.insert("recipientContains".into(), json!([to]));
        }
    }
    if let Some(subject) = &criteria.subject {
        conditions.insert("subjectContains".into(), json!([subject]));
    }
    if let Some(query) = &criteria.query {
        conditions.insert("bodyOrSubjectContains".into(), json!([query]));
    }
    if criteria.has_attachment == Some(true) {
        conditions.insert("hasAttachments".into(), true.into());
    }
    Value::Object(conditions)
}

fn actions_of(view: &View<'_>, action: &FilterAction) -> Value {
    let folders = view.outlook.folder_labels();
    let mut actions = Map::new();
    let names = |ids: &[String]| -> Vec<String> {
        view.mail
            .labels()
            .iter()
            .filter(|label| label.id.as_ref().is_some_and(|id| ids.contains(id)))
            .filter_map(|label| label.name.clone())
            .collect()
    };
    let categories: Vec<String> = action
        .add_label_ids
        .iter()
        .filter(|id| id.starts_with("Label_") && !folders.contains(id))
        .cloned()
        .collect();
    if !categories.is_empty() {
        actions.insert("assignCategories".into(), names(&categories).into());
    }
    let folder = action
        .add_label_ids
        .iter()
        .find(|id| folders.contains(id))
        .map(|id| Folder::User(id.clone()));
    let removes_inbox = action.remove_label_ids.iter().any(|id| id == "INBOX");
    if action.add_label_ids.iter().any(|id| id == "TRASH") {
        actions.insert("delete".into(), true.into());
    } else if action.add_label_ids.iter().any(|id| id == "SPAM") {
        actions.insert(
            "moveToFolder".into(),
            ids::folder(&Folder::JunkEmail).into(),
        );
    } else if let Some(folder) = folder.filter(|_| removes_inbox) {
        // A filter that labels mail without taking it out of the inbox leaves it in the
        // Inbox, so in Outlook's terms it moves nothing.
        actions.insert("moveToFolder".into(), ids::folder(&folder).into());
    } else if removes_inbox {
        actions.insert("moveToFolder".into(), ids::folder(&Folder::Archive).into());
    }
    if action.remove_label_ids.iter().any(|id| id == "UNREAD") {
        actions.insert("markAsRead".into(), true.into());
    }
    if action.add_label_ids.iter().any(|id| id == "IMPORTANT") {
        actions.insert("markImportance".into(), "high".into());
    }
    if let Some(forward) = &action.forward {
        actions.insert(
            "forwardTo".into(),
            json!([{ "emailAddress": { "address": forward } }]),
        );
    }
    actions.insert("stopProcessingRules".into(), false.into());
    Value::Object(actions)
}

/// The Gmail filter a rule's conditions and actions come to (what Gmail can do of them).
fn filter_for(
    world: &mut World,
    view_folder: impl Fn(&str) -> Option<Folder>,
    conditions: &Value,
    actions: &Value,
) -> Result<(FilterCriteria, FilterAction)> {
    let first_address = |name: &str| -> Option<String> {
        conditions[name]
            .as_array()?
            .iter()
            .filter_map(|r| r["emailAddress"]["address"].as_str())
            .map(str::to_owned)
            .reduce(|a, b| format!("{a} OR {b}"))
    };
    let first_text = |name: &str| -> Option<String> {
        conditions[name]
            .as_array()?
            .iter()
            .filter_map(Value::as_str)
            .map(str::to_owned)
            .reduce(|a, b| format!("{a} OR {b}"))
    };
    let criteria = FilterCriteria {
        from: first_address("fromAddresses").or_else(|| first_text("senderContains")),
        to: first_address("sentToAddresses").or_else(|| first_text("recipientContains")),
        subject: first_text("subjectContains"),
        query: first_text("bodyOrSubjectContains").or_else(|| first_text("bodyContains")),
        has_attachment: conditions["hasAttachments"].as_bool(),
        ..FilterCriteria::default()
    };
    let mut action = FilterAction::default();
    for name in actions["assignCategories"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(Value::as_str)
    {
        let id = category_label(world, name, None)?.id.unwrap_or_default();
        action.add_label_ids.push(id);
    }
    let push_unique = |list: &mut Vec<String>, label: &str| {
        if !list.iter().any(|id| id == label) {
            list.push(label.to_owned());
        }
    };
    if let Some(target) = actions["moveToFolder"].as_str() {
        match view_folder(target) {
            Some(Folder::User(label)) => {
                push_unique(&mut action.add_label_ids, &label);
                push_unique(&mut action.remove_label_ids, "INBOX");
            }
            Some(Folder::DeletedItems) => {
                push_unique(&mut action.add_label_ids, "TRASH");
                push_unique(&mut action.remove_label_ids, "INBOX");
            }
            Some(Folder::JunkEmail) => {
                push_unique(&mut action.add_label_ids, "SPAM");
                push_unique(&mut action.remove_label_ids, "INBOX");
            }
            Some(Folder::Inbox) => {}
            Some(_) => push_unique(&mut action.remove_label_ids, "INBOX"),
            None => return Err(GraphError::folder_not_found()),
        }
    }
    if actions["delete"].as_bool() == Some(true)
        || actions["permanentDelete"].as_bool() == Some(true)
    {
        push_unique(&mut action.add_label_ids, "TRASH");
        push_unique(&mut action.remove_label_ids, "INBOX");
    }
    if actions["markAsRead"].as_bool() == Some(true) {
        action.remove_label_ids.push("UNREAD".into());
    }
    if actions["markImportance"].as_str() == Some("high") {
        action.add_label_ids.push("IMPORTANT".into());
    }
    action.forward = actions["forwardTo"]
        .as_array()
        .and_then(|to| to.first())
        .and_then(|r| r["emailAddress"]["address"].as_str())
        .map(str::to_owned);
    Ok((criteria, action))
}

fn check_inbox(call: &Call, world: &World, folder: &str) -> Result<()> {
    let origin = origin(world);
    let view = call.view(world, &origin);
    if resolve(&view, folder)? != Folder::Inbox {
        return Err(GraphError::invalid_request(
            "Rules apply to the Inbox only.",
        ));
    }
    Ok(())
}

/// `GET /me/mailFolders/inbox/messageRules`.
pub async fn list_rules(call: Call, Path(params): super::Params) -> Result<Response> {
    let world = call.world();
    check_inbox(&call, &world, &params["folder"])?;
    let origin = origin(&world);
    let view = call.view(&world, &origin);
    let value = rules(&world)
        .iter()
        .map(|(filter, enabled)| Value::Object(rule_json(&world, &view, filter, *enabled)))
        .collect();
    Ok(reply(
        200,
        odata::collection(
            view.context("mailFolders('inbox')/messageRules"),
            value,
            None,
            None,
        ),
    ))
}

fn find_rule(world: &World, id: &str) -> Result<(Filter, bool)> {
    let filter = filter_of(id).ok_or_else(GraphError::malformed_id)?;
    rules(world)
        .into_iter()
        .find(|(f, _)| f.id.as_deref() == Some(&filter))
        .ok_or_else(GraphError::not_found)
}

/// `GET /me/mailFolders/inbox/messageRules/{id}`.
pub async fn get_rule(call: Call, Path(params): super::Params) -> Result<Response> {
    let world = call.world();
    check_inbox(&call, &world, &params["folder"])?;
    let (filter, enabled) = find_rule(&world, &params["id"])?;
    let origin = origin(&world);
    let view = call.view(&world, &origin);
    Ok(reply_map(
        200,
        entity(
            view.context("mailFolders('inbox')/messageRules/$entity"),
            rule_json(&world, &view, &filter, enabled),
        ),
    ))
}

const RULE_PROPERTIES: &[&str] = &[
    "displayName",
    "sequence",
    "isEnabled",
    "conditions",
    "actions",
    "exceptions",
    "hasError",
    "isReadOnly",
];

/// `POST /me/mailFolders/inbox/messageRules`: a rule, as a Gmail filter doing what Gmail can of it.
pub async fn create_rule(
    call: Call,
    Path(params): super::Params,
    body: axum::body::Bytes,
) -> Result<Response> {
    let request: Map<String, Value> = body_json(&body, "messageRule")?;
    if let Some(unknown) = request
        .keys()
        .find(|k| !k.starts_with('@') && !RULE_PROPERTIES.contains(&k.as_str()))
    {
        return Err(GraphError::unknown_body_property(unknown, "messageRule"));
    }
    let mut world = call.world();
    check_inbox(&call, &world, &params["folder"])?;
    let id = world.mail.next_opaque_id("ANe1Bm");
    let filter = save_rule(&call, &mut world, &id, &request, None)?;
    let enabled = request
        .get("isEnabled")
        .and_then(Value::as_bool)
        .unwrap_or(true);
    let origin = origin(&world);
    let view = call.view(&world, &origin);
    Ok(reply_map(
        201,
        entity(
            view.context("mailFolders('inbox')/messageRules/$entity"),
            rule_json(&world, &view, &filter, enabled),
        ),
    ))
}

/// Stores a rule's Gmail filter and Outlook-only parts, merging `request` over `current`.
fn save_rule(
    call: &Call,
    world: &mut World,
    id: &str,
    request: &Map<String, Value>,
    current: Option<Map<String, Value>>,
) -> Result<Filter> {
    let current = current.unwrap_or_default();
    let get = |name: &str| request.get(name).or_else(|| current.get(name)).cloned();
    let conditions = get("conditions").unwrap_or_else(|| json!({}));
    let actions = get("actions").unwrap_or_else(|| json!({}));
    let enabled = get("isEnabled").and_then(|v| v.as_bool()).unwrap_or(true);
    let folders: Vec<(String, Folder)> = {
        let origin = origin(world);
        let view = call.view(world, &origin);
        actions["moveToFolder"]
            .as_str()
            .map(|target| resolve_target(&view, target).map(|f| (target.to_owned(), f)))
            .transpose()?
            .into_iter()
            .collect()
    };
    let (criteria, action) = filter_for(
        world,
        |target| {
            folders
                .iter()
                .find(|(t, _)| t == target)
                .map(|(_, f)| f.clone())
        },
        &conditions,
        &actions,
    )?;
    let filter = Filter {
        id: Some(id.to_owned()),
        criteria: Some(criteria),
        action: Some(action),
    };
    world
        .mail
        .settings
        .filters
        .retain(|f| f.id.as_deref() != Some(id));
    let rule = Rule {
        display_name: get("displayName").and_then(|v| v.as_str().map(str::to_owned)),
        sequence: get("sequence").and_then(|v| v.as_i64()),
        conditions: Some(conditions),
        actions: Some(actions),
        disabled: (!enabled).then(|| filter.clone()),
    };
    if enabled {
        world.mail.settings.filters.push(filter.clone());
    }
    world.graph.rules.insert(id.to_owned(), rule);
    Ok(filter)
}

/// `PATCH /me/mailFolders/inbox/messageRules/{id}`.
pub async fn update_rule(
    call: Call,
    Path(params): super::Params,
    body: axum::body::Bytes,
) -> Result<Response> {
    let request: Map<String, Value> = body_json(&body, "messageRule")?;
    if let Some(unknown) = request
        .keys()
        .find(|k| !k.starts_with('@') && !RULE_PROPERTIES.contains(&k.as_str()))
    {
        return Err(GraphError::unknown_body_property(unknown, "messageRule"));
    }
    let mut world = call.world();
    check_inbox(&call, &world, &params["folder"])?;
    let (filter, _) = find_rule(&world, &params["id"])?;
    let id = filter.id.clone().unwrap_or_default();
    let current = {
        let origin = origin(&world);
        let view = call.view(&world, &origin);
        let enabled = !world
            .graph
            .rules
            .get(&id)
            .is_some_and(|r| r.disabled.is_some());
        rule_json(&world, &view, &filter, enabled)
    };
    let saved = save_rule(&call, &mut world, &id, &request, Some(current))?;
    let enabled = world
        .graph
        .rules
        .get(&id)
        .is_none_or(|r| r.disabled.is_none());
    let origin = origin(&world);
    let view = call.view(&world, &origin);
    Ok(reply_map(
        200,
        entity(
            view.context("mailFolders('inbox')/messageRules/$entity"),
            rule_json(&world, &view, &saved, enabled),
        ),
    ))
}

/// `DELETE /me/mailFolders/inbox/messageRules/{id}`.
pub async fn delete_rule(call: Call, Path(params): super::Params) -> Result<Response> {
    let mut world = call.world();
    check_inbox(&call, &world, &params["folder"])?;
    let (filter, _) = find_rule(&world, &params["id"])?;
    let id = filter.id.unwrap_or_default();
    world
        .mail
        .settings
        .filters
        .retain(|f| f.id.as_deref() != Some(&id));
    world.graph.rules.remove(&id);
    Ok(StatusCode::NO_CONTENT.into_response())
}

// Focused Inbox overrides

fn override_json(rule: &Override) -> Map<String, Value> {
    let mut json = Map::new();
    json.insert(
        "classifyAs".into(),
        if rule.focused { "focused" } else { "other" }.into(),
    );
    json.insert(
        "senderEmailAddress".into(),
        json!({ "name": rule.name.clone().unwrap_or_else(|| rule.address.clone()), "address": rule.address }),
    );
    json.insert("id".into(), rule.id.clone().into());
    json
}

fn classify_as(value: Option<&Value>) -> Result<Option<bool>> {
    match value.map(|v| v.as_str()) {
        None => Ok(None),
        Some(Some("focused")) => Ok(Some(true)),
        Some(Some("other")) => Ok(Some(false)),
        Some(_) => Err(GraphError::bad_request(
            "classifyAs must be focused or other.",
        )),
    }
}

/// `GET /me/inferenceClassification/overrides`.
pub async fn list_overrides(call: Call) -> Result<Response> {
    let world = call.world();
    let origin = origin(&world);
    let view = call.view(&world, &origin);
    let value = world
        .graph
        .overrides
        .iter()
        .map(|rule| Value::Object(override_json(rule)))
        .collect();
    Ok(reply(
        200,
        odata::collection(
            view.context("inferenceClassification/overrides"),
            value,
            None,
            None,
        ),
    ))
}

/// `POST /me/inferenceClassification/overrides`: classifies the sender's future messages. An
/// override for the same address is updated instead.
pub async fn create_override(call: Call, body: axum::body::Bytes) -> Result<Response> {
    let request: Map<String, Value> = body_json(&body, "inferenceClassificationOverride")?;
    if let Some(unknown) = request
        .keys()
        .find(|k| !k.starts_with('@') && !matches!(k.as_str(), "classifyAs" | "senderEmailAddress"))
    {
        return Err(GraphError::unknown_body_property(
            unknown,
            "inferenceClassificationOverride",
        ));
    }
    let focused = classify_as(request.get("classifyAs"))?.unwrap_or(true);
    let sender = request
        .get("senderEmailAddress")
        .cloned()
        .unwrap_or(Value::Null);
    let address = sender["address"]
        .as_str()
        .filter(|a| a.contains('@'))
        .ok_or_else(|| GraphError::invalid_request("senderEmailAddress needs an address."))?
        .to_owned();
    let name = sender["name"].as_str().map(str::to_owned);
    let mut world = call.world();
    let overrides = &mut world.graph.overrides;
    let rule = match overrides
        .iter_mut()
        .find(|rule| rule.address.eq_ignore_ascii_case(&address))
    {
        Some(rule) => {
            rule.focused = focused;
            if name.is_some() {
                rule.name = name;
            }
            rule.clone()
        }
        None => {
            let rule = Override {
                id: ids::guid(&format!("override {}", address.to_lowercase())),
                focused,
                name,
                address,
            };
            overrides.push(rule.clone());
            rule
        }
    };
    let origin = origin(&world);
    let view = call.view(&world, &origin);
    Ok(reply_map(
        201,
        entity(
            view.context("inferenceClassification/overrides/$entity"),
            override_json(&rule),
        ),
    ))
}

/// `PATCH /me/inferenceClassification/overrides/{id}`.
pub async fn update_override(
    call: Call,
    Path(params): super::Params,
    body: axum::body::Bytes,
) -> Result<Response> {
    let request: Map<String, Value> = body_json(&body, "inferenceClassificationOverride")?;
    let focused = classify_as(request.get("classifyAs"))?;
    let mut world = call.world();
    let rule = world
        .graph
        .overrides
        .iter_mut()
        .find(|rule| rule.id == params["id"])
        .ok_or_else(GraphError::not_found)?;
    if let Some(focused) = focused {
        rule.focused = focused;
    }
    let rule = rule.clone();
    let origin = origin(&world);
    let view = call.view(&world, &origin);
    Ok(reply_map(
        200,
        entity(
            view.context("inferenceClassification/overrides/$entity"),
            override_json(&rule),
        ),
    ))
}

/// `DELETE /me/inferenceClassification/overrides/{id}`.
pub async fn delete_override(call: Call, Path(params): super::Params) -> Result<Response> {
    let mut world = call.world();
    let before = world.graph.overrides.len();
    world.graph.overrides.retain(|rule| rule.id != params["id"]);
    if world.graph.overrides.len() == before {
        return Err(GraphError::not_found());
    }
    Ok(StatusCode::NO_CONTENT.into_response())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn colors_meet_halfway() {
        let label = |hex: &str| Label {
            color: Some(LabelColor {
                text_color: Some("#ffffff".into()),
                background_color: Some(hex.into()),
            }),
            ..Label::default()
        };
        assert_eq!(preset_of(&label("#4a86e8")), "preset7");
        assert_eq!(
            preset_of(&label("#16a766")),
            "preset5",
            "Gmail's green is teal-ish"
        );
        assert_eq!(preset_of(&label("#fad165")), "preset3");
        assert_eq!(preset_of(&Label::default()), "none");
        let color = color_of("preset0").unwrap();
        assert!(gmail::LABEL_PALETTE.contains(&color.background_color.as_deref().unwrap()));
    }

    #[test]
    fn rule_ids_round_trip() {
        assert_eq!(
            filter_of(&rule_id("ANe1Bm0001")).as_deref(),
            Some("ANe1Bm0001")
        );
    }

    #[test]
    fn rgb_requires_exact_six_ascii_hex_digits() {
        assert_eq!(rgb("#4a86e8"), Some((0x4a, 0x86, 0xe8)));
        assert_eq!(rgb("4A86E8"), Some((0x4a, 0x86, 0xe8)));
        for bad in [
            "#+a+b+c",
            "#ffffff00",
            "#123456zz",
            "#fff",
            "##ffffff",
            "#12345é",
            "",
        ] {
            assert_eq!(rgb(bad), None, "rgb should reject {bad:?}");
        }
    }

    #[test]
    fn auto_replies_handles_key_order_and_rejects_bad_timestamps() {
        let mut world = crate::fixtures::world(&crate::Config::default());

        // `status: alwaysEnabled` BEFORE scheduled timestamps must still clear both timestamps.
        let status_first = json!({
            "status": "alwaysEnabled",
            "scheduledStartDateTime": { "dateTime": "2026-11-26T00:00:00", "timeZone": "UTC" },
            "scheduledEndDateTime": { "dateTime": "2026-11-30T00:00:00", "timeZone": "UTC" },
        });
        set_auto_replies(&mut world, &status_first).unwrap();
        assert_eq!(world.mail.settings.vacation.enable_auto_reply, Some(true));
        assert_eq!(world.mail.settings.vacation.start_time, None);
        assert_eq!(world.mail.settings.vacation.end_time, None);
        assert_eq!(auto_replies(&world)["status"], "alwaysEnabled");

        // `status: alwaysEnabled` AFTER scheduled timestamps clears both too.
        let status_last = json!({
            "scheduledStartDateTime": { "dateTime": "2026-11-26T00:00:00", "timeZone": "UTC" },
            "scheduledEndDateTime": { "dateTime": "2026-11-30T00:00:00", "timeZone": "UTC" },
            "status": "alwaysEnabled",
        });
        set_auto_replies(&mut world, &status_last).unwrap();
        assert_eq!(world.mail.settings.vacation.start_time, None);
        assert_eq!(world.mail.settings.vacation.end_time, None);
        assert_eq!(auto_replies(&world)["status"], "alwaysEnabled");

        // Unparseable scheduledStartDateTime or scheduledEndDateTime returns 400.
        let bad_start = json!({
            "status": "scheduled",
            "scheduledStartDateTime": { "dateTime": "not-a-timestamp", "timeZone": "UTC" },
        });
        let err = set_auto_replies(&mut world, &bad_start).unwrap_err();
        assert_eq!(err.status, 400);

        let bad_end = json!({
            "status": "alwaysEnabled",
            "scheduledEndDateTime": { "dateTime": "2026-11-30T00:00:00", "timeZone": "Atlantis" },
        });
        let err = set_auto_replies(&mut world, &bad_end).unwrap_err();
        assert_eq!(err.status, 400);
    }

    #[test]
    fn inbox_rule_trash_and_spam_remove_inbox_and_round_trip_junk() {
        let mut world = crate::fixtures::world(&crate::Config::default());
        let lookup = |name: &str| Folder::well_known(name);

        let (_, spam_action) = filter_for(
            &mut world,
            lookup,
            &json!({ "senderContains": ["promo.example"] }),
            &json!({ "moveToFolder": "junkemail" }),
        )
        .unwrap();
        assert!(spam_action.add_label_ids.contains(&"SPAM".to_owned()));
        assert!(spam_action.remove_label_ids.contains(&"INBOX".to_owned()));

        let (_, trash_action) = filter_for(
            &mut world,
            lookup,
            &json!({ "senderContains": ["noise.example"] }),
            &json!({ "moveToFolder": "deleteditems" }),
        )
        .unwrap();
        assert!(trash_action.add_label_ids.contains(&"TRASH".to_owned()));
        assert!(trash_action.remove_label_ids.contains(&"INBOX".to_owned()));

        let (_, delete_action) = filter_for(
            &mut world,
            lookup,
            &json!({ "senderContains": ["drop.example"] }),
            &json!({ "delete": true }),
        )
        .unwrap();
        assert!(delete_action.add_label_ids.contains(&"TRASH".to_owned()));
        assert!(delete_action.remove_label_ids.contains(&"INBOX".to_owned()));

        // Map Gmail SPAM filter back to Graph JunkEmail (not Archive).
        let origin = url::Url::parse("http://127.0.0.1:1/").unwrap();
        let who = crate::graph::Account::of(&world, &world.account.email, false);
        let view = View {
            mail: &world.mail,
            outlook: &world.graph,
            calendars: &world.calendar,
            who: &who,
            origin: &origin,
            prefer: odata::Prefer::default(),
            tz: &world.account.tz,
        };
        let mapped = actions_of(&view, &spam_action);
        assert_eq!(
            mapped["moveToFolder"],
            ids::folder(&Folder::JunkEmail),
            "SPAM filter should map to JunkEmail, not Archive"
        );
    }
}

//! The calendar as Outlook serves it: `/me/calendars`, events (listing, reading, creating,
//! updating, deleting), calendar views and their delta, and a series' instances.
//!
//! Everything is the world's calendar, the one Google Calendar's API serves, read through the
//! signed-in address's own calendars ([`crate::world::CalendarWorld`]): an event made here is on
//! Google's calendar too, and the other way round. Cancelled events are gone, as Outlook deletes
//! them.

use std::collections::{BTreeMap, HashMap};

use axum::extract::Path;
use axum::http::{StatusCode, Uri};
use axum::response::{IntoResponse, Response};
use gmail::calendar::{CalendarListEntry, Event};
use jiff::Timestamp;
use serde_json::{Map, Value, json};

use super::error::{GraphError, Result};
use super::event_input::{self, Target};
use super::events::{self, EVENT_PROPERTIES};
use super::filter::Expr;
use super::ids::{self, Id};
use super::messages::{compare_values, entity, lookup, parse_orderby};
use super::odata;
use super::project::View;
use super::state::Round;
use super::{Call, origin, reply, reply_map};

/// Every property of `calendar`.
pub const CALENDAR_PROPERTIES: &[&str] = &[
    "id",
    "name",
    "color",
    "hexColor",
    "isDefaultCalendar",
    "changeKey",
    "canShare",
    "canViewPrivateItems",
    "isSharedWithMe",
    "canEdit",
    "allowedOnlineMeetingProviders",
    "defaultOnlineMeetingProvider",
    "isTallyingResponses",
    "isRemovable",
    "owner",
];

const MAX_PAGE: usize = 1000;

/// A delta's page without `Prefer: odata.maxpagesize`.
const DELTA_PAGE: usize = 10;

impl View<'_> {
    /// The calendars Outlook lists: the calendar list's, the primary first, without those
    /// hidden from it.
    fn calendar_entries(&self) -> Vec<&CalendarListEntry> {
        let primary = &self.calendars.primary;
        let mut entries: Vec<&CalendarListEntry> = self
            .calendars
            .entries
            .iter()
            .filter(|entry| entry.hidden != Some(true))
            .collect();
        entries.sort_by_key(|entry| entry.id.as_ref() != Some(primary));
        entries
    }

    /// Whether the signed-in user can write to `calendar`.
    fn can_edit(&self, calendar: &str) -> bool {
        self.calendars
            .calendar(calendar)
            .ok()
            .and_then(|entry| entry.access_role.as_deref())
            .is_some_and(|role| matches!(role, "owner" | "writer"))
    }

    /// The online meeting providers events in `calendar` may use: Teams for a work account,
    /// Skype for a personal one, none on a calendar the user can't write to.
    pub fn providers(&self, calendar: &str) -> &'static [&'static str] {
        match (self.can_edit(calendar), self.who.personal()) {
            (false, _) => &[],
            (true, false) => &["teamsForBusiness"],
            (true, true) => &["skypeForConsumer"],
        }
    }

    fn calendar_json(&self, entry: &CalendarListEntry) -> Map<String, Value> {
        let id = entry.id.clone().unwrap_or_default();
        let primary = id == self.calendars.primary;
        let owner = entry.access_role.as_deref() == Some("owner");
        let providers = self.providers(&id);
        let (owner_name, owner_address) = self.calendar_owner(&id);
        let mut json = Map::new();
        let change_key = ids::change_key(
            crate::fixtures::fnv(&id),
            crate::fixtures::fnv(entry.etag.as_deref().unwrap_or_default()),
        );
        for name in CALENDAR_PROPERTIES {
            let value = match *name {
                "id" => ids::calendar(&id).into(),
                "name" => if primary {
                    "Calendar".to_owned()
                } else {
                    entry.summary.clone().unwrap_or_default()
                }
                .into(),
                "color" => "auto".into(),
                "hexColor" => entry.background_color.clone().unwrap_or_default().into(),
                "isDefaultCalendar" => primary.into(),
                "changeKey" => change_key.clone().into(),
                "canShare" | "canViewPrivateItems" => owner.into(),
                "isSharedWithMe" => (!owner).into(),
                "canEdit" => self.can_edit(&id).into(),
                "allowedOnlineMeetingProviders" => json!(providers),
                "defaultOnlineMeetingProvider" => {
                    providers.first().copied().unwrap_or("unknown").into()
                }
                "isTallyingResponses" => primary.into(),
                "isRemovable" => (!primary).into(),
                "owner" => json!({ "name": owner_name, "address": owner_address }),
                _ => Value::Null,
            };
            json.insert((*name).to_owned(), value);
        }
        json
    }

    /// The calendar a request names: `/me/calendars/{id}`, or the primary one.
    fn calendar_of(&self, params: &HashMap<String, String>) -> Result<String> {
        let Some(id) = params.get("calendar") else {
            return Ok(self.calendars.primary.clone());
        };
        let Some(Id::Calendar(calendar)) = ids::decode(id) else {
            return Err(GraphError::malformed_id());
        };
        self.calendar_entries()
            .iter()
            .any(|entry| entry.id.as_deref() == Some(&calendar))
            .then_some(calendar)
            .ok_or_else(GraphError::not_found)
    }

    /// The event a request names, in the calendar it names (when it names one), and that event's
    /// calendar.
    fn event_of(&self, params: &HashMap<String, String>) -> Result<(String, Event)> {
        let Some(Id::Event { calendar, event }) = ids::decode(&params["id"]) else {
            return Err(GraphError::malformed_id());
        };
        let scope = params
            .contains_key("calendar")
            .then(|| self.calendar_of(params))
            .transpose()?;
        if scope.is_some_and(|scope| scope != calendar)
            || !self
                .calendar_entries()
                .iter()
                .any(|entry| entry.id.as_deref() == Some(&calendar))
        {
            return Err(GraphError::not_found());
        }
        let found = self
            .calendars
            .event(&calendar, &event)
            .map_err(|_| GraphError::not_found())?;
        if found.status.as_deref() == Some("cancelled") {
            return Err(GraphError::not_found());
        }
        Ok((calendar, found))
    }

    /// `calendar`'s events in `[start, end)`: single events, occurrences and exceptions.
    fn occurrences(&self, calendar: &str, start: Timestamp, end: Timestamp) -> Vec<Event> {
        self.calendars
            .occurrences(calendar, start, end)
            .into_iter()
            .filter(|event| event.status.as_deref() != Some("cancelled"))
            .collect()
    }
}

/// The `@odata.context` path for a request's collection or entity: `calendars('…')/events` for
/// `/me/calendars/{id}/events`, and so on.
fn fragment(path: &str) -> String {
    let segments: Vec<&str> = path
        .trim_start_matches("/v1.0/")
        .trim_start_matches("me/")
        .split('/')
        .collect();
    let mut out: Vec<String> = Vec::new();
    let mut at = 0;
    while at < segments.len() {
        match segments.get(at + 1) {
            Some(key) if matches!(segments[at], "calendars" | "events") => {
                out.push(format!("{}('{key}')", segments[at]));
                at += 2;
            }
            _ => {
                out.push(segments[at].to_owned());
                at += 1;
            }
        }
    }
    out.join("/")
}

/// `startDateTime` and `endDateTime`, which a view requires: ISO 8601, UTC without an offset.
fn window(call: &Call) -> Result<(Timestamp, Timestamp)> {
    let parse = |name: &str| -> Result<Timestamp> {
        let text = call.options.custom(name).ok_or_else(|| {
            GraphError::new(
                400,
                "ErrorInvalidParameter",
                "This request requires a time window specified by the query string parameters StartDateTime and EndDateTime.",
            )
        })?;
        parse_instant(text).ok_or_else(|| {
            GraphError::new(
                400,
                "ErrorInvalidParameter",
                format!("The value '{text}' of parameter '{name}' is invalid."),
            )
        })
    };
    Ok((parse("startDateTime")?, parse("endDateTime")?))
}

pub(super) fn parse_instant(text: &str) -> Option<Timestamp> {
    if let Ok(at) = text.parse::<Timestamp>() {
        return Some(at);
    }
    let civil: jiff::civil::DateTime = text.parse().ok().or_else(|| {
        text.parse::<jiff::civil::Date>()
            .ok()
            .map(|d| d.at(0, 0, 0, 0))
    })?;
    civil
        .to_zoned(jiff::tz::TimeZone::UTC)
        .ok()
        .map(|z| z.timestamp())
}

/// A list of events with the request's `$filter`, `$orderby`, `$select`, `$top`, `$skip` and
/// `$count` applied, ordered by start unless asked otherwise.
fn list_events(
    call: &Call,
    view: &View<'_>,
    path: &str,
    calendar: &str,
    events: Vec<Event>,
) -> Result<Response> {
    let options = &call.options;
    let filter = options.filter.as_deref().map(Expr::parse).transpose()?;
    let mut order = parse_orderby(options.orderby.as_deref(), EVENT_PROPERTIES, "event")?;
    if order.is_empty() {
        order.push((vec!["start".into(), "dateTime".into()], false));
    }
    if let Some(filter) = &filter {
        odata::check_properties(&filter.properties(), EVENT_PROPERTIES, "event")?;
    }
    // Filters and orders read times in UTC, whatever zone the response is in.
    let utc = View {
        prefer: odata::Prefer {
            zone: None,
            ..view.prefer.clone()
        },
        ..*view
    };
    let mut rows: Vec<(Map<String, Value>, Event)> = Vec::new();
    for event in events {
        let json = utc.event_json(calendar, &event, None)?;
        if filter
            .as_ref()
            .is_none_or(|filter| filter.matches(&Value::Object(json.clone())))
        {
            rows.push((json, event));
        }
    }
    rows.sort_by(|(a, _), (b, _)| {
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
            .find(|ordering| ordering.is_ne())
            .unwrap_or(std::cmp::Ordering::Equal)
    });
    let total = rows.len();
    let top = options.top.unwrap_or(10).clamp(1, MAX_PAGE);
    let skip = options.skip;
    let expand = Expand::parse(options.expand.as_deref())?;
    let mut value = Vec::new();
    for (_, event) in rows.iter().skip(skip).take(top) {
        let mut json = view.event_json(calendar, event, options.select.as_deref())?;
        expand.apply(view, calendar, &mut json);
        value.push(Value::Object(json));
    }
    let next = (skip + top < total).then(|| {
        odata::link(
            view.origin,
            path.trim_start_matches("/v1.0/"),
            &odata::next_page(&call.query, skip + top),
        )
    });
    let mut context = fragment(path);
    if let Some(select) = &options.select {
        context = format!("{context}({})", select.join(","));
    }
    Ok(reply(
        200,
        odata::collection(
            view.context(&context),
            value,
            options.count.then_some(total),
            next,
        ),
    ))
}

/// An event's `$expand`: its `calendar`, and its `attachments` (events made here have none).
#[derive(Debug, Default)]
struct Expand {
    calendar: bool,
    attachments: bool,
}

impl Expand {
    fn parse(text: Option<&str>) -> Result<Self> {
        let mut expand = Self::default();
        for name in text.unwrap_or_default().split(',').map(str::trim) {
            match name {
                "" => {}
                "calendar" => expand.calendar = true,
                "attachments" => expand.attachments = true,
                other => return Err(GraphError::unknown_property(other, "event")),
            }
        }
        Ok(expand)
    }

    fn apply(&self, view: &View<'_>, calendar: &str, json: &mut Map<String, Value>) {
        if self.calendar
            && let Ok(entry) = view.calendars.calendar(calendar)
        {
            json.insert("calendar".into(), Value::Object(view.calendar_json(entry)));
        }
        if self.attachments {
            json.insert("attachments".into(), json!([]));
        }
    }
}

/// `GET /me/calendars`.
pub async fn list_calendars(call: Call) -> Result<Response> {
    let world = call.calendar_world();
    let origin = origin(&world);
    let view = call.view(&world, &origin);
    let filter = call
        .options
        .filter
        .as_deref()
        .map(Expr::parse)
        .transpose()?;
    let all: Vec<Map<String, Value>> = view
        .calendar_entries()
        .into_iter()
        .map(|entry| view.calendar_json(entry))
        .filter(|json| {
            filter
                .as_ref()
                .is_none_or(|f| f.matches(&Value::Object(json.clone())))
        })
        .collect();
    let total = all.len();
    let top = call.options.top.unwrap_or(10).clamp(1, MAX_PAGE);
    let skip = call.options.skip;
    let value = all
        .into_iter()
        .skip(skip)
        .take(top)
        .map(|json| {
            odata::select(
                json,
                call.options.select.as_deref(),
                CALENDAR_PROPERTIES,
                "calendar",
            )
            .map(Value::Object)
        })
        .collect::<Result<Vec<_>>>()?;
    let next = (skip + top < total).then(|| {
        odata::link(
            &origin,
            "me/calendars",
            &odata::next_page(&call.query, skip + top),
        )
    });
    Ok(reply(
        200,
        odata::collection(
            view.context("calendars"),
            value,
            call.options.count.then_some(total),
            next,
        ),
    ))
}

/// `GET /me/calendar` and `GET /me/calendars/{id}`.
pub async fn get_calendar(call: Call, uri: Uri, Path(params): super::Params) -> Result<Response> {
    let world = call.calendar_world();
    let origin = origin(&world);
    let view = call.view(&world, &origin);
    let calendar = view.calendar_of(&params)?;
    let entry = view.calendars.calendar(&calendar)?;
    let json = odata::select(
        view.calendar_json(entry),
        call.options.select.as_deref(),
        CALENDAR_PROPERTIES,
        "calendar",
    )?;
    let context = if uri.path().ends_with("/calendar") {
        "calendar/$entity".to_owned()
    } else {
        "calendars/$entity".to_owned()
    };
    Ok(reply_map(200, entity(view.context(&context), json)))
}

/// `GET /me/events` and a calendar's: its single events and series, not their occurrences.
pub async fn list(call: Call, uri: Uri, Path(params): super::Params) -> Result<Response> {
    let world = call.calendar_world();
    let origin = origin(&world);
    let view = call.view(&world, &origin);
    let calendar = view.calendar_of(&params)?;
    let events: Vec<Event> = view
        .calendars
        .stored(&calendar)
        .map(|stored| stored.event.clone())
        .filter(|event| {
            event.recurring_event_id.is_none() && event.status.as_deref() != Some("cancelled")
        })
        .collect();
    list_events(&call, &view, uri.path(), &calendar, events)
}

/// `GET /me/calendarView` and a calendar's: every event in the window, a series' occurrences
/// and exceptions included.
pub async fn calendar_view(call: Call, uri: Uri, Path(params): super::Params) -> Result<Response> {
    let (start, end) = window(&call)?;
    let world = call.calendar_world();
    let origin = origin(&world);
    let view = call.view(&world, &origin);
    let calendar = view.calendar_of(&params)?;
    let events = view.occurrences(&calendar, start, end);
    list_events(&call, &view, uri.path(), &calendar, events)
}

/// `GET /me/events/{id}/instances`: a series' occurrences and exceptions in the window.
pub async fn instances(call: Call, uri: Uri, Path(params): super::Params) -> Result<Response> {
    let (start, end) = window(&call)?;
    let world = call.calendar_world();
    let origin = origin(&world);
    let view = call.view(&world, &origin);
    let (calendar, series) = view.event_of(&params)?;
    let events: Vec<Event> = if series.recurrence.is_empty() {
        Vec::new()
    } else {
        view.occurrences(&calendar, start, end)
            .into_iter()
            .filter(|event| event.recurring_event_id == series.id)
            .collect()
    };
    list_events(&call, &view, uri.path(), &calendar, events)
}

/// `GET /me/events/{id}`.
pub async fn get(call: Call, Path(params): super::Params) -> Result<Response> {
    let world = call.calendar_world();
    let origin = origin(&world);
    let view = call.view(&world, &origin);
    let (calendar, event) = view.event_of(&params)?;
    let mut json = view.event_json(&calendar, &event, call.options.select.as_deref())?;
    Expand::parse(call.options.expand.as_deref())?.apply(&view, &calendar, &mut json);
    Ok(reply_map(200, entity(view.context("events/$entity"), json)))
}

/// `POST /me/events` and a calendar's: a new event (201). A `transactionId` already used for an
/// event of the calendar gives that event back instead of a second one.
pub async fn create(
    call: Call,
    uri: Uri,
    Path(params): super::Params,
    body: axum::body::Bytes,
) -> Result<Response> {
    let request: Map<String, Value> = super::folders::body_json(&body, "event")?;
    let mut world = call.calendar_world();
    let origin = origin(&world);
    let context = format!("{}/$entity", fragment(uri.path()));
    let (calendar, providers, tz) = {
        let view = call.view(&world, &origin);
        let calendar = view.calendar_of(&params)?;
        if !view.can_edit(&calendar) {
            return Err(GraphError::access_denied());
        }
        let providers = view.providers(&calendar);
        (calendar, providers, view.calendars.tz.clone())
    };
    let mut event = Event::default();
    let mut stored = Map::new();
    let target = Target { tz: &tz, providers };
    event_input::apply(&target, &mut event, &mut stored, &request, true)?;
    {
        let view = call.view(&world, &origin);
        let repeated = stored.get("transactionId").and_then(|transaction| {
            world
                .graph
                .events
                .iter()
                .filter(|((cal, _), kept)| {
                    *cal == calendar && kept.get("transactionId") == Some(transaction)
                })
                .find_map(|((_, id), _)| view.calendars.event(&calendar, id).ok())
                .filter(|event| event.status.as_deref() != Some("cancelled"))
        });
        if let Some(event) = repeated {
            let json = view.event_json(&calendar, &event, None)?;
            return Ok(reply_map(201, entity(view.context(&context), json)));
        }
    }
    let event = crate::calendar::routes::stamp_new(&mut world, event)?;
    let id = event.id.clone().unwrap_or_default();
    if stored.get("isOnlineMeeting") == Some(&Value::Bool(true)) {
        stored.insert(
            "onlineMeeting".into(),
            events::teams_meeting(&origin, &calendar, &id),
        );
    }
    world.calendar.put(&calendar, event.clone());
    if !stored.is_empty() {
        world.graph.events.insert((calendar.clone(), id), stored);
    }
    let view = call.view(&world, &origin);
    let json = view.event_json(&calendar, &event, None)?;
    Ok(reply_map(201, entity(view.context(&context), json)))
}

/// `PATCH /me/events/{id}`: the changes, applied to the event (an occurrence becomes an
/// exception of its series).
pub async fn update(
    call: Call,
    Path(params): super::Params,
    body: axum::body::Bytes,
) -> Result<Response> {
    let request: Map<String, Value> = super::folders::body_json(&body, "event")?;
    let mut world = call.calendar_world();
    let origin = origin(&world);
    let (calendar, mut event, providers, tz) = {
        let view = call.view(&world, &origin);
        let (calendar, event) = view.event_of(&params)?;
        if !view.can_edit(&calendar) {
            return Err(GraphError::access_denied());
        }
        let providers = view.providers(&calendar);
        (calendar, event, providers, view.calendars.tz.clone())
    };
    let id = event.id.clone().unwrap_or_default();
    let key = (calendar.clone(), id.clone());
    let mut stored = world.graph.events.get(&key).cloned().unwrap_or_default();
    let target = Target { tz: &tz, providers };
    event_input::apply(&target, &mut event, &mut stored, &request, false)?;
    crate::calendar::routes::check_recurrence(&event)?;
    if stored.get("isOnlineMeeting") == Some(&Value::Bool(true))
        && !stored.contains_key("onlineMeeting")
    {
        stored.insert(
            "onlineMeeting".into(),
            events::teams_meeting(&origin, &calendar, &id),
        );
    }
    let event = crate::calendar::routes::touch(&world, event);
    world.calendar.put(&calendar, event.clone());
    if stored.is_empty() {
        world.graph.events.remove(&key);
    } else {
        world.graph.events.insert(key, stored);
    }
    let view = call.view(&world, &origin);
    let json = view.event_json(&calendar, &event, None)?;
    Ok(reply_map(200, entity(view.context("events/$entity"), json)))
}

/// `DELETE /me/events/{id}` (204): the event leaves the calendar, a series with its exceptions.
pub async fn delete(call: Call, Path(params): super::Params) -> Result<Response> {
    let mut world = call.calendar_world();
    let origin = origin(&world);
    let (calendar, event) = {
        let view = call.view(&world, &origin);
        let (calendar, event) = view.event_of(&params)?;
        if !view.can_edit(&calendar) {
            return Err(GraphError::access_denied());
        }
        (calendar, event)
    };
    crate::calendar::routes::cancel_event(&mut world, &calendar, event);
    Ok(StatusCode::NO_CONTENT.into_response())
}

/// What the parameters a calendar view's delta refuses are, as Graph lists them.
const DELTA_UNSUPPORTED: &str = "The following parameters are not supported with change tracking over the 'CalendarView' resource: '$orderby, $filter, $select, $expand, $search, $top'.";

/// A delta round's state token: the round, and for a page within it, where the page starts.
fn token(round: usize, offset: Option<usize>) -> String {
    odata::encode_token(&(round, offset))
}

fn read_token(text: &str) -> Option<(usize, Option<usize>)> {
    odata::decode_token(text)
}

/// `GET /me/calendarView/delta` and a calendar's: the view's events, then what was added to,
/// changed in and removed from it since. An event that leaves the window, and a change to an
/// event outside it, come back as `@removed`.
pub async fn view_delta(call: Call, uri: Uri, Path(params): super::Params) -> Result<Response> {
    let options = &call.options;
    if options.select.is_some()
        || options.expand.is_some()
        || options.filter.is_some()
        || options.orderby.is_some()
        || options.search.is_some()
        || options.top.is_some()
    {
        return Err(GraphError::new(
            400,
            "ErrorInvalidUrlQuery",
            DELTA_UNSUPPORTED,
        ));
    }
    let mut world = call.calendar_world();
    let origin = origin(&world);
    let calendar = call.view(&world, &origin).calendar_of(&params)?;
    let state = options
        .skiptoken
        .as_deref()
        .or(options.deltatoken.as_deref())
        .map(|text| read_token(text).ok_or_else(GraphError::sync_state_not_found))
        .transpose()?;
    if let Some((round, _)) = state {
        let known = world
            .graph
            .rounds
            .get(round)
            .ok_or_else(GraphError::sync_state_not_found)?;
        if known.calendar != calendar {
            return Err(GraphError::bad_request(
                "The state token belongs to another calendar.",
            ));
        }
    }
    let (round, offset) = match state {
        Some((round, Some(offset))) if options.skiptoken.is_some() => (round, offset),
        Some((base, _)) => {
            let base = world.graph.rounds[base].clone();
            let next = take_round(
                &call,
                &world,
                &origin,
                &calendar,
                base.start,
                base.end,
                Some(&base),
            );
            world.graph.rounds.push(next);
            (world.graph.rounds.len() - 1, 0)
        }
        None => {
            let (start, end) = window(&call)?;
            let next = take_round(&call, &world, &origin, &calendar, start, end, None);
            world.graph.rounds.push(next);
            (world.graph.rounds.len() - 1, 0)
        }
    };
    let view = call.view(&world, &origin);
    let entries = &world.graph.rounds[round].entries;
    let size = call
        .prefer
        .max_page
        .unwrap_or(DELTA_PAGE)
        .clamp(1, MAX_PAGE);
    let mut value = Vec::new();
    for (id, removed) in entries.iter().skip(offset).take(size) {
        let event = (!removed)
            .then(|| view.calendars.event(&calendar, id).ok())
            .flatten()
            .filter(|event| event.status.as_deref() != Some("cancelled"));
        let mut json = match event {
            Some(event) => view.event_json(&calendar, &event, None)?,
            None => {
                let mut gone = Map::new();
                gone.insert("id".into(), ids::event(&calendar, id).into());
                gone.insert("@removed".into(), json!({ "reason": "deleted" }));
                gone
            }
        };
        json.insert("@odata.type".into(), "#microsoft.graph.event".into());
        value.push(Value::Object(json));
    }
    let path = uri.path().trim_start_matches("/v1.0/");
    let mut body = Map::new();
    body.insert(
        "@odata.context".into(),
        odata::context(&origin, "Collection(event)").into(),
    );
    if offset + size < entries.len() {
        body.insert(
            "@odata.nextLink".into(),
            odata::link(
                &origin,
                path,
                &[("$skiptoken".into(), token(round, Some(offset + size)))],
            )
            .into(),
        );
    } else {
        body.insert(
            "@odata.deltaLink".into(),
            odata::link(&origin, path, &[("$deltatoken".into(), token(round, None))]).into(),
        );
    }
    body.insert("value".into(), value.into());
    Ok(reply_map(200, body))
}

/// A new delta round over `calendar`'s window: everything in it for a first round, else what
/// changed since `base`.
fn take_round(
    call: &Call,
    world: &crate::world::World,
    origin: &url::Url,
    calendar: &str,
    start: Timestamp,
    end: Timestamp,
    base: Option<&Round>,
) -> Round {
    let view = call.view(world, origin);
    let now: Vec<Event> = view.occurrences(calendar, start, end);
    let seen: BTreeMap<String, String> = now
        .iter()
        .map(|event| {
            (
                event.id.clone().unwrap_or_default(),
                view.event_change_key(calendar, event),
            )
        })
        .collect();
    let mut entries: Vec<(String, bool)> = now
        .iter()
        .filter_map(|event| {
            let id = event.id.clone().unwrap_or_default();
            let unchanged = base.is_some_and(|base| base.seen.get(&id) == seen.get(&id));
            (!unchanged).then_some((id, false))
        })
        .collect();
    if let Some(base) = base {
        entries.extend(
            base.seen
                .keys()
                .filter(|id| !seen.contains_key(*id))
                .map(|id| (id.clone(), true)),
        );
        // Changes outside the window, which Graph reports as removed.
        for stored in view.calendars.stored(calendar) {
            let id = stored.event.id.clone().unwrap_or_default();
            let inside = seen
                .keys()
                .any(|seen| *seen == id || seen.starts_with(&format!("{id}_")));
            if stored.revision > base.revision
                && !inside
                && !entries.iter().any(|(entry, _)| *entry == id)
            {
                entries.push((id, true));
            }
        }
    }
    Round {
        calendar: calendar.to_owned(),
        start,
        end,
        revision: view.calendars.revision(),
        entries,
        seen,
    }
}

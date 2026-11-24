//! The calendar's events as Outlook's `event` resource.

use gmail::calendar::{Event, EventDateTime};
use jiff::Timestamp;
use jiff::tz::TimeZone;
use serde_json::{Map, Value, json};

use super::ids;
use super::odata::{self, BodyType};
use super::project::{self, View};
use super::recurrence;

/// Every property of `event` (with `outlookItem`'s and `entity`'s).
pub const EVENT_PROPERTIES: &[&str] = &[
    "id",
    "createdDateTime",
    "lastModifiedDateTime",
    "changeKey",
    "categories",
    "transactionId",
    "originalStartTimeZone",
    "originalEndTimeZone",
    "iCalUId",
    "reminderMinutesBeforeStart",
    "isReminderOn",
    "hasAttachments",
    "subject",
    "bodyPreview",
    "importance",
    "sensitivity",
    "isAllDay",
    "isCancelled",
    "isOrganizer",
    "responseRequested",
    "seriesMasterId",
    "showAs",
    "type",
    "webLink",
    "onlineMeetingUrl",
    "isOnlineMeeting",
    "onlineMeetingProvider",
    "allowNewTimeProposals",
    "isDraft",
    "hideAttendees",
    "responseStatus",
    "body",
    "start",
    "end",
    "location",
    "locations",
    "recurrence",
    "attendees",
    "organizer",
    "onlineMeeting",
    "originalStart",
    "cancelledOccurrences",
];

/// What an event carries without `$select`: all but `cancelledOccurrences`, which Graph returns
/// only when selected.
const EVENT_DEFAULT: usize = EVENT_PROPERTIES.len() - 1;

/// A time in the zone `Prefer: outlook.timezone` asks for (UTC otherwise), as a
/// `dateTimeTimeZone` with Outlook's seven fractional digits.
pub fn date_time(at: Timestamp, zone: Option<&str>) -> Value {
    let (tz, name) = match zone {
        Some(name) => match gmail::ics::resolve_timezone(name) {
            Some(tz) => (tz, name.to_owned()),
            None => (TimeZone::UTC, "UTC".to_owned()),
        },
        None => (TimeZone::UTC, "UTC".to_owned()),
    };
    json!({
        "dateTime": at.to_zoned(tz).strftime("%Y-%m-%dT%H:%M:%S.0000000").to_string(),
        "timeZone": name,
    })
}

/// A place by name, as Outlook writes a typed-in location.
pub fn location(name: &str) -> Value {
    if name.is_empty() {
        json!({ "displayName": "", "locationType": "default", "uniqueIdType": "unknown" })
    } else {
        json!({ "displayName": name, "locationType": "default", "uniqueId": name, "uniqueIdType": "private" })
    }
}

/// Google's response to Outlook's.
fn response(status: Option<&str>) -> &'static str {
    match status {
        Some("accepted") => "accepted",
        Some("tentative") => "tentativelyAccepted",
        Some("declined") => "declined",
        _ => "none",
    }
}

/// The zone an event's times were written in, by Outlook's (Windows) name.
fn zone_name(time: Option<&EventDateTime>, calendar_tz: &TimeZone) -> String {
    let iana = time
        .and_then(|t| t.time_zone.clone())
        .unwrap_or_else(|| calendar_tz.iana_name().unwrap_or("UTC").to_owned());
    project::windows_zone(&iana)
}

/// The Teams meeting Outlook makes for an online event: a join link on the mock's origin and a
/// conference ID, both derived from the event.
pub fn teams_meeting(origin: &url::Url, calendar: &str, event: &str) -> Value {
    let seed = crate::fixtures::fnv(&format!("{calendar}/{event}"));
    let thread = ids::guid(&format!("teams/{calendar}/{event}"));
    json!({
        "joinUrl": format!("{origin}l/meetup-join/19%3ameeting_{thread}%40thread.v2/0"),
        "conferenceId": (seed % 1_000_000_000).to_string(),
        "phones": [],
        "tollFreeNumbers": [],
    })
}

/// Outlook's HTML body for a calendar description, which the calendar keeps as HTML.
fn html_body(description: &str) -> String {
    if description.trim_start().starts_with("<html") {
        description.to_owned()
    } else {
        format!("<html><head></head><body>{description}</body></html>")
    }
}

impl View<'_> {
    /// `calendar`'s `event` as Outlook renders it, with the selected properties or the default
    /// ones.
    pub fn event_json(
        &self,
        calendar: &str,
        event: &Event,
        select: Option<&[String]>,
    ) -> super::error::Result<Map<String, Value>> {
        let wanted: Vec<&str> = match select {
            Some(select) => {
                odata::check_properties(select, EVENT_PROPERTIES, "event")?;
                EVENT_PROPERTIES
                    .iter()
                    .copied()
                    .filter(|name| select.iter().any(|s| s.eq_ignore_ascii_case(name)))
                    .collect()
            }
            None => EVENT_PROPERTIES[..EVENT_DEFAULT].to_vec(),
        };
        let id = event.id.clone().unwrap_or_default();
        let change_key = self.event_change_key(calendar, event);
        let stored = self.event_stored(calendar, event);
        let mut json = Map::new();
        json.insert("@odata.etag".into(), format!("W/\"{change_key}\"").into());
        json.insert("id".into(), ids::event(calendar, &id).into());
        for name in wanted.into_iter().filter(|name| *name != "id") {
            let value = match stored.get(name) {
                Some(value) => value.clone(),
                None => self.event_property(name, calendar, event, &change_key),
            };
            // Single events and series have no original start, which Graph leaves out.
            if name == "originalStart" && value.is_null() {
                continue;
            }
            json.insert(name.to_owned(), value);
        }
        Ok(json)
    }

    /// An event's change key: it moves with every change the calendar stores (an instance's
    /// with its series').
    pub fn event_change_key(&self, calendar: &str, event: &Event) -> String {
        let own = event.id.as_deref();
        let series = event.recurring_event_id.as_deref();
        let revision = self
            .calendars
            .stored(calendar)
            .find(|s| s.event.id.as_deref() == own)
            .or_else(|| {
                self.calendars
                    .stored(calendar)
                    .find(|s| series.is_some() && s.event.id.as_deref() == series)
            })
            .map_or(0, |s| s.revision);
        ids::change_key(crate::fixtures::fnv(own.unwrap_or_default()), revision)
    }

    /// The properties Outlook keeps on `event` itself: its series', then its own.
    fn event_stored(&self, calendar: &str, event: &Event) -> Map<String, Value> {
        let get = |id: Option<&String>| {
            id.and_then(|id| self.outlook.events.get(&(calendar.to_owned(), id.clone())))
        };
        let mut stored = get(event.recurring_event_id.as_ref())
            .cloned()
            .unwrap_or_default();
        if let Some(own) = get(event.id.as_ref()) {
            stored.extend(own.clone());
        }
        // A series' own transaction isn't its occurrences'.
        if event.recurring_event_id.is_some() && get(event.id.as_ref()).is_none() {
            stored.remove("transactionId");
        }
        stored
    }

    /// Who owns `calendar`: the signed-in user for their own, else the calendar itself, by name
    /// and address.
    pub fn calendar_owner(&self, calendar: &str) -> (String, String) {
        let entry = self.calendars.calendar(calendar).ok();
        let role = entry.and_then(|e| e.access_role.as_deref());
        if calendar == self.calendars.primary || role == Some("owner") {
            (self.who.name.clone(), self.calendars.primary.clone())
        } else {
            let name = entry
                .and_then(|e| e.summary.clone())
                .unwrap_or_else(|| calendar.to_owned());
            (name, calendar.to_owned())
        }
    }

    fn event_property(&self, name: &str, calendar: &str, event: &Event, change_key: &str) -> Value {
        let calendars = self.calendars;
        let tz = &calendars.tz;
        let me = calendars.primary.to_lowercase();
        let organizer = event.organizer.as_ref();
        let is_organizer = organizer.is_none_or(|o| {
            o.is_self == Some(true)
                || o.email
                    .as_deref()
                    .is_some_and(|e| e.eq_ignore_ascii_case(&me))
        });
        let mine = event.attendees.iter().find(|a| {
            a.is_self == Some(true)
                || a.email
                    .as_deref()
                    .is_some_and(|e| e.eq_ignore_ascii_case(&me))
        });
        let all_day = event.start.as_ref().is_some_and(|s| s.date.is_some());
        let zone = self.prefer.zone.as_deref();
        let at =
            |time: Option<&EventDateTime>| time.and_then(|t| t.timestamp(tz)).unwrap_or_default();
        let stamp = |text: Option<&str>| {
            text.and_then(|t| t.parse::<Timestamp>().ok())
                .map_or(Value::Null, |t| odata::datetime(t).into())
        };
        let description = event.description.clone().unwrap_or_default();
        let reminders = event.reminders.as_ref();
        let reminder = match reminders {
            Some(r) if r.use_default == Some(false) => r.overrides.first().and_then(|o| o.minutes),
            _ => calendars
                .calendar(calendar)
                .ok()
                .and_then(|entry| entry.default_reminders.first())
                .and_then(|r| r.minutes),
        };
        match name {
            "createdDateTime" => stamp(event.created.as_deref()),
            "lastModifiedDateTime" => stamp(event.updated.as_deref()),
            "changeKey" => change_key.into(),
            "categories" => json!([]),
            "transactionId" => Value::Null,
            "originalStartTimeZone" => zone_name(event.start.as_ref(), tz).into(),
            "originalEndTimeZone" => zone_name(event.end.as_ref(), tz).into(),
            "iCalUId" => event.ical_uid.clone().map_or(Value::Null, Value::from),
            "reminderMinutesBeforeStart" => reminder.map_or(15, i64::from).into(),
            "isReminderOn" => reminder.is_some().into(),
            "hasAttachments" => false.into(),
            "subject" => event.summary.clone().unwrap_or_default().into(),
            "bodyPreview" => {
                let text = mail_parser::decoders::html::html_to_text(&description);
                text.split_whitespace().collect::<Vec<_>>().join(" ").chars().take(255).collect::<String>().into()
            }
            "importance" => "normal".into(),
            "sensitivity" => match event.visibility.as_deref() {
                Some("private") => "private",
                Some("confidential") => "confidential",
                _ => "normal",
            }
            .into(),
            "isAllDay" => all_day.into(),
            "isCancelled" => (event.status.as_deref() == Some("cancelled")).into(),
            "isOrganizer" => is_organizer.into(),
            "responseRequested" => true.into(),
            "seriesMasterId" => event
                .recurring_event_id
                .as_deref()
                .map_or(Value::Null, |master| ids::event(calendar, master).into()),
            "showAs" => {
                let response = mine.and_then(|a| a.response_status.as_deref());
                if event.event_type.as_deref() == Some("outOfOffice") {
                    "oof"
                } else if event.event_type.as_deref() == Some("workingLocation") {
                    "workingElsewhere"
                } else if event.transparency.as_deref() == Some("transparent") || response == Some("declined") {
                    "free"
                } else if !is_organizer && matches!(response, None | Some("needsAction") | Some("tentative")) {
                    "tentative"
                } else {
                    "busy"
                }
                .into()
            }
            "type" => if !event.recurrence.is_empty() {
                "seriesMaster"
            } else if event.recurring_event_id.is_some() {
                let stored = calendars.stored(calendar).any(|s| s.event.id == event.id);
                if stored { "exception" } else { "occurrence" }
            } else {
                "singleInstance"
            }
            .into(),
            "webLink" => {
                let id: String = url::form_urlencoded::byte_serialize(ids::event(calendar, event.id.as_deref().unwrap_or_default()).as_bytes()).collect();
                format!("{}owa/?itemid={id}&exvsurl=1&path=/calendar/item", self.origin).into()
            }
            "onlineMeetingUrl" => Value::Null,
            "isOnlineMeeting" => event.hangout_link.is_some().into(),
            "onlineMeetingProvider" => "unknown".into(),
            "allowNewTimeProposals" => true.into(),
            "isDraft" => false.into(),
            "hideAttendees" => (event.guests_can_see_other_guests == Some(false)).into(),
            "responseStatus" => {
                let response = if is_organizer { "organizer" } else { response(mine.and_then(|a| a.response_status.as_deref())) };
                json!({ "response": response, "time": "0001-01-01T00:00:00Z" })
            }
            "body" => match self.prefer.body {
                Some(BodyType::Text) => json!({ "contentType": "text", "content": mail_parser::decoders::html::html_to_text(&description) }),
                _ => json!({ "contentType": "html", "content": html_body(&description) }),
            },
            "start" | "end" => {
                let time = if name == "start" { event.start.as_ref() } else { event.end.as_ref() };
                match time.and_then(|t| t.date.as_deref()) {
                    // An all-day event's days are midnights in whatever zone it's read in.
                    Some(date) => json!({
                        "dateTime": format!("{date}T00:00:00.0000000"),
                        "timeZone": zone.unwrap_or("UTC"),
                    }),
                    None => date_time(at(time), zone),
                }
            }
            "location" => location(event.location.as_deref().unwrap_or_default()),
            "locations" => match event.location.as_deref().filter(|l| !l.is_empty()) {
                Some(place) => json!([{ "displayName": place, "locationType": "default", "uniqueIdType": "unknown" }]),
                None => json!([]),
            },
            "recurrence" => {
                let start = at(event.start.as_ref());
                let zone_tz = event.start.as_ref().and_then(|s| s.time_zone.as_deref()).and_then(|z| TimeZone::get(z).ok()).unwrap_or_else(|| tz.clone());
                recurrence::to_graph(&event.recurrence, start.to_zoned(zone_tz.clone()).date(), &zone_name(event.start.as_ref(), tz), &zone_tz)
                    .unwrap_or(Value::Null)
            }
            "attendees" => event
                .attendees
                .iter()
                .filter(|a| a.organizer != Some(true))
                .map(|a| {
                    let email = a.email.clone().unwrap_or_default();
                    json!({
                        "type": if a.resource == Some(true) { "resource" } else if a.optional == Some(true) { "optional" } else { "required" },
                        "status": { "response": response(a.response_status.as_deref()), "time": "0001-01-01T00:00:00Z" },
                        "emailAddress": { "name": a.display_name.clone().unwrap_or_else(|| email.clone()), "address": email },
                    })
                })
                .collect::<Vec<_>>()
                .into(),
            "organizer" => {
                let (owner_name, owner_address) = self.calendar_owner(calendar);
                let email = organizer.and_then(|o| o.email.clone()).unwrap_or(owner_address);
                let name = organizer.and_then(|o| o.display_name.clone()).unwrap_or_else(|| {
                    if email.eq_ignore_ascii_case(&me) {
                        self.who.name.clone()
                    } else if organizer.is_none() {
                        owner_name
                    } else {
                        email.clone()
                    }
                });
                json!({ "emailAddress": { "name": name, "address": email } })
            }
            "onlineMeeting" => event.hangout_link.as_ref().map_or(Value::Null, |link| json!({ "joinUrl": link })),
            "originalStart" => event
                .original_start_time
                .as_ref()
                .and_then(|t| t.timestamp(tz))
                .map_or(Value::Null, |t| odata::datetime(t).into()),
            "cancelledOccurrences" => {
                let id = event.id.clone().unwrap_or_default();
                let master = ids::event(calendar, &id);
                calendars
                    .stored(calendar)
                    .filter(|s| s.event.recurring_event_id.as_deref() == Some(id.as_str()) && s.event.status.as_deref() == Some("cancelled"))
                    .filter_map(|s| s.event.original_start_time.as_ref())
                    .map(|start| {
                        let zone_tz = start.time_zone.as_deref().and_then(|z| TimeZone::get(z).ok()).unwrap_or_else(|| tz.clone());
                        let date = start.timestamp(&zone_tz).map(|t| t.to_zoned(zone_tz).date().to_string()).unwrap_or_default();
                        Value::from(format!("OID.{master}.{date}"))
                    })
                    .collect::<Vec<_>>()
                    .into()
            }
            _ => Value::Null,
        }
    }

    /// The event in the mailbox owner's primary calendar with iCalendar UID `uid` (the series,
    /// for a recurring one).
    pub fn event_by_uid(&self, uid: &str) -> Option<(String, Event)> {
        let calendar = self.calendars.primary.clone();
        self.calendars
            .stored(&calendar)
            .find(|stored| {
                stored.event.ical_uid.as_deref() == Some(uid)
                    && stored.event.recurring_event_id.is_none()
            })
            .map(|stored| (calendar.clone(), stored.event.clone()))
    }
}

/// Properties of `eventMessage`, then those `eventMessageRequest` and `eventMessageResponse` add.
const MEETING_PROPERTIES: &[&str] = &[
    "startDateTime",
    "endDateTime",
    "location",
    "type",
    "recurrence",
    "isAllDay",
    "isDelegated",
    "isOutOfDate",
    "meetingMessageType",
];
const REQUEST_PROPERTIES: &[&str] = &[
    "allowNewTimeProposals",
    "meetingRequestType",
    "previousEndDateTime",
    "previousLocation",
    "previousStartDateTime",
    "responseRequested",
];
const RESPONSE_PROPERTIES: &[&str] = &["proposedNewTime", "responseType"];

/// A `$select` split into a message's own properties and those cast to a meeting message's
/// type, as `(type, property)`.
type Selected = (Option<Vec<String>>, Vec<(String, String)>);

/// `$select` split into a message's own properties and those cast to a meeting message's type
/// (`microsoft.graph.eventMessage/meetingMessageType`).
pub fn split_select(select: Option<&[String]>) -> super::error::Result<Selected> {
    let Some(select) = select else {
        return Ok((None, Vec::new()));
    };
    let mut plain = Vec::new();
    let mut casts = Vec::new();
    for name in select {
        let Some((kind, property)) = name.trim_start_matches("microsoft.graph.").split_once('/')
        else {
            plain.push(name.clone());
            continue;
        };
        let known: Vec<&str> = match kind {
            "eventMessage" => MEETING_PROPERTIES.to_vec(),
            "eventMessageRequest" => [MEETING_PROPERTIES, REQUEST_PROPERTIES].concat(),
            "eventMessageResponse" => [MEETING_PROPERTIES, RESPONSE_PROPERTIES].concat(),
            _ => return Err(super::error::GraphError::unknown_property(name, "message")),
        };
        let found = known
            .iter()
            .find(|known| known.eq_ignore_ascii_case(property))
            .ok_or_else(|| super::error::GraphError::unknown_property(property, kind))?;
        casts.push((kind.to_owned(), (*found).to_owned()));
    }
    Ok((Some(plain), casts))
}

/// The iTIP object a message carries in its `text/calendar` part, which makes it a meeting
/// message.
pub fn meeting(message: &crate::mail::StoredMessage) -> Option<super::ics::Itip> {
    let mut stack = vec![&message.mime.payload];
    while let Some(part) = stack.pop() {
        if part.mime_type.as_deref() == Some("text/calendar")
            && let Some(data) = part.body.as_ref().and_then(|b| b.data.as_ref())
        {
            return super::ics::Itip::parse(&String::from_utf8_lossy(data));
        }
        stack.extend(part.parts.iter());
    }
    None
}

/// The Graph type a meeting message is: a request, a response (or counter-proposal), or a
/// plain `eventMessage` (a cancellation).
pub fn meeting_type(itip: &super::ics::Itip) -> &'static str {
    match itip.method.as_str() {
        "REQUEST" => "eventMessageRequest",
        "REPLY" | "COUNTER" => "eventMessageResponse",
        _ => "eventMessage",
    }
}

/// The meeting properties a message shows: all its type's without `$select`, else the ones
/// cast to a type it is.
pub fn meeting_properties(
    itip: &super::ics::Itip,
    all: bool,
    casts: &[(String, String)],
) -> Vec<&'static str> {
    let kind = meeting_type(itip);
    let own: Vec<&'static str> = match kind {
        "eventMessageRequest" => [MEETING_PROPERTIES, REQUEST_PROPERTIES].concat(),
        "eventMessageResponse" => [MEETING_PROPERTIES, RESPONSE_PROPERTIES].concat(),
        _ => MEETING_PROPERTIES.to_vec(),
    };
    own.into_iter()
        .filter(|name| {
            all || casts.iter().any(|(cast, property)| {
                (cast == "eventMessage" || cast == kind) && property == name
            })
        })
        .collect()
}

impl View<'_> {
    /// One of a meeting message's own properties.
    pub fn meeting_property(
        &self,
        name: &str,
        itip: &super::ics::Itip,
        message: &crate::mail::StoredMessage,
    ) -> Value {
        let tz = &self.calendars.tz;
        let zone = self.prefer.zone.as_deref();
        let time = |when: Option<super::ics::When>| {
            when.and_then(|w| w.timestamp(tz))
                .map_or(Value::Null, |at| date_time(at, zone))
        };
        let sender = message
            .mime
            .payload
            .header("From")
            .map(|from| gmail::mime::Mailbox::parse(from).email)
            .unwrap_or_default();
        let me = self.mail.email.clone();
        match name {
            "startDateTime" => time(itip.start),
            "endDateTime" => time(itip.end),
            "location" => location(&itip.location),
            "type" => if !itip.recurrence.is_empty() {
                "seriesMaster"
            } else if itip.recurrence_id.is_some() {
                "exception"
            } else {
                "singleInstance"
            }
            .into(),
            "recurrence" => {
                let start = itip.start.and_then(|w| w.timestamp(tz)).unwrap_or_default();
                let zone_name = project::windows_zone(tz.iana_name().unwrap_or("UTC"));
                recurrence::to_graph(
                    &itip.recurrence,
                    start.to_zoned(tz.clone()).date(),
                    &zone_name,
                    tz,
                )
                .unwrap_or(Value::Null)
            }
            "isAllDay" => matches!(itip.start, Some(super::ics::When::Day(_))).into(),
            "isDelegated" => false.into(),
            "isOutOfDate" => self
                .event_by_uid(&itip.uid)
                .is_some_and(|(_, event)| {
                    event.sequence.unwrap_or_default() > itip.sequence
                        || (itip.method == "REQUEST"
                            && event.status.as_deref() == Some("cancelled"))
                })
                .into(),
            "meetingMessageType" => match itip.method.as_str() {
                "REQUEST" => "meetingRequest",
                "CANCEL" => "meetingCancelled",
                "COUNTER" => "meetingTenativelyAccepted",
                "REPLY" => match itip.partstat(&sender) {
                    Some("ACCEPTED") => "meetingAccepted",
                    Some("TENTATIVE") => "meetingTenativelyAccepted",
                    Some("DECLINED") => "meetingDeclined",
                    _ => "none",
                },
                _ => "none",
            }
            .into(),
            "allowNewTimeProposals" => (!itip.no_counter).into(),
            "meetingRequestType" => if itip.sequence == 0 {
                "newMeetingRequest"
            } else {
                "fullUpdate"
            }
            .into(),
            "previousEndDateTime" | "previousStartDateTime" | "previousLocation" => Value::Null,
            "responseRequested" => itip
                .attendees
                .iter()
                .find(|(email, ..)| email.eq_ignore_ascii_case(&me))
                .is_none_or(|(.., rsvp)| *rsvp)
                .into(),
            "proposedNewTime" if itip.method == "COUNTER" => {
                json!({ "start": time(itip.start), "end": time(itip.end) })
            }
            "proposedNewTime" => Value::Null,
            "responseType" => match (itip.method.as_str(), itip.partstat(&sender)) {
                ("COUNTER", _) | (_, Some("TENTATIVE")) => "tentativelyAccepted",
                (_, Some("ACCEPTED")) => "accepted",
                (_, Some("DECLINED")) => "declined",
                _ => "none",
            }
            .into(),
            _ => Value::Null,
        }
    }
}

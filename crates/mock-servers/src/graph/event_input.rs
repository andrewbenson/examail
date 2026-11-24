//! An event as a client writes it (a create, or a PATCH's changes), applied to the calendar's
//! event and to the properties Outlook keeps beside it ([`super::Outlook::events`]).
//!
//! The calendar holds what it has a place for: subject, body, times, location, attendees,
//! recurrence, free/busy, visibility, reminders and whether guests see each other. The rest
//! (`categories`, `importance`, `showAs` beyond free and busy, `personal` sensitivity, a
//! transaction ID, the response and proposal switches, several locations, a Teams meeting) is
//! kept beside it.

use gmail::calendar::{Event, EventAttendee, EventDateTime, EventReminder, EventReminders};
use jiff::Timestamp;
use jiff::civil::DateTime;
use jiff::tz::TimeZone;
use serde_json::{Map, Value};

use super::error::{GraphError, Result};
use super::events::EVENT_PROPERTIES;
use super::recurrence;

/// What a client may write; the rest of an event's properties are read-only and ignored, as
/// Outlook ignores them.
const WRITABLE: &[&str] = &[
    "subject",
    "body",
    "start",
    "end",
    "isAllDay",
    "location",
    "locations",
    "attendees",
    "recurrence",
    "showAs",
    "sensitivity",
    "importance",
    "categories",
    "isReminderOn",
    "reminderMinutesBeforeStart",
    "responseRequested",
    "allowNewTimeProposals",
    "hideAttendees",
    "isOnlineMeeting",
    "onlineMeetingProvider",
    "transactionId",
];

/// Where a write lands: the calendar's zone (for all-day times), and the online meeting
/// providers it allows.
pub struct Target<'a> {
    pub tz: &'a TimeZone,
    pub providers: &'a [&'static str],
}

/// Applies `body` to `event` and `stored`. `creating` is a new event's first write, where the
/// times are required and `transactionId` may be set.
pub fn apply(
    target: &Target<'_>,
    event: &mut Event,
    stored: &mut Map<String, Value>,
    body: &Map<String, Value>,
    creating: bool,
) -> Result<()> {
    for name in body.keys().filter(|name| !name.starts_with('@')) {
        if !EVENT_PROPERTIES.contains(&name.as_str()) {
            return Err(GraphError::unknown_body_property(name, "event"));
        }
    }
    let writes = |name: &str| WRITABLE.contains(&name) && body.contains_key(name);
    if creating && (!body.contains_key("start") || !body.contains_key("end")) {
        return Err(GraphError::invalid_request(
            "An event needs its start and end.",
        ));
    }
    if let Some(subject) = body.get("subject") {
        event.summary = text(subject, "subject")?.filter(|s| !s.is_empty());
    }
    if let Some(item) = body.get("body") {
        event.description = Some(description(item)?).filter(|d| !d.is_empty());
    }
    times(target, event, body)?;
    if writes("location") {
        let name = body["location"]
            .get("displayName")
            .map(|v| text(v, "location"))
            .transpose()?
            .flatten()
            .unwrap_or_default();
        event.location = Some(name).filter(|n| !n.is_empty());
        stored.remove("locations");
    } else if let Some(locations) = body.get("locations") {
        let places = locations
            .as_array()
            .ok_or_else(|| GraphError::unreadable_body("locations must be a collection"))?;
        let names: Vec<&str> = places
            .iter()
            .filter_map(|place| place.get("displayName")?.as_str())
            .filter(|name| !name.is_empty())
            .collect();
        event.location = Some(names.join("; ")).filter(|n| !n.is_empty());
        if places.len() > 1 {
            stored.insert("locations".into(), locations.clone());
        } else {
            stored.remove("locations");
        }
    }
    if let Some(attendees) = body.get("attendees") {
        event.attendees = attendees_of(event, attendees)?;
    }
    if let Some(rule) = body.get("recurrence") {
        series(target, event, rule)?;
    }
    if let Some(show_as) = body.get("showAs") {
        let value = choice(
            show_as,
            &[
                "free",
                "tentative",
                "busy",
                "oof",
                "workingElsewhere",
                "unknown",
            ],
        )?;
        event.transparency = Some(
            if value == "free" {
                "transparent"
            } else {
                "opaque"
            }
            .into(),
        );
        keep(stored, "showAs", show_as, !matches!(value, "free" | "busy"));
    }
    if let Some(sensitivity) = body.get("sensitivity") {
        let value = choice(
            sensitivity,
            &["normal", "personal", "private", "confidential"],
        )?;
        event.visibility = match value {
            "private" | "confidential" => Some(value.to_owned()),
            _ => None,
        };
        keep(stored, "sensitivity", sensitivity, value == "personal");
    }
    if let Some(importance) = body.get("importance") {
        let value = choice(importance, &["low", "normal", "high"])?;
        keep(stored, "importance", importance, value != "normal");
    }
    if let Some(categories) = body.get("categories") {
        let empty = categories.as_array().is_none_or(Vec::is_empty);
        keep(stored, "categories", categories, !empty);
    }
    for (name, default) in [("responseRequested", true), ("allowNewTimeProposals", true)] {
        if let Some(value) = body.get(name) {
            let on = flag(value, name)?;
            keep(stored, name, value, on != default);
        }
    }
    if let Some(hide) = body.get("hideAttendees") {
        event.guests_can_see_other_guests = Some(!flag(hide, "hideAttendees")?);
    }
    reminders(event, body)?;
    if creating && let Some(transaction) = body.get("transactionId") {
        keep(stored, "transactionId", transaction, !transaction.is_null());
    }
    online(target, event, stored, body)
}

/// A string property, or null.
fn text(value: &Value, name: &str) -> Result<Option<String>> {
    match value {
        Value::Null => Ok(None),
        Value::String(text) => Ok(Some(text.clone())),
        _ => Err(GraphError::unreadable_body(format!(
            "{name} must be a string"
        ))),
    }
}

fn flag(value: &Value, name: &str) -> Result<bool> {
    value
        .as_bool()
        .ok_or_else(|| GraphError::unreadable_body(format!("{name} must be a Boolean")))
}

/// An enumeration's value, which Graph matches without regard to case.
fn choice<'a>(value: &Value, members: &[&'a str]) -> Result<&'a str> {
    let given = value.as_str().unwrap_or_default();
    members
        .iter()
        .find(|member| member.eq_ignore_ascii_case(given))
        .copied()
        .ok_or_else(|| {
            GraphError::unreadable_body(format!("Requested value '{given}' was not found."))
        })
}

/// Keeps `value` as Outlook's own property when `keep`, else forgets it (its default).
fn keep(stored: &mut Map<String, Value>, name: &str, value: &Value, keep: bool) {
    if keep {
        stored.insert(name.to_owned(), value.clone());
    } else {
        stored.remove(name);
    }
}

/// An `itemBody` as the calendar's description, which holds HTML: text bodies are escaped, with
/// their line breaks kept.
fn description(item: &Value) -> Result<String> {
    let content = item
        .get("content")
        .and_then(Value::as_str)
        .unwrap_or_default();
    let kind = item
        .get("contentType")
        .and_then(Value::as_str)
        .unwrap_or("text");
    Ok(if kind.eq_ignore_ascii_case("html") {
        content.to_owned()
    } else {
        crate::mail::mime::escape_html_text(content)
            .replace("\r\n", "\n")
            .replace('\n', "<br>")
    })
}

/// A `dateTimeTimeZone` as the instant and zone it names. A zone Outlook doesn't know is
/// refused.
fn instant(value: &Value, name: &str) -> Result<(Timestamp, TimeZone, String)> {
    let text = value
        .get("dateTime")
        .and_then(Value::as_str)
        .ok_or_else(|| GraphError::unreadable_body(format!("{name} needs its dateTime")))?;
    let zone_name = value
        .get("timeZone")
        .and_then(Value::as_str)
        .unwrap_or("UTC");
    let tz = gmail::ics::resolve_timezone(zone_name).ok_or_else(|| {
        GraphError::new(
            400,
            "TimeZoneNotSupportedException",
            format!(
                "A valid TimeZone value must be specified. The following TimeZone value is not supported: '{zone_name}'."
            ),
        )
    })?;
    let civil: DateTime = text
        .trim_end_matches('Z')
        .parse()
        .map_err(|_| GraphError::unreadable_body(format!("'{text}' isn't a date and time")))?;
    let at = civil
        .to_zoned(tz.clone())
        .map_err(|_| GraphError::unreadable_body(format!("'{text}' isn't a date and time")))?
        .timestamp();
    let iana = tz
        .iana_name()
        .or_else(|| gmail::ics::windows_to_iana(zone_name))
        .unwrap_or(zone_name)
        .to_owned();
    Ok((at, tz, iana))
}

/// The calendar's form of a time: a date for an all-day event, else the local time with its
/// offset and IANA zone.
fn calendar_time(at: Timestamp, tz: &TimeZone, iana: &str, all_day: bool) -> EventDateTime {
    let zoned = at.to_zoned(tz.clone());
    if all_day {
        EventDateTime {
            date: Some(zoned.date().to_string()),
            ..EventDateTime::default()
        }
    } else {
        EventDateTime {
            date: None,
            date_time: Some(zoned.strftime("%Y-%m-%dT%H:%M:%S%:z").to_string()),
            time_zone: Some(iana.to_owned()),
        }
    }
}

/// `start`, `end` and `isAllDay`. An all-day event's times are midnights in one zone, and its
/// days are the calendar's dates; turning it into a timed event puts it at those midnights in
/// the calendar's zone.
fn times(target: &Target<'_>, event: &mut Event, body: &Map<String, Value>) -> Result<()> {
    let was_all_day = event.start.as_ref().is_some_and(|s| s.date.is_some());
    let all_day = match body.get("isAllDay") {
        Some(value) => flag(value, "isAllDay")?,
        None => was_all_day,
    };
    let current = |time: Option<&EventDateTime>| -> Option<(Timestamp, TimeZone, String)> {
        let time = time?;
        let iana = time
            .time_zone
            .clone()
            .unwrap_or_else(|| target.tz.iana_name().unwrap_or("UTC").to_owned());
        let tz = TimeZone::get(&iana).unwrap_or_else(|_| target.tz.clone());
        Some((time.timestamp(&tz)?, tz, iana))
    };
    let start = match body.get("start") {
        Some(value) => Some(instant(value, "start")?),
        None => current(event.start.as_ref()),
    };
    let end = match body.get("end") {
        Some(value) => Some(instant(value, "end")?),
        None => current(event.end.as_ref()),
    };
    let (Some(start), Some(end)) = (start, end) else {
        return Ok(());
    };
    if all_day {
        let midnight = |(at, tz, _): &(Timestamp, TimeZone, String)| {
            at.to_zoned(tz.clone()).time() == jiff::civil::Time::midnight()
        };
        let rewritten = body.contains_key("start") || body.contains_key("end") || !was_all_day;
        if rewritten && (!midnight(&start) || !midnight(&end)) {
            return Err(GraphError::invalid_request(
                "An all-day event's start and end must be at midnight.",
            ));
        }
    }
    event.start = Some(calendar_time(start.0, &start.1, &start.2, all_day));
    event.end = Some(calendar_time(end.0, &end.1, &end.2, all_day));
    Ok(())
}

/// `attendees`, keeping what the calendar knows of those already invited (their responses) and
/// its organizer's entry.
fn attendees_of(event: &Event, attendees: &Value) -> Result<Vec<EventAttendee>> {
    let list = attendees
        .as_array()
        .ok_or_else(|| GraphError::unreadable_body("attendees must be a collection"))?;
    let mut out: Vec<EventAttendee> = event
        .attendees
        .iter()
        .filter(|a| a.organizer == Some(true))
        .cloned()
        .collect();
    for attendee in list {
        let address = attendee["emailAddress"]["address"]
            .as_str()
            .filter(|a| a.contains('@'))
            .ok_or_else(|| {
                GraphError::new(
                    400,
                    "ErrorInvalidRecipients",
                    "At least one recipient isn't valid.",
                )
            })?;
        let kind = choice(
            attendee.get("type").unwrap_or(&Value::from("required")),
            &["required", "optional", "resource"],
        )?;
        let is_organizer = out.iter().any(|a| {
            a.organizer == Some(true)
                && a.email
                    .as_deref()
                    .is_some_and(|e| e.eq_ignore_ascii_case(address))
        }) || event
            .organizer
            .as_ref()
            .and_then(|o| o.email.as_deref())
            .is_some_and(|e| e.eq_ignore_ascii_case(address));
        if is_organizer {
            continue;
        }
        let known = event.attendees.iter().find(|a| {
            a.email
                .as_deref()
                .is_some_and(|e| e.eq_ignore_ascii_case(address))
        });
        out.push(EventAttendee {
            email: Some(address.to_owned()),
            display_name: attendee["emailAddress"]["name"]
                .as_str()
                .filter(|n| !n.is_empty())
                .map(str::to_owned)
                .or_else(|| known.and_then(|a| a.display_name.clone())),
            optional: (kind == "optional").then_some(true),
            resource: (kind == "resource").then_some(true),
            response_status: Some(
                known
                    .and_then(|a| a.response_status.clone())
                    .unwrap_or_else(|| "needsAction".into()),
            ),
            ..known.cloned().unwrap_or_default()
        });
    }
    Ok(out)
}

/// `recurrence`: a series' pattern as its RRULE, or `null` to make it a single event. A new or
/// changed pattern starts the series on the pattern's first date from the range's start, at
/// the event's time of day, as Outlook does.
fn series(target: &Target<'_>, event: &mut Event, rule: &Value) -> Result<()> {
    if event.recurring_event_id.is_some() {
        return Err(GraphError::invalid_request(
            "An occurrence of a series has no recurrence of its own.",
        ));
    }
    if rule.is_null() {
        event.recurrence.clear();
        return Ok(());
    }
    let all_day = event.start.as_ref().is_some_and(|s| s.date.is_some());
    let zone = |time: Option<&EventDateTime>| {
        time.and_then(|t| t.time_zone.as_deref())
            .and_then(|z| TimeZone::get(z).ok())
            .unwrap_or_else(|| target.tz.clone())
    };
    let tz = zone(event.start.as_ref());
    let line = recurrence::from_graph(rule, &tz, all_day).map_err(GraphError::invalid_request)?;
    event.recurrence = vec![line];
    let (Some(start), Some(end)) = (event.start.clone(), event.end.clone()) else {
        return Ok(());
    };
    let (Some(begin), Some(finish)) = (start.timestamp(&tz), end.timestamp(&tz)) else {
        return Ok(());
    };
    let begin = begin.to_zoned(tz.clone());
    let anchor = recurrence::range_start(rule).unwrap_or(begin.date());
    let Some(first) = recurrence::first_date(rule, anchor) else {
        return Ok(());
    };
    if first == begin.date() {
        return Ok(());
    }
    if all_day {
        let finish_date = finish.to_zoned(tz).date();
        let days = begin.date().until(finish_date).map_err(|_| {
            GraphError::invalid_request("The series' first occurrence is out of range.")
        })?;
        let end_date = first.checked_add(days).map_err(|_| {
            GraphError::invalid_request("The series' first occurrence is out of range.")
        })?;
        event.start = Some(EventDateTime {
            date: Some(first.to_string()),
            ..EventDateTime::default()
        });
        event.end = Some(EventDateTime {
            date: Some(end_date.to_string()),
            ..EventDateTime::default()
        });
        return Ok(());
    }
    let moved = first
        .to_datetime(begin.time())
        .to_zoned(tz.clone())
        .map_err(|_| {
            GraphError::invalid_request("The series' first occurrence is out of range.")
        })?;
    let length = finish.duration_since(begin.timestamp());
    let iana = start
        .time_zone
        .clone()
        .unwrap_or_else(|| tz.iana_name().unwrap_or("UTC").to_owned());
    let moved_end = moved.timestamp() + length;
    event.start = Some(calendar_time(moved.timestamp(), &tz, &iana, false));
    event.end = Some(calendar_time(moved_end, &tz, &iana, false));
    Ok(())
}

/// `isReminderOn` and `reminderMinutesBeforeStart`, as the calendar's popup reminder.
fn reminders(event: &mut Event, body: &Map<String, Value>) -> Result<()> {
    let on = body
        .get("isReminderOn")
        .map(|v| flag(v, "isReminderOn"))
        .transpose()?;
    let minutes = body
        .get("reminderMinutesBeforeStart")
        .map(|v| {
            v.as_u64()
                .and_then(|m| u32::try_from(m).ok())
                .ok_or_else(|| {
                    GraphError::unreadable_body("reminderMinutesBeforeStart must be a number")
                })
        })
        .transpose()?;
    match (on, minutes) {
        (Some(false), _) => {
            event.reminders = Some(EventReminders {
                use_default: Some(false),
                overrides: Vec::new(),
            });
        }
        (_, Some(minutes)) => {
            event.reminders = Some(EventReminders {
                use_default: Some(false),
                overrides: vec![EventReminder {
                    method: Some("popup".into()),
                    minutes: Some(minutes),
                }],
            });
        }
        (Some(true), None) => {
            let off = event
                .reminders
                .as_ref()
                .is_some_and(|r| r.use_default == Some(false) && r.overrides.is_empty());
            if off {
                event.reminders = Some(EventReminders {
                    use_default: Some(true),
                    overrides: Vec::new(),
                });
            }
        }
        (None, None) => {}
    }
    Ok(())
}

/// `isOnlineMeeting` and `onlineMeetingProvider`: an event made online gets its meeting (the
/// caller adds it once the event has its ID) and stays online, as Outlook ignores later
/// changes. A provider the calendar doesn't allow is refused.
fn online(
    target: &Target<'_>,
    event: &Event,
    stored: &mut Map<String, Value>,
    body: &Map<String, Value>,
) -> Result<()> {
    let already = stored.contains_key("onlineMeeting") || event.hangout_link.is_some();
    let asked = body
        .get("isOnlineMeeting")
        .map(|v| flag(v, "isOnlineMeeting"))
        .transpose()?
        .unwrap_or(false);
    let provider = body
        .get("onlineMeetingProvider")
        .map(|v| {
            choice(
                v,
                &[
                    "unknown",
                    "teamsForBusiness",
                    "skypeForBusiness",
                    "skypeForConsumer",
                ],
            )
        })
        .transpose()?
        .filter(|p| *p != "unknown");
    if already || !(asked || provider.is_some()) {
        return Ok(());
    }
    let provider = match provider {
        Some(provider) => provider,
        None => target.providers.first().copied().ok_or_else(|| {
            GraphError::invalid_request("This calendar doesn't allow online meetings.")
        })?,
    };
    if !target.providers.contains(&provider) {
        return Err(GraphError::invalid_request(format!(
            "This calendar doesn't allow online meetings by {provider}."
        )));
    }
    stored.insert("isOnlineMeeting".into(), true.into());
    stored.insert("onlineMeetingProvider".into(), provider.into());
    Ok(())
}

#[cfg(test)]
mod tests {
    use gmail::calendar::EventPerson;
    use serde_json::json;

    use super::*;

    #[test]
    fn all_day_series_preserves_civil_day_span_across_dst() {
        let tz = TimeZone::get("America/New_York").unwrap();
        let target = Target {
            tz: &tz,
            providers: &[],
        };

        // Spring forward: 2026-03-08 is a 23-hour day in New York; shifting first occurrence to
        // Monday 2026-03-09 (a 24-hour day) must keep the 1-day span (end = 2026-03-10).
        let mut spring = Event {
            start: Some(EventDateTime {
                date: Some("2026-03-08".into()),
                ..EventDateTime::default()
            }),
            end: Some(EventDateTime {
                date: Some("2026-03-09".into()),
                ..EventDateTime::default()
            }),
            ..Event::default()
        };
        let rule = json!({
            "pattern": { "type": "weekly", "interval": 1, "daysOfWeek": ["monday"] },
            "range": { "type": "noEnd", "startDate": "2026-03-08" },
        });
        series(&target, &mut spring, &rule).unwrap();
        assert_eq!(spring.start.unwrap().date.as_deref(), Some("2026-03-09"));
        assert_eq!(spring.end.unwrap().date.as_deref(), Some("2026-03-10"));

        // Fall back: 2026-10-31 is 24 hours, 2026-11-01 is 25 hours; shifting onto Sunday
        // 2026-11-01 must keep the 1-day span (end = 2026-11-02).
        let mut fall = Event {
            start: Some(EventDateTime {
                date: Some("2026-10-31".into()),
                ..EventDateTime::default()
            }),
            end: Some(EventDateTime {
                date: Some("2026-11-01".into()),
                ..EventDateTime::default()
            }),
            ..Event::default()
        };
        let fall_rule = json!({
            "pattern": { "type": "weekly", "interval": 1, "daysOfWeek": ["sunday"] },
            "range": { "type": "noEnd", "startDate": "2026-10-31" },
        });
        series(&target, &mut fall, &fall_rule).unwrap();
        assert_eq!(fall.start.unwrap().date.as_deref(), Some("2026-11-01"));
        assert_eq!(fall.end.unwrap().date.as_deref(), Some("2026-11-02"));
    }

    #[test]
    fn attendees_of_skips_organizer_without_duplicating() {
        let event = Event {
            organizer: Some(EventPerson {
                email: Some("maya@brightmoor.co".into()),
                is_self: Some(true),
                ..EventPerson::default()
            }),
            attendees: vec![
                EventAttendee {
                    email: Some("maya@brightmoor.co".into()),
                    organizer: Some(true),
                    response_status: Some("accepted".into()),
                    ..EventAttendee::default()
                },
                EventAttendee {
                    email: Some("daniel@brightmoor.co".into()),
                    response_status: Some("accepted".into()),
                    ..EventAttendee::default()
                },
            ],
            ..Event::default()
        };
        let body = json!([
            { "emailAddress": { "address": "MAYA@brightmoor.co", "name": "Maya" }, "type": "required" },
            { "emailAddress": { "address": "daniel@brightmoor.co", "name": "Daniel" }, "type": "required" },
            { "emailAddress": { "address": "lena@brightmoor.co", "name": "Lena" }, "type": "optional" },
        ]);
        let updated = attendees_of(&event, &body).unwrap();
        let emails: Vec<&str> = updated.iter().filter_map(|a| a.email.as_deref()).collect();
        assert_eq!(
            emails,
            [
                "maya@brightmoor.co",
                "daniel@brightmoor.co",
                "lena@brightmoor.co"
            ]
        );
        assert_eq!(updated[0].organizer, Some(true));
        assert_eq!(updated[1].response_status.as_deref(), Some("accepted"));
        assert_eq!(updated[2].optional, Some(true));
    }

    #[test]
    fn instant_resolves_full_cldr_windows_zones() {
        let (_, _, iana) = instant(
            &json!({ "dateTime": "2026-11-18T12:00:00", "timeZone": "Chatham Islands Standard Time" }),
            "start",
        )
        .unwrap();
        assert_eq!(iana, "Pacific/Chatham");
        assert!(
            instant(
                &json!({ "dateTime": "2026-11-18T12:00:00", "timeZone": "Not A Real Zone" }),
                "start"
            )
            .is_err()
        );
    }
}

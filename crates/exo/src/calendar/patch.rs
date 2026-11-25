use std::sync::atomic::{AtomicU64, Ordering};

use gmail::calendar::{Event as ApiEvent, EventAttendee, EventDateTime, EventPerson};
use jiff::{SignedDuration, ToSpan, Zoned};

use super::{
    event::Event,
    form::{Field, Guest, Meeting, Values},
};

/// The 1:1 titles a new event without guests is asked about.
pub(super) const ONE_ON_ONE: [&str; 6] = [
    r"(?i)\b1\s*[:x-]\s*1\b",
    r"(?i)\b1\s+on\s+1\b",
    r"(?i)\b[a-z0-9][a-z0-9.'&-]*\s*<>\s*[a-z0-9][a-z0-9.'&-]*\b",
    r"(?i)\b[a-z0-9][a-z0-9.'&-]*\s+\+\s+[a-z0-9][a-z0-9.'&-]*\b",
    r"(?i)^[a-z0-9][a-z0-9.'&-]*\s+and\s+[a-z0-9][a-z0-9.'&-]*$",
    r"(?i)^[a-z0-9][a-z0-9.'&-]*\s*:\s*[a-z0-9][a-z0-9.'&-]*(?:\s+[a-z0-9][a-z0-9.'&-]*)?$",
];

/// Counts ids, in case the system's randomness is out.
static NEXT_ID: AtomicU64 = AtomicU64::new(0);

/// An id for a new event, chosen here because Calendar takes a client's own. It's 32 hex digits,
/// which are base32hex.
pub(super) fn new_id() -> String {
    let mut bytes = [0u8; 16];
    if getrandom::fill(&mut bytes).is_err() {
        let fallback = jiff::Timestamp::now().as_nanosecond() as u128
            ^ u128::from(NEXT_ID.fetch_add(1, Ordering::Relaxed));
        bytes = fallback.to_le_bytes();
    }
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}

/// The form's values for `event`, with its notes as HTML for About's editor and a plain-text
/// description's lines kept as lines.
pub(super) fn values_of(event: &Event, source: &ApiEvent, me: &str) -> Values {
    let description = source.description.clone().unwrap_or_default();
    let about = if looks_like_html(&description) {
        description
    } else {
        description
            .lines()
            .map(|line| format!("<div>{}</div>", exo_mail::escape_html_text(line)))
            .collect()
    };
    let meet = super::event::video_uri_of(source);
    Values {
        title: source.summary.clone().unwrap_or_default(),
        when: Some((event.start.clone(), event.end.clone())),
        all_day: source
            .start
            .as_ref()
            .is_some_and(|start| start.date.is_some()),
        guests: source
            .attendees
            .iter()
            .filter(|attendee| attendee.resource != Some(true))
            .filter_map(|attendee| {
                let email = attendee.email.clone()?;
                Some(Guest {
                    me: attendee.is_self == Some(true) || email.eq_ignore_ascii_case(me),
                    name: attendee.display_name.clone(),
                    optional: attendee.optional == Some(true),
                    response: attendee.response_status.clone(),
                    organizer: attendee.organizer == Some(true),
                    email,
                })
            })
            .collect(),
        location: source.location.clone().unwrap_or_default(),
        about,
        meeting: meet.map_or(Meeting::None, Meeting::Existing),
        calendar: event.calendar_id.clone(),
        repeat: None,
    }
}

/// Whether `text` appears to contain HTML markup rather than plain text (which may still use `<`
/// in inequalities like `"Budget < $1000"` or angle-bracketed emails/URLs).
fn looks_like_html(text: &str) -> bool {
    const TAGS: &[&str] = &[
        "a",
        "b",
        "blockquote",
        "br",
        "code",
        "del",
        "div",
        "em",
        "font",
        "h1",
        "h2",
        "h3",
        "h4",
        "h5",
        "h6",
        "hr",
        "i",
        "img",
        "li",
        "ol",
        "p",
        "pre",
        "s",
        "span",
        "strike",
        "strong",
        "sub",
        "sup",
        "table",
        "tbody",
        "td",
        "th",
        "thead",
        "tr",
        "u",
        "ul",
    ];
    let bytes = text.as_bytes();
    let mut i = 0;
    while let Some(rel) = text[i..].find('<') {
        let start = i + rel + 1;
        let name_start = if bytes.get(start) == Some(&b'/') {
            start + 1
        } else {
            start
        };
        let mut end = name_start;
        while bytes.get(end).is_some_and(u8::is_ascii_alphanumeric) {
            end += 1;
        }
        if end > name_start {
            let name = &text[name_start..end];
            let boundary = bytes
                .get(end)
                .is_some_and(|b| matches!(b, b'>' | b'/' | b' ' | b'\t' | b'\r' | b'\n'));
            if boundary
                && TAGS.iter().any(|tag| tag.eq_ignore_ascii_case(name))
                && text[end..].contains('>')
            {
                return true;
            }
        }
        i = start;
    }
    false
}

/// A start or end as Calendar takes it: a date for an all-day event, else the time with its
/// offset and zone.
fn date_time(at: &Zoned, all_day: bool) -> EventDateTime {
    if all_day {
        return EventDateTime {
            date: Some(at.date().to_string()),
            ..EventDateTime::default()
        };
    }
    EventDateTime {
        date_time: Some(at.strftime("%Y-%m-%dT%H:%M:%S%:z").to_string()),
        time_zone: at.time_zone().iana_name().map(str::to_owned),
        ..EventDateTime::default()
    }
}

/// The guests as Calendar's attendees. When there's anyone else (or a booked room), preserves
/// `existing` room/resource attendees, `organizer` flags, `mine.email`, and untouched attendee
/// metadata (`id`, `comment`, `additional_guests`).
fn attendees(values: &Values, me: &str, existing: &[EventAttendee]) -> Vec<EventAttendee> {
    let others: Vec<&Guest> = values
        .guests
        .iter()
        .filter(|guest| !guest.me && !guest.email.eq_ignore_ascii_case(me))
        .collect();
    let resources: Vec<EventAttendee> = existing
        .iter()
        .filter(|attendee| attendee.resource == Some(true))
        .cloned()
        .collect();
    if others.is_empty() && resources.is_empty() {
        return Vec::new();
    }
    let mine = values.guests.iter().find(|guest| guest.me).or_else(|| {
        values
            .guests
            .iter()
            .find(|guest| guest.email.eq_ignore_ascii_case(me))
    });
    let mut list = Vec::new();
    if !others.is_empty() || mine.is_some() {
        let my_email = mine.map_or(me, |guest| guest.email.as_str());
        let prior_mine = existing.iter().find(|attendee| {
            attendee.resource != Some(true)
                && (attendee.is_self == Some(true)
                    || attendee
                        .email
                        .as_deref()
                        .is_some_and(|email| email.eq_ignore_ascii_case(my_email)))
        });
        list.push(EventAttendee {
            id: prior_mine.and_then(|attendee| attendee.id.clone()),
            email: Some(my_email.to_owned()),
            display_name: mine
                .and_then(|guest| guest.name.clone())
                .or_else(|| prior_mine.and_then(|attendee| attendee.display_name.clone())),
            organizer: match mine {
                Some(guest) if guest.organizer => Some(true),
                Some(_) => prior_mine.and_then(|attendee| attendee.organizer),
                None => Some(true),
            },
            is_self: prior_mine.and_then(|attendee| attendee.is_self),
            optional: mine.is_some_and(|guest| guest.optional).then_some(true),
            response_status: Some(
                mine.and_then(|guest| guest.response.clone())
                    .unwrap_or_else(|| "accepted".to_owned()),
            ),
            comment: prior_mine.and_then(|attendee| attendee.comment.clone()),
            additional_guests: prior_mine.and_then(|attendee| attendee.additional_guests),
            ..EventAttendee::default()
        });
    }
    list.extend(others.into_iter().map(|guest| {
        let prior = existing.iter().find(|attendee| {
            attendee.resource != Some(true)
                && attendee
                    .email
                    .as_deref()
                    .is_some_and(|email| email.eq_ignore_ascii_case(&guest.email))
        });
        EventAttendee {
            id: prior.and_then(|attendee| attendee.id.clone()),
            email: Some(guest.email.clone()),
            display_name: guest.name.clone(),
            organizer: guest
                .organizer
                .then_some(true)
                .or_else(|| prior.and_then(|attendee| attendee.organizer)),
            optional: guest.optional.then_some(true),
            response_status: Some(
                guest
                    .response
                    .clone()
                    .unwrap_or_else(|| "needsAction".to_owned()),
            ),
            comment: prior.and_then(|attendee| attendee.comment.clone()),
            additional_guests: prior.and_then(|attendee| attendee.additional_guests),
            ..EventAttendee::default()
        }
    }));
    list.extend(resources);
    list
}

/// A Google Meet link asked for (`conferenceData.createRequest`).
fn meet_request() -> serde_json::Value {
    serde_json::json!({ "createRequest": { "requestId": new_id(), "conferenceSolutionKey": { "type": "hangoutsMeet" } } })
}

/// A new event from the form, its id chosen here so it shows before Calendar has it.
pub(super) fn new_event(values: &Values, start: &Zoned, end: &Zoned, me: &str) -> ApiEvent {
    let me_person = EventPerson {
        email: Some(me.to_owned()),
        is_self: Some(true),
        ..EventPerson::default()
    };
    // A series starts on its rule's first day, with its end moved by as many days.
    let (start, end) = match values.repeat.as_ref().and_then(|rule| rule.first(start)) {
        Some(first) => {
            let days = start
                .date()
                .until(first.date())
                .map_or(0, |span| span.get_days());
            (
                first,
                end.checked_add(i64::from(days).days())
                    .unwrap_or_else(|_| end.clone()),
            )
        }
        None => (start.clone(), end.clone()),
    };
    let (start, end) = (&start, &end);
    ApiEvent {
        recurrence: values
            .repeat
            .iter()
            .map(|rule| rule.rule(start, values.all_day))
            .collect(),
        id: Some(new_id()),
        summary: Some(values.title.trim().to_owned()),
        description: (!values.about.trim().is_empty()).then(|| values.about.clone()),
        location: (!values.location.trim().is_empty()).then(|| values.location.trim().to_owned()),
        start: Some(date_time(start, values.all_day)),
        end: Some(date_time(end, values.all_day)),
        attendees: attendees(values, me, &[]),
        conference_data: (values.meeting == Meeting::NewMeet).then(meet_request),
        organizer: Some(me_person.clone()),
        creator: Some(me_person),
        ..ApiEvent::default()
    }
}

/// The changed fields as a patch. When `original` is given, untouched attendee metadata, room
/// resources, and organizer flags are preserved across guest edits.
pub(super) fn patch_of(
    values: &Values,
    dirty: &[Field],
    me: &str,
    original: Option<&ApiEvent>,
) -> ApiEvent {
    let existing: &[EventAttendee] = original.map_or(&[], |event| event.attendees.as_slice());
    let mut patch = ApiEvent::default();
    for field in dirty {
        match field {
            Field::Title => patch.summary = Some(values.title.trim().to_owned()),
            Field::When => {
                if let Some((start, end)) = &values.when {
                    patch.start = Some(date_time(start, values.all_day));
                    patch.end = Some(date_time(end, values.all_day));
                }
            }
            Field::Location => patch.location = Some(values.location.trim().to_owned()),
            Field::About => patch.description = Some(values.about.clone()),
            Field::Guests => {
                patch.attendees = attendees(values, me, existing);
                patch.clear_attendees = patch.attendees.is_empty();
            }
            Field::Meeting => {
                patch.conference_data = Some(match &values.meeting {
                    Meeting::NewMeet => meet_request(),
                    Meeting::None | Meeting::Existing(_) => serde_json::Value::Null,
                });
            }
            Field::Calendar | Field::Repeat => {}
        }
    }
    patch
}

/// `event` with what `patch` sets, as it'll look once Calendar has it.
pub(super) fn merged(event: &ApiEvent, patch: &ApiEvent) -> ApiEvent {
    let mut event = event.clone();
    if let Some(summary) = &patch.summary {
        event.summary = Some(summary.clone());
    }
    if let Some(start) = &patch.start {
        event.start = Some(start.clone());
    }
    if let Some(end) = &patch.end {
        event.end = Some(end.clone());
    }
    if let Some(location) = &patch.location {
        event.location = Some(location.clone());
    }
    if let Some(description) = &patch.description {
        event.description = Some(description.clone());
    }
    if !patch.attendees.is_empty() || patch.clear_attendees {
        event.attendees = patch.attendees.clone();
    }
    if let Some(conference_data) = &patch.conference_data {
        if conference_data.is_null() {
            event.conference_data = None;
            event.hangout_link = None;
        } else {
            event.conference_data = Some(conference_data.clone());
        }
    }
    event
}

/// How far an instance's civil wall-clock start and end moved, for its series (`All events` and
/// `Following`).
pub(super) fn shift_of(
    instance: &ApiEvent,
    values: &Values,
) -> Option<(SignedDuration, SignedDuration)> {
    let (start, end) = values.when.as_ref()?;
    let tz = start.time_zone();
    let civil = |time: &EventDateTime| -> Option<jiff::civil::DateTime> {
        if let Some(date) = &time.date {
            return Some(date.parse::<jiff::civil::Date>().ok()?.at(0, 0, 0, 0));
        }
        Some(time.timestamp(tz)?.to_zoned(tz.clone()).datetime())
    };
    let from = civil(instance.start.as_ref()?)?;
    let to = civil(instance.end.as_ref()?)?;
    Some((
        start.datetime().duration_since(from),
        end.datetime().duration_since(to),
    ))
}

#[cfg(test)]
mod tests {
    use exo_store::CalendarChange;
    use jiff::tz::TimeZone;

    use super::*;

    fn at(text: &str) -> Zoned {
        text.parse::<jiff::civil::DateTime>()
            .expect("time")
            .to_zoned(TimeZone::get("America/New_York").expect("zone"))
            .expect("exists")
    }

    fn values() -> Values {
        Values {
            title: "Lunch".into(),
            when: Some((at("2026-11-20T12:00"), at("2026-11-20T13:00"))),
            all_day: false,
            guests: Vec::new(),
            location: String::new(),
            about: String::new(),
            meeting: Meeting::None,
            calendar: "maya@brightmoor.co".into(),
            repeat: None,
        }
    }

    #[test]
    fn a_new_event_carries_its_times_and_guests() {
        let mut values = values();
        let (start, end) = values.when.clone().expect("a time");
        let event = new_event(&values, &start, &end, "maya@brightmoor.co");
        assert_eq!(
            event.start.and_then(|start| start.date_time).as_deref(),
            Some("2026-11-20T12:00:00-05:00")
        );
        assert!(event.attendees.is_empty(), "alone, no attendee list");
        assert!(event.id.is_some_and(|id| {
            id.bytes()
                .all(|byte| matches!(byte, b'0'..=b'9' | b'a'..=b'v'))
        }));
        values.guests.push(Guest {
            email: "lena@brightmoor.co".into(),
            name: None,
            optional: true,
            response: None,
            me: false,
            organizer: false,
        });
        values.all_day = true;
        let event = new_event(&values, &start, &end, "maya@brightmoor.co");
        let emails: Vec<_> = event
            .attendees
            .iter()
            .map(|attendee| {
                (
                    attendee.email.clone().unwrap_or_default(),
                    attendee.response_status.clone().unwrap_or_default(),
                )
            })
            .collect();
        assert_eq!(
            emails,
            [
                ("maya@brightmoor.co".to_owned(), "accepted".to_owned()),
                ("lena@brightmoor.co".to_owned(), "needsAction".to_owned())
            ]
        );
        assert_eq!(
            event.start.and_then(|start| start.date).as_deref(),
            Some("2026-11-20")
        );
    }

    #[test]
    fn a_patch_has_only_what_changed() {
        let values = values();
        let patch = patch_of(&values, &[Field::Title], "maya@brightmoor.co", None);
        assert_eq!(
            patch,
            ApiEvent {
                summary: Some("Lunch".into()),
                ..ApiEvent::default()
            }
        );
        let removed = patch_of(
            &Values {
                meeting: Meeting::None,
                ..values
            },
            &[Field::Meeting],
            "maya@brightmoor.co",
            None,
        );
        assert_eq!(
            serde_json::to_value(&removed).expect("serializes"),
            serde_json::json!({ "conferenceData": null }),
            "a removed link is cleared"
        );
    }

    #[test]
    fn one_on_ones_are_recognized() {
        let matches = |title: &str| {
            ONE_ON_ONE
                .iter()
                .any(|pattern| regex::Regex::new(pattern).expect("pattern").is_match(title))
        };
        assert!(
            matches("Priya 1:1")
                && matches("Maya <> Lena")
                && matches("Maya and Lena")
                && matches("Sync: Lena")
        );
        assert!(!matches("Design crit"));
    }

    #[test]
    fn removing_all_guests_clears_attendees_locally_and_marks_patch_for_clear() {
        let mut vals = values();
        vals.guests.clear();
        let patch = patch_of(&vals, &[Field::Guests], "maya@brightmoor.co", None);
        assert!(patch.attendees.is_empty() && patch.clear_attendees);

        let before = ApiEvent {
            id: Some("e1".into()),
            attendees: vec![EventAttendee {
                email: Some("lena@brightmoor.co".into()),
                ..EventAttendee::default()
            }],
            ..ApiEvent::default()
        };
        let after = merged(&before, &patch);
        assert!(after.attendees.is_empty(), "cleared locally in merged()");
    }

    #[test]
    fn patch_preserves_rooms_organizer_mine_email_and_attendee_metadata() {
        let original = ApiEvent {
            id: Some("e1".into()),
            attendees: vec![
                EventAttendee {
                    id: Some("p-self".into()),
                    email: Some("alias@brightmoor.co".into()),
                    display_name: Some("Maya Alias".into()),
                    is_self: Some(true),
                    organizer: Some(false),
                    response_status: Some("accepted".into()),
                    comment: Some("Joining remotely".into()),
                    additional_guests: Some(1),
                    ..EventAttendee::default()
                },
                EventAttendee {
                    id: Some("p-org".into()),
                    email: Some("boss@brightmoor.co".into()),
                    display_name: Some("Boss".into()),
                    organizer: Some(true),
                    response_status: Some("accepted".into()),
                    comment: Some("Bringing deck".into()),
                    additional_guests: Some(2),
                    ..EventAttendee::default()
                },
                EventAttendee {
                    id: Some("p-room".into()),
                    email: Some("oak-room@resource.calendar.google.com".into()),
                    display_name: Some("Oak Room".into()),
                    resource: Some(true),
                    response_status: Some("accepted".into()),
                    ..EventAttendee::default()
                },
            ],
            ..ApiEvent::default()
        };
        let event = Event::draft(
            "e1".into(),
            "Review".into(),
            at("2026-11-20T12:00"),
            at("2026-11-20T13:00"),
            false,
            exo_ui::CalendarColor::BLUE,
        );
        let mut vals = values_of(&event, &original, "maya@brightmoor.co");
        vals.guests.push(Guest {
            email: "newguest@brightmoor.co".into(),
            name: Some("New Guest".into()),
            optional: true,
            response: None,
            me: false,
            organizer: false,
        });
        let patch = patch_of(
            &vals,
            &[Field::Guests],
            "maya@brightmoor.co",
            Some(&original),
        );
        assert_eq!(patch.attendees.len(), 4);

        let me_entry = &patch.attendees[0];
        assert_eq!(me_entry.email.as_deref(), Some("alias@brightmoor.co"));
        assert_eq!(me_entry.id.as_deref(), Some("p-self"));
        assert_eq!(me_entry.is_self, Some(true));
        assert_eq!(me_entry.organizer, Some(false));
        assert_eq!(me_entry.comment.as_deref(), Some("Joining remotely"));
        assert_eq!(me_entry.additional_guests, Some(1));

        let org_entry = &patch.attendees[1];
        assert_eq!(org_entry.email.as_deref(), Some("boss@brightmoor.co"));
        assert_eq!(org_entry.id.as_deref(), Some("p-org"));
        assert_eq!(org_entry.organizer, Some(true));
        assert_eq!(org_entry.comment.as_deref(), Some("Bringing deck"));
        assert_eq!(org_entry.additional_guests, Some(2));

        let added = &patch.attendees[2];
        assert_eq!(added.email.as_deref(), Some("newguest@brightmoor.co"));
        assert_eq!(added.optional, Some(true));
        assert_eq!(added.response_status.as_deref(), Some("needsAction"));

        let room = &patch.attendees[3];
        assert_eq!(
            room.email.as_deref(),
            Some("oak-room@resource.calendar.google.com")
        );
        assert_eq!(room.resource, Some(true));
        assert_eq!(room.id.as_deref(), Some("p-room"));

        // Removing all human guests still preserves booked rooms (and doesn't clear attendees).
        vals.guests.retain(|guest| guest.me);
        let room_only_patch = patch_of(
            &vals,
            &[Field::Guests],
            "maya@brightmoor.co",
            Some(&original),
        );
        assert!(!room_only_patch.clear_attendees);
        assert!(
            room_only_patch
                .attendees
                .iter()
                .any(|attendee| attendee.resource == Some(true)),
            "room booking survives removing human guests"
        );
    }

    #[test]
    fn plain_text_descriptions_with_angle_brackets_keep_line_breaks() {
        let event = Event::draft(
            "e1".into(),
            "Budget".into(),
            at("2026-11-20T12:00"),
            at("2026-11-20T13:00"),
            false,
            exo_ui::CalendarColor::BLUE,
        );
        let plain = ApiEvent {
            description: Some("Budget < $1000\nContact <alice@example.com> & team".into()),
            ..ApiEvent::default()
        };
        let vals = values_of(&event, &plain, "maya@brightmoor.co");
        assert_eq!(
            vals.about,
            "<div>Budget &lt; $1000</div><div>Contact &lt;alice@example.com&gt; &amp; team</div>"
        );

        let html = ApiEvent {
            description: Some("<p>Agenda</p><ul><li>Item 1</li></ul>".into()),
            ..ApiEvent::default()
        };
        assert_eq!(
            values_of(&event, &html, "maya@brightmoor.co").about,
            "<p>Agenda</p><ul><li>Item 1</li></ul>"
        );
        assert!(looks_like_html("Line 1<br>Line 2"));
        assert!(looks_like_html(
            "See <A href=\"https://example.com\">doc</A>"
        ));
        assert!(!looks_like_html("Budget < $1000"));
        assert!(!looks_like_html("Maya <> Lena"));
        assert!(!looks_like_html("<https://meet.google.com/abc-defg-hij>"));
    }

    #[test]
    fn merged_applies_and_clears_conference_data_and_hangout_link() {
        let before = ApiEvent {
            id: Some("e1".into()),
            hangout_link: Some("https://meet.google.com/abc-defg-hij".into()),
            conference_data: Some(serde_json::json!({
                "entryPoints": [{
                    "entryPointType": "video",
                    "uri": "https://meet.google.com/abc-defg-hij"
                }]
            })),
            ..ApiEvent::default()
        };
        let remove_patch = patch_of(
            &Values {
                meeting: Meeting::None,
                ..values()
            },
            &[Field::Meeting],
            "maya@brightmoor.co",
            Some(&before),
        );
        let cleared = merged(&before, &remove_patch);
        assert_eq!(cleared.conference_data, None);
        assert_eq!(cleared.hangout_link, None);
        assert_eq!(super::super::event::video_uri_of(&cleared), None);

        let add_patch = patch_of(
            &Values {
                meeting: Meeting::NewMeet,
                ..values()
            },
            &[Field::Meeting],
            "maya@brightmoor.co",
            Some(&cleared),
        );
        let added = merged(&cleared, &add_patch);
        assert!(
            added
                .conference_data
                .as_ref()
                .is_some_and(|cd| cd.get("createRequest").is_some())
        );
    }

    #[test]
    fn shift_of_uses_wall_clock_across_dst_and_supports_all_day_and_toggles() {
        // 25h fall-back day (2026-11-01 in America/New_York): moving 00:30 EDT -> 09:00 EST is an
        // 8h30m wall-clock move (not 9h30m elapsed), so an EDT instance lands at 09:00 EDT.
        let instance = ApiEvent {
            start: Some(EventDateTime {
                date_time: Some("2026-11-01T00:30:00-04:00".into()),
                time_zone: Some("America/New_York".into()),
                ..EventDateTime::default()
            }),
            end: Some(EventDateTime {
                date_time: Some("2026-11-01T01:00:00-04:00".into()),
                time_zone: Some("America/New_York".into()),
                ..EventDateTime::default()
            }),
            ..ApiEvent::default()
        };
        let mut vals = values();
        vals.when = Some((at("2026-11-01T09:00"), at("2026-11-01T09:30")));
        let (ds, de) = shift_of(&instance, &vals).expect("shift");
        assert_eq!(ds, SignedDuration::from_mins(8 * 60 + 30));
        assert_eq!(de, SignedDuration::from_mins(8 * 60 + 30));

        let oct_instance = EventDateTime {
            date_time: Some("2026-10-25T00:30:00-04:00".into()),
            time_zone: Some("America/New_York".into()),
            ..EventDateTime::default()
        };
        assert_eq!(
            CalendarChange::shift_event_time(Some(&oct_instance), ds)
                .and_then(|t| t.date_time)
                .as_deref(),
            Some("2026-10-25T09:00:00-04:00")
        );

        // All-day instance toggling to timed 09:00–10:00 on Scope::All.
        let all_day_inst = ApiEvent {
            start: Some(EventDateTime {
                date: Some("2026-11-17".into()),
                ..EventDateTime::default()
            }),
            end: Some(EventDateTime {
                date: Some("2026-11-18".into()),
                ..EventDateTime::default()
            }),
            ..ApiEvent::default()
        };
        vals.all_day = false;
        vals.when = Some((at("2026-11-17T09:00"), at("2026-11-17T10:00")));
        let patch = patch_of(&vals, &[Field::When], "maya@brightmoor.co", None);
        let (ds, de) = shift_of(&all_day_inst, &vals).expect("all-day to timed shift");
        let earlier = EventDateTime {
            date: Some("2026-10-27".into()),
            ..EventDateTime::default()
        };
        assert_eq!(
            CalendarChange::retime_event_time(Some(&earlier), ds, patch.start.as_ref())
                .and_then(|t| t.date_time)
                .as_deref(),
            Some("2026-10-27T09:00:00-04:00")
        );
        let earlier_end = EventDateTime {
            date: Some("2026-10-28".into()),
            ..EventDateTime::default()
        };
        assert_eq!(
            CalendarChange::retime_event_time(Some(&earlier_end), de, patch.end.as_ref())
                .and_then(|t| t.date_time)
                .as_deref(),
            Some("2026-10-27T10:00:00-04:00")
        );
    }
}

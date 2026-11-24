//! The iTIP (RFC 5546) part of meeting mail, read as far as Outlook's `eventMessage` needs: the
//! method, and the first `VEVENT`'s times, place, recurrence, organizer and attendees.

use jiff::Timestamp;
use jiff::civil::{Date, DateTime};
use jiff::tz::TimeZone;

/// A meeting message's calendar object.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Itip {
    /// `REQUEST`, `REPLY`, `CANCEL`, `COUNTER`…
    pub method: String,
    pub uid: String,
    pub summary: String,
    pub location: String,
    pub start: Option<When>,
    pub end: Option<When>,
    /// `RRULE`, `EXDATE` and `RDATE` lines, as the calendar keeps them.
    pub recurrence: Vec<String>,
    /// Set on a message about one occurrence.
    pub recurrence_id: Option<When>,
    pub sequence: u32,
    pub organizer: Option<String>,
    /// Attendees' addresses and participation (`ACCEPTED`, `NEEDS-ACTION`…).
    pub attendees: Vec<(String, String, bool)>,
    /// Whether the organizer refuses new time proposals.
    pub no_counter: bool,
}

/// A `DTSTART`-style value: an instant, or a whole day.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum When {
    At(Timestamp),
    Day(Date),
}

impl When {
    /// The instant, a day starting at midnight in `tz`.
    pub fn timestamp(self, tz: &TimeZone) -> Option<Timestamp> {
        match self {
            Self::At(at) => Some(at),
            Self::Day(day) => day.to_zoned(tz.clone()).ok().map(|z| z.timestamp()),
        }
    }
}

impl Itip {
    /// The calendar object in `text`, if it has a `VEVENT`.
    pub fn parse(text: &str) -> Option<Self> {
        let mut itip = Self::default();
        let mut in_event = false;
        let mut nested_depth: usize = 0;
        let mut seen_event = false;
        for line in gmail::ics::unfold(text) {
            let Some((name, params, value)) = gmail::ics::content_line(&line) else {
                continue;
            };
            let param = |key: &str| params.get(key).map(String::as_str);
            if in_event && nested_depth > 0 {
                match name.as_str() {
                    "BEGIN" => nested_depth += 1,
                    "END" => nested_depth = nested_depth.saturating_sub(1),
                    _ => {}
                }
                continue;
            }
            match name.as_str() {
                "METHOD" if !in_event => itip.method = value.trim().to_ascii_uppercase(),
                "BEGIN" if !in_event && value.eq_ignore_ascii_case("VEVENT") && !seen_event => {
                    in_event = true;
                    nested_depth = 0;
                }
                "BEGIN" if in_event => nested_depth = 1,
                "END" if in_event && value.eq_ignore_ascii_case("VEVENT") => {
                    in_event = false;
                    seen_event = true;
                }
                _ if !in_event => {}
                "UID" => itip.uid = value.to_owned(),
                "SUMMARY" => itip.summary = gmail::ics::unescape(value),
                "LOCATION" => itip.location = gmail::ics::unescape(value),
                "DTSTART" => itip.start = when(value, param("TZID"), param("VALUE")),
                "DTEND" => itip.end = when(value, param("TZID"), param("VALUE")),
                "RECURRENCE-ID" => itip.recurrence_id = when(value, param("TZID"), param("VALUE")),
                "RRULE" | "EXDATE" | "RDATE" => itip.recurrence.push(line.clone()),
                "SEQUENCE" => itip.sequence = value.trim().parse().unwrap_or_default(),
                "ORGANIZER" => itip.organizer = Some(gmail::ics::strip_mailto(value).to_owned()),
                "ATTENDEE" => itip.attendees.push((
                    gmail::ics::strip_mailto(value).to_owned(),
                    param("PARTSTAT")
                        .unwrap_or("NEEDS-ACTION")
                        .to_ascii_uppercase(),
                    param("RSVP").is_some_and(|rsvp| rsvp.eq_ignore_ascii_case("TRUE")),
                )),
                "X-MICROSOFT-DISALLOW-COUNTER" => {
                    itip.no_counter = value.eq_ignore_ascii_case("TRUE")
                }
                _ => {}
            }
        }
        seen_event.then_some(itip)
    }

    /// The participation `address` gives, when the object lists them.
    pub fn partstat(&self, address: &str) -> Option<&str> {
        self.attendees
            .iter()
            .find(|(email, ..)| email.eq_ignore_ascii_case(address))
            .map(|(_, partstat, _)| partstat.as_str())
    }
}

/// `20261118T150000Z`, `20261118T100000` in `tzid`, or `20261118` (a day).
fn when(value: &str, tzid: Option<&str>, kind: Option<&str>) -> Option<When> {
    let value = value.trim();
    if kind.is_some_and(|k| k.eq_ignore_ascii_case("DATE")) || value.len() == 8 {
        let date = Date::strptime("%Y%m%d", value).ok()?;
        return Some(When::Day(date));
    }
    let civil = DateTime::strptime("%Y%m%dT%H%M%S", value.trim_end_matches('Z')).ok()?;
    let tz = if value.ends_with('Z') {
        TimeZone::UTC
    } else {
        tzid.and_then(gmail::ics::resolve_timezone)
            .unwrap_or(TimeZone::UTC)
    };
    civil
        .to_zoned(tz)
        .ok()
        .map(|zoned| When::At(zoned.timestamp()))
}

#[cfg(test)]
mod tests {
    use super::*;

    const REPLY: &str = "BEGIN:VCALENDAR\r\nMETHOD:REPLY\r\nBEGIN:VTIMEZONE\r\nTZID:Eastern Standard Time\r\nEND:VTIMEZONE\r\nBEGIN:VEVENT\r\nUID:abc@google.com\r\nDTSTART;TZID=Eastern Standard Time:20261118T100000\r\nDTEND;TZID=America/New_York:20261118T103000\r\nSEQUENCE:2\r\nSUMMARY:Atlas sync\\, weekly\r\nLOCATION:Room 4\r\nORGANIZER;CN=Maya:mailto:maya@brightmoor.co\r\nATTENDEE;PARTSTAT=ACCEPTED;CN=Daniel\r\n Reyes:mailto:daniel@brightmoor.co\r\nRRULE:FREQ=WEEKLY;BYDAY=WE\r\nEND:VEVENT\r\nEND:VCALENDAR\r\n";

    #[test]
    fn reads_an_itip_reply() {
        let itip = Itip::parse(REPLY).unwrap();
        assert_eq!(itip.method, "REPLY");
        assert_eq!(itip.summary, "Atlas sync, weekly");
        assert_eq!(itip.sequence, 2);
        assert_eq!(itip.organizer.as_deref(), Some("maya@brightmoor.co"));
        assert_eq!(itip.partstat("DANIEL@brightmoor.co"), Some("ACCEPTED"));
        assert_eq!(itip.recurrence, ["RRULE:FREQ=WEEKLY;BYDAY=WE"]);
        let start = itip.start.unwrap().timestamp(&TimeZone::UTC).unwrap();
        assert_eq!(
            start.to_string(),
            "2026-11-18T15:00:00Z",
            "Windows zone names resolve"
        );
        assert_eq!(
            itip.end
                .unwrap()
                .timestamp(&TimeZone::UTC)
                .unwrap()
                .to_string(),
            "2026-11-18T15:30:00Z"
        );
    }

    #[test]
    fn days_and_plain_text() {
        let itip = Itip::parse("BEGIN:VEVENT\nDTSTART;VALUE=DATE:20261126\nEND:VEVENT\n").unwrap();
        assert_eq!(itip.start, Some(When::Day(Date::constant(2026, 11, 26))));
        assert!(Itip::parse("not a calendar").is_none());
    }

    #[test]
    fn valarms_quoted_parameters_rfc6868_and_escaped_backslashes_are_preserved() {
        let raw = concat!(
            "BEGIN:VCALENDAR\r\n",
            "METHOD:REQUEST\r\n",
            "BEGIN:VEVENT\r\n",
            "UID:valarm-test@example.com\r\n",
            "DTSTART:20261118T150000Z\r\n",
            "DTEND:20261118T160000Z\r\n",
            "SUMMARY:Path C:\\\\new\\\\notes\\nand agenda\r\n",
            "LOCATION;ALTREP=\"https://room.example.com:8443/a;b\":Floor 2\\; Wing B\r\n",
            "ORGANIZER;CN=\"Maya; Role: Lead ^'PM^'^nNYC^^1\":mailto:maya@brightmoor.co\r\n",
            "ATTENDEE;CN=\"Reyes; Daniel: Staff\";PARTSTAT=TENTATIVE;RSVP=TRUE:mailto:daniel@brightmoor.co\r\n",
            "BEGIN:VALARM\r\n",
            "ACTION:EMAIL\r\n",
            "DESCRIPTION:Reminder\r\n",
            "SUMMARY:Alarm override\r\n",
            "ATTENDEE:mailto:alarm-only@brightmoor.co\r\n",
            "END:VALARM\r\n",
            "END:VEVENT\r\n",
            "END:VCALENDAR\r\n",
        );
        let itip = Itip::parse(raw).expect("parses");
        assert_eq!(itip.summary, "Path C:\\new\\notes\nand agenda");
        assert_eq!(itip.location, "Floor 2; Wing B");
        assert_eq!(itip.organizer.as_deref(), Some("maya@brightmoor.co"));
        assert_eq!(
            itip.attendees,
            vec![(
                "daniel@brightmoor.co".to_owned(),
                "TENTATIVE".to_owned(),
                true
            )],
            "VALARM attendee and summary are ignored and quoted :/; in CN do not break parsing"
        );
        let (name, params, value) = gmail::ics::content_line(
            "ORGANIZER;CN=\"Maya; Role: Lead ^'PM^'^nNYC^^1^z\":mailto:maya@brightmoor.co",
        )
        .expect("line");
        assert_eq!(name, "ORGANIZER");
        assert_eq!(value, "mailto:maya@brightmoor.co");
        assert_eq!(
            params.get("CN").map(String::as_str),
            Some("Maya; Role: Lead \"PM\"\nNYC^1^z")
        );
    }

    #[test]
    fn resolves_leading_slash_and_exchange_tzids() {
        let raw = concat!(
            "BEGIN:VCALENDAR\r\n",
            "METHOD:REQUEST\r\n",
            "BEGIN:VEVENT\r\n",
            "UID:tzid-slash@example.com\r\n",
            "DTSTART;TZID=/America/New_York:20261118T100000\r\n",
            "DTEND;TZID=\"/(UTC-05:00) Eastern Standard Time\":20261118T110000\r\n",
            "ORGANIZER:MailTo:maya@brightmoor.co\r\n",
            "END:VEVENT\r\n",
            "END:VCALENDAR\r\n",
        );
        let itip = Itip::parse(raw).expect("parses");
        assert_eq!(itip.organizer.as_deref(), Some("maya@brightmoor.co"));
        assert_eq!(
            itip.start
                .unwrap()
                .timestamp(&TimeZone::UTC)
                .unwrap()
                .to_string(),
            "2026-11-18T15:00:00Z"
        );
        assert_eq!(
            itip.end
                .unwrap()
                .timestamp(&TimeZone::UTC)
                .unwrap()
                .to_string(),
            "2026-11-18T16:00:00Z"
        );
    }
}

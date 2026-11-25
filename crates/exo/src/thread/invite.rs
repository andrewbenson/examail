//! A message's calendar invite card: a photo picked by the event's title with the update's
//! status over it, then the RSVP (or `Add To Calendar`), When, Where, video, Who, About and
//! Files rows, read from the message's `.ics`. The buttons answer through the calendar.

use std::{
    collections::HashMap,
    path::Path,
    sync::{Arc, LazyLock, Mutex},
};

use egui::{
    Color32, CornerRadius, Rect, Sense, Stroke, TextureHandle, Ui, epaint::RectShape, pos2, vec2,
};
use exo_store::{Attachment, Contact, MessageDetail};
use exo_ui::{Family, Font, TextStyle};
use gmail::ics::{content_line, resolve_timezone, strip_mailto, unescape, unfold};
use jiff::{
    SignedDuration, Timestamp, ToSpan as _, Zoned,
    civil::{Date, DateTime},
    tz::TimeZone,
};
use regex::Regex;

pub use crate::calendar::event::ConferenceCall as Meeting;
use crate::calendar::{
    description::{interact_link, linkify},
    event::extract_conference_text,
};
use crate::format::{
    dates,
    names::{NameFormat, Person, display_name},
};
pub use crate::rsvp::{ANSWERS, Rsvp as Response};
use crate::rsvp::{RsvpButtonMetrics, show_rsvp_button, show_rsvp_buttons};

/// The card's text is 14px/24px, and the row names are bold with `min-width: 92px`.
const TEXT: TextStyle = TextStyle::new(Font::new(Family::Sans, 400), 14.0, 24.0);
const BOLD: TextStyle = TextStyle::new(Font::new(Family::Sans, 700), 14.0, 24.0);
const QUESTION: f32 = 92.0;
/// A row's value: `padding-left: 12px`.
const PROPERTY_PADDING: f32 = 12.0;
/// The photo: 176px tall.
const IMAGE_HEIGHT: f32 = 176.0;
/// The rows: `padding: 28px 24px 24px`, 16px apart.
const ROWS_TOP: f32 = 28.0;
const ROWS_SIDE: f32 = 24.0;
const ROWS_BOTTOM: f32 = 24.0;
const ROW_GAP: f32 = 16.0;
/// The card's `margin-bottom: 16px; top: 1px`.
pub(super) const MARGIN_BOTTOM: f32 = 16.0;
/// An RSVP button: 28px tall, `padding: 0 12px; margin-top: -4px`, 16px apart.
const BUTTON_HEIGHT: f32 = 28.0;
const BUTTON_PADDING: f32 = 12.0;
const BUTTON_GAP: f32 = 16.0;
/// Who lists the first 15 attendees.
const MAX_WHO: usize = 15;
/// About shows the first 8 lines until `Show more`.
const ABOUT_LINES: usize = 8;
/// About's `Show more`: 16px under the text.
const SHOW_MORE_GAP: f32 = 16.0;
/// The status over the photo: `top: 16px; left: 24px; margin-left: -7px; padding: 4px 7px
/// 0`, a 1px border, `max-width: calc(100% - 24px)`.
const STATUS_TOP: f32 = 16.0;
const STATUS_LEFT: f32 = 24.0 - 7.0;
const STATUS_PADDING_TOP: f32 = 4.0;
const STATUS_PADDING_X: f32 = 7.0;
/// Files: three columns `minmax(120px, 154.16px)`, 8px apart.
const FILE_WIDTH: f32 = 154.16;
const FILE_GAP: f32 = 8.0;
/// The guest's note: the email sans at 14px, `line-height: 1.714` (24px), `padding-bottom:
/// 16px`. Its `margin-top: 1px` collapses into the body's 20px.
const NOTE: TextStyle = TextStyle::new(Font::new(Family::SansEmail, 400), 14.0, 24.0);
const NOTE_BOTTOM: f32 = 16.0;
/// A title longer than this leads the description.
const LONG_SUMMARY: usize = 60;

/// What a click on the card asked for.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum CardAction {
    /// A web link: the location's map, the video call, a file.
    Open(String),
    /// A validated phone number (`tel:` and 7..=15 digits, built by [`phone_href`]).
    Dial(String),
    /// An answer other than the current one. `NeedsAction` adds the event to the calendar.
    Respond(Response),
    /// `Show more` / `Show Less`.
    ToggleAbout,
}

/// A file the invite attaches (`ATTACH` with a name and a type).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct File {
    pub name: String,
    pub mime: String,
    pub url: String,
}

/// What the card shows of an invite.
#[derive(Clone, Debug, PartialEq)]
pub struct Invite {
    /// `UID`, which the event on the calendar shares.
    pub uid: String,
    pub summary: String,
    /// The description, with its `<url>`s and Teams' join lines made links.
    pub description: String,
    pub location: String,
    pub start: Timestamp,
    pub end: Timestamp,
    /// True when `DTSTART` is a date-only (`VALUE=DATE` / `YYYYMMDD`) all-day event.
    pub all_day: bool,
    /// Civil `(start, end)` dates when `all_day` is true (exclusive `end` date).
    pub dates: Option<(Date, Date)>,
    /// The `METHOD` is `REQUEST` (an invitation, not a reply or a cancellation).
    pub request: bool,
    /// `STATUS:CANCELLED` or `METHOD:CANCEL`.
    pub cancelled: bool,
    /// `METHOD:COUNTER`, where a guest proposes the time it gives (RFC 5546 §3.2.7).
    pub counter: bool,
    /// It repeats (`RRULE`, `EXRULE`, `RDATE` or `EXDATE`).
    pub recurring: bool,
    /// The calendar that made it (`PRODID`).
    pub product: String,
    /// Attendees: name, address, answer.
    pub attendees: Vec<(String, String, Response)>,
    /// The organizer: name, address.
    pub organizer: Option<(String, String)>,
    pub meeting: Option<Meeting>,
    pub files: Vec<File>,
    /// A guest's note: the one attendee's `X-RESPONSE-COMMENT`, else the event's `COMMENT`.
    /// Empty without one.
    pub note: String,
    /// What I answered here, which overrides the answer in the `.ics`.
    pub saved: Option<Response>,
}

impl Invite {
    /// Resolves `(start, end)` in `tz`, keeping date-only all-day invites anchored to their civil
    /// dates in `tz` rather than shifting across `TimeZone::system()`.
    #[must_use]
    pub fn zoned(&self, tz: &TimeZone) -> (Zoned, Zoned) {
        if self.all_day
            && let Some((first, last)) = self.dates
            && let (Ok(start), Ok(end)) = (first.to_zoned(tz.clone()), last.to_zoned(tz.clone()))
        {
            return (start, end);
        }
        (
            self.start.to_zoned(tz.clone()),
            self.end.to_zoned(tz.clone()),
        )
    }
    /// Who I am on the invite: `me` (the mail account) when it's invited, else the first of my
    /// other calendar accounts in `accounts` that is, else `me`.
    pub fn calendar_account<'a>(&'a self, me: &'a str, accounts: &[String]) -> &'a str {
        if self
            .attendees
            .iter()
            .any(|(_, email, _)| email.eq_ignore_ascii_case(me))
        {
            return me;
        }
        self.attendees
            .iter()
            .map(|(_, email, _)| email.as_str())
            .find(|email| {
                accounts
                    .iter()
                    .any(|account| account.eq_ignore_ascii_case(email))
            })
            .unwrap_or(me)
    }

    /// Whether I can answer it: I'm on it, and it isn't called off.
    pub fn can_respond(&self, me: &str) -> bool {
        !self.cancelled
            && self
                .attendees
                .iter()
                .any(|(_, email, _)| email.eq_ignore_ascii_case(me))
    }

    /// Whether `Add To Calendar` shows instead: not for a series, an answer or a cancellation.
    pub fn can_add_to_calendar(&self, me: &str) -> bool {
        !self.can_respond(me) && !self.recurring && !self.is_response(me) && !self.cancelled
    }

    /// My answer: the one given here, else as the invite has it.
    pub fn answer_of(&self, me: &str) -> Option<Response> {
        self.saved.or_else(|| {
            self.attendees
                .iter()
                .find(|(_, email, _)| email.eq_ignore_ascii_case(me))
                .map(|(_, _, response)| *response)
        })
    }

    /// Records `response` as my answer.
    pub fn answer(&mut self, me: &str, response: Response) {
        self.saved = Some(response);
        for (_, email, answer) in &mut self.attendees {
            if email.eq_ignore_ascii_case(me) {
                *answer = response;
            }
        }
    }

    /// Whether this is a guest's answer to an invitation (one attendee, not me, who answered) or a
    /// guest's proposal (`METHOD:COUNTER`).
    fn is_response(&self, me: &str) -> bool {
        let [(_, email, response)] = self.attendees.as_slice() else {
            return false;
        };
        !email.eq_ignore_ascii_case(me) && (self.counter || *response != Response::NeedsAction)
    }

    /// The description as About shows it, with Calendar's boilerplate dropped and a long title
    /// first. `None` when there's nothing.
    fn about(&self) -> Option<String> {
        let summary = self.summary.trim();
        let long = summary.chars().count() > LONG_SUMMARY;
        if self.description.is_empty() && !long {
            return None;
        }
        let cleaned = crate::calendar::description::clean(&self.description);
        Some(if long {
            format!("{summary}\n\n{cleaned}")
        } else {
            cleaned
        })
    }
}

/// The message's invite attachment, if it carries one (not on a forward, `Fwd…`).
pub(crate) fn attachment(message: &MessageDetail) -> Option<&Attachment> {
    let subject = message.subject.trim_start().to_lowercase();
    if subject.starts_with("fwd") || subject.starts_with("fw:") {
        return None;
    }
    message
        .attachments
        .iter()
        .find(|attachment| super::attachments::is_invite(attachment))
}

/// Whether the card stands in for the message's body: Google Calendar's notification (its
/// microdata, or sent by `calendar-notification@google.com`), or
/// Outlook's when it isn't a reply.
pub(super) fn hides_body(invite: &Invite, message: &MessageDetail, gcal: bool) -> bool {
    let sender = message
        .headers
        .iter()
        .find(|header| header.name.eq_ignore_ascii_case("sender"))
        .map(|header| gmail::mime::Mailbox::parse(&header.value).email);
    let google = gcal
        || sender
            .is_some_and(|sender| sender.eq_ignore_ascii_case("calendar-notification@google.com"));
    let outlook = ["Outlook", "Microsoft Exchange Server 2010"].contains(&invite.product.as_str())
        && message.in_reply_to.is_none();
    google || outlook
}

#[derive(Clone, Debug)]
enum ParsedWhen {
    Day(Date),
    Zoned(Zoned),
}

/// Reads the first `VEVENT` of an iCalendar file.
pub fn parse(ics: &str) -> Option<Invite> {
    let mut in_event = false;
    let mut nested_depth: usize = 0;
    let mut method = String::new();
    let mut status = String::new();
    let mut invite = Invite {
        uid: String::new(),
        summary: String::new(),
        description: String::new(),
        location: String::new(),
        start: Timestamp::UNIX_EPOCH,
        end: Timestamp::UNIX_EPOCH,
        all_day: false,
        dates: None,
        request: false,
        cancelled: false,
        counter: false,
        recurring: false,
        product: String::new(),
        attendees: Vec::new(),
        organizer: None,
        meeting: None,
        files: Vec::new(),
        note: String::new(),
        saved: None,
    };
    let mut parsed_start: Option<ParsedWhen> = None;
    let mut parsed_end: Option<ParsedWhen> = None;
    let mut parsed_duration: Option<(i64, SignedDuration)> = None;
    let (mut comment, mut response_comments) = (None, Vec::new());
    for line in unfold(ics) {
        let Some((name, params, value)) = content_line(&line) else {
            continue;
        };
        if in_event && nested_depth > 0 {
            match name.as_str() {
                "BEGIN" => nested_depth += 1,
                "END" => nested_depth = nested_depth.saturating_sub(1),
                _ => {}
            }
            continue;
        }
        match name.as_str() {
            "METHOD" if !in_event => method = value.trim().to_ascii_uppercase(),
            "PRODID" if !in_event => invite.product = value.trim().to_owned(),
            "BEGIN" if !in_event && value.eq_ignore_ascii_case("VEVENT") => {
                in_event = true;
                nested_depth = 0;
            }
            "BEGIN" if in_event => nested_depth = 1,
            "END" if in_event && value.eq_ignore_ascii_case("VEVENT") => break,
            _ if !in_event => {}
            "UID" => invite.uid = value.trim().to_owned(),
            "SUMMARY" => invite.summary = unescape(value),
            "DESCRIPTION" => invite.description = unescape(value),
            "LOCATION" => invite.location = unescape(value),
            "COMMENT" if comment.is_none() => comment = Some(unescape(value)),
            "STATUS" => status = value.trim().to_ascii_uppercase(),
            "RRULE" | "EXRULE" | "RDATE" | "EXDATE" => invite.recurring = true,
            "DTSTART" => {
                parsed_start = Some(when_of(value, params.get("TZID"), params.get("VALUE"))?);
            }
            "DTEND" => {
                parsed_end = Some(when_of(value, params.get("TZID"), params.get("VALUE"))?);
            }
            "DURATION" => {
                parsed_duration = Some(parse_duration(value)?);
            }
            "ORGANIZER" => {
                let email = strip_mailto(value).to_owned();
                invite.organizer = Some((params.get("CN").cloned().unwrap_or_default(), email));
            }
            "ATTENDEE" => {
                let email = strip_mailto(value).to_owned();
                let response = Response::parse_ics(params.get("PARTSTAT").map(String::as_str));
                invite.attendees.push((
                    params.get("CN").cloned().unwrap_or_default(),
                    email,
                    response,
                ));
                response_comments.push(
                    params
                        .get("X-RESPONSE-COMMENT")
                        .map_or("", |comment| comment.trim())
                        .to_owned(),
                );
            }
            // An attachment is listed only with a name, a type, and a web URL.
            "ATTACH" => {
                if let (Some(name), Some(mime), Some(url)) = (
                    params.get("FILENAME"),
                    params.get("FMTTYPE"),
                    crate::format::urls::web(value.trim()),
                ) {
                    invite.files.push(File {
                        name: name.clone(),
                        mime: mime.clone(),
                        url,
                    });
                }
            }
            _ => {}
        }
    }
    match parsed_start? {
        ParsedWhen::Day(start_date) => {
            let end_date = match (parsed_end, parsed_duration) {
                (Some(ParsedWhen::Day(end)), _) if end > start_date => end,
                (Some(ParsedWhen::Zoned(end)), _) if end.date() > start_date => end.date(),
                (_, Some((days, sub))) => {
                    let span_days = (days + sub.as_secs() / 86_400).max(1);
                    start_date.checked_add(span_days.days()).ok()?
                }
                _ => start_date.tomorrow().ok()?,
            };
            invite.all_day = true;
            invite.dates = Some((start_date, end_date));
            invite.start = start_date
                .at(11, 0, 0, 0)
                .to_zoned(TimeZone::UTC)
                .ok()?
                .timestamp();
            invite.end = end_date
                .at(11, 0, 0, 0)
                .to_zoned(TimeZone::UTC)
                .ok()?
                .timestamp();
        }
        ParsedWhen::Zoned(start_zoned) => {
            invite.all_day = false;
            invite.dates = None;
            invite.start = start_zoned.timestamp();
            invite.end = match (parsed_end, parsed_duration) {
                (Some(ParsedWhen::Zoned(end)), _) => end.timestamp(),
                (Some(ParsedWhen::Day(end)), _) => end
                    .to_zoned(start_zoned.time_zone().clone())
                    .ok()?
                    .timestamp(),
                (None, Some((days, sub))) => start_zoned
                    .checked_add(days.days())
                    .ok()?
                    .checked_add(sub)
                    .ok()?
                    .timestamp(),
                (None, None) => invite.start,
            };
        }
    }
    invite.request = method == "REQUEST";
    invite.counter = method == "COUNTER";
    invite.cancelled = status == "CANCELLED" || method == "CANCEL";
    let (description, meeting) = extract_meeting(&invite.description);
    (invite.description, invite.meeting) = (description, meeting);
    invite.note = match response_comments.as_slice() {
        [only] if !only.is_empty() => only.clone(),
        _ => comment.unwrap_or_default(),
    };
    Some(invite)
}

/// Rewrites `<url>` and Teams join lines into links and extracts a `Meet`, `Teams`, or `Zoom`
/// call via [`crate::calendar::event::extract_conference_text`].
fn extract_meeting(description: &str) -> (String, Option<Meeting>) {
    extract_conference_text(description)
}

/// Parses an RFC 5545 `DATE` (`VALUE=DATE` or 8-digit `YYYYMMDD`) or `DATE-TIME` (`...Z` or local
/// with IANA / Windows `TZID`).
fn when_of(value: &str, zone: Option<&String>, kind: Option<&String>) -> Option<ParsedWhen> {
    let value = value.trim();
    let utc = value.ends_with('Z');
    let digits = value.trim_end_matches('Z');
    if kind.is_some_and(|v| v.eq_ignore_ascii_case("DATE")) || (!utc && digits.len() == 8) {
        return Date::strptime("%Y%m%d", digits).ok().map(ParsedWhen::Day);
    }
    let civil = DateTime::strptime("%Y%m%dT%H%M%S", digits).ok()?;
    let zone = if utc {
        TimeZone::UTC
    } else {
        zone.and_then(|name| resolve_timezone(name))
            .unwrap_or_else(TimeZone::system)
    };
    civil.to_zoned(zone).ok().map(ParsedWhen::Zoned)
}

/// Parses an RFC 5545 / ISO 8601 `DURATION` value (`P1W`, `P2D`, `PT1H30M`, `P1DT2H`, `-PT15M`)
/// into `(days, sub_day_duration)`.
fn parse_duration(raw: &str) -> Option<(i64, SignedDuration)> {
    let trimmed = raw.trim();
    let (sign, rest) = if let Some(rest) = trimmed.strip_prefix('-') {
        (-1_i64, rest)
    } else {
        (1_i64, trimmed.strip_prefix('+').unwrap_or(trimmed))
    };
    let body = rest.strip_prefix('P').or_else(|| rest.strip_prefix('p'))?;
    if body.is_empty() {
        return None;
    }
    let (date_part, time_part) = match body.find(['T', 't']) {
        Some(idx) => {
            let tail = &body[idx + 1..];
            if tail.is_empty() {
                return None;
            }
            (&body[..idx], Some(tail))
        }
        None => (body, None),
    };
    let mut days: i64 = 0;
    let mut secs: i64 = 0;
    let mut parsed_any = false;
    let mut num: i64 = 0;
    let mut has_digits = false;
    for ch in date_part.chars() {
        if let Some(d) = ch.to_digit(10) {
            num = num.checked_mul(10)?.checked_add(i64::from(d))?;
            has_digits = true;
        } else {
            if !has_digits {
                return None;
            }
            match ch.to_ascii_uppercase() {
                'W' => days = days.checked_add(num.checked_mul(7)?)?,
                'D' => days = days.checked_add(num)?,
                _ => return None,
            }
            parsed_any = true;
            num = 0;
            has_digits = false;
        }
    }
    if has_digits {
        return None;
    }
    if let Some(time_part) = time_part {
        for ch in time_part.chars() {
            if let Some(d) = ch.to_digit(10) {
                num = num.checked_mul(10)?.checked_add(i64::from(d))?;
                has_digits = true;
            } else {
                if !has_digits {
                    return None;
                }
                match ch.to_ascii_uppercase() {
                    'H' => secs = secs.checked_add(num.checked_mul(3600)?)?,
                    'M' => secs = secs.checked_add(num.checked_mul(60)?)?,
                    'S' => secs = secs.checked_add(num)?,
                    _ => return None,
                }
                parsed_any = true;
                num = 0;
                has_digits = false;
            }
        }
        if has_digits {
            return None;
        }
    }
    parsed_any.then(|| {
        (
            days.saturating_mul(sign),
            SignedDuration::from_secs(secs.saturating_mul(sign)),
        )
    })
}

/// Which photo heads the card.
pub(super) fn image_kind(invite: &Invite) -> &'static str {
    let attendees = invite.attendees.len();
    kind(&invite.summary, attendees)
        .or_else(|| kind(&invite.description, attendees))
        .unwrap_or("default")
}

fn kind(text: &str, attendees: usize) -> Option<&'static str> {
    const EDGE: &str = r#"(^|$|\s|-|—|[.;:'"()?!&_\/~])"#;
    static RULES: LazyLock<Vec<(Regex, &'static str)>> = LazyLock::new(|| {
        [
            (r"beach|swim|surf|boat|jet(-|\s)?ski|snorkling|scuba(-|\s)?div|paddling|whale(-|\s)?watching|kayak|sail".to_owned(), "beach"),
            (r"birth\s?day|b'?day".to_owned(), "birthday"),
            (r"meeting|interview|planning|stand-?up|agenda|all(\s|-)?hand|on\-?boarding".to_owned(), "meeting"),
            (format!(r"{EDGE}call{EDGE}|phone"), "call"),
            (format!(r"coff?ee|{EDGE}caf(e|é){EDGE}|espresso|starbucks|cappuccino|mocha"), "coffee"),
            (r"dinner|restaurant|cook\-off|dining|dine|supper".to_owned(), "dinner"),
            (format!(r"drink|{EDGE}bar{EDGE}|happy\shour|cocktail|whiske?y|bourbon|vodka|{EDGE}gin{EDGE}|tequila|liquor|{EDGE}wine{EDGE}"), "drinks"),
            (format!(r"exc?er(c|s)i(s|z)e|workout|{EDGE}gym{EDGE}|treadmill|{EDGE}run{EDGE}"), "exercise"),
            (format!(r"flight|airport|flying|airplane|{EDGE}jet{EDGE}"), "flight"),
            (r"hike|hiking".to_owned(), "hike"),
            (r"breakfast|brunch".to_owned(), "breakfast"),
            (format!(r"lunch|{EDGE}eat{EDGE}|food|meal"), "lunch"),
            (r"party|rave|rager|celebration|gala".to_owned(), "party"),
            (format!(r"{EDGE}(tea|ocha|matcha){EDGE}"), "tea"),
            (format!(r"({EDGE}1(-|\s)?on(-|\s)?1{EDGE})|(1\s?(\:|\-|/)\s?1{EDGE})|(one(-|\s)?on(-|\s)?one)"), "1on1"),
        ]
        .into_iter()
        .map(|(pattern, kind)| (Regex::new(&format!("(?i){pattern}")).expect("valid regex"), kind))
        .collect()
    });
    static PAIR: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"<(\s|-)?>").expect("valid regex"));
    // A call over video is a meeting, not a phone call.
    static VIDEO_CALL: LazyLock<Regex> =
        LazyLock::new(|| Regex::new(r"(?i)video\scall").expect("valid regex"));
    for (rule, kind) in RULES.iter() {
        if rule.is_match(text) && !(*kind == "call" && VIDEO_CALL.is_match(text)) {
            return Some(kind);
        }
    }
    PAIR.is_match(text)
        .then_some(if attendees == 2 { "1on1" } else { "meeting" })
}

/// Decoded invite card textures cached in `egui::Context` data per `(images, kind)`, so opening or
/// stepping across threads reuses uploaded textures without re-reading or decoding JPEGs on the UI
/// thread.
type PhotoCache = Arc<Mutex<HashMap<&'static str, Option<TextureHandle>>>>;

fn photo(ctx: &egui::Context, images: &Path, kind: &'static str) -> Option<TextureHandle> {
    let cache: PhotoCache = ctx.data_mut(|data| {
        data.get_temp_mut_or_default::<PhotoCache>(egui::Id::new((
            "exo::thread::invite::photos",
            images,
        )))
        .clone()
    });
    if let Ok(guard) = cache.lock()
        && let Some(cached) = guard.get(kind)
    {
        return cached.clone();
    }
    let loaded = load_photo(ctx, images, kind);
    match cache.lock() {
        Ok(mut guard) => guard.entry(kind).or_insert(loaded).clone(),
        Err(_) => loaded,
    }
}

fn load_photo(ctx: &egui::Context, images: &Path, kind: &'static str) -> Option<TextureHandle> {
    let path = images.join(format!("invite-{kind}.jpg"));
    let decoded = std::fs::read(&path)
        .ok()
        .and_then(|bytes| image::load_from_memory(&bytes).ok());
    let Some(decoded) = decoded else {
        log::warn!("no invite photo at {}", path.display());
        return None;
    };
    let rgba = decoded.to_rgba8();
    let color = egui::ColorImage::from_rgba_unmultiplied(
        [rgba.width() as usize, rgba.height() as usize],
        rgba.as_raw(),
    );
    Some(ctx.load_texture(
        format!("invite {kind}"),
        color,
        egui::TextureOptions::LINEAR,
    ))
}

/// What the card knows beyond the invite: whose card it is (the calendar account it's to), the
/// mail account (Who's `Me`), the message it came in, the notification's facts, and whether
/// About is opened up.
pub(super) struct Context<'a> {
    pub me: &'a str,
    pub user: &'a str,
    pub now: &'a Zoned,
    pub message: &'a MessageDetail,
    pub calendar: Option<&'a exo_mail::CalendarData>,
    pub about_open: bool,
}

/// The status over the photo: a cancellation or an update, else a guest's answer.
fn status(invite: &Invite, context: &Context) -> Option<String> {
    let calendar = context.calendar.cloned().unwrap_or_default();
    let updated = context.message.subject.contains("Updated Invitation:");
    if invite.cancelled || updated || calendar.event_status.is_some() {
        if invite.cancelled || calendar.event_status == Some(exo_mail::EventStatus::Cancelled) {
            return Some("Event has been canceled".to_owned());
        }
        if !(updated
            || calendar.event_status == Some(exo_mail::EventStatus::Rescheduled)
            || calendar.updated_fields.is_some())
        {
            return None;
        }
        return Some(match &calendar.updated_fields {
            Some(fields) => format!("Event has been changed - {fields}"),
            None => "Event has been changed".to_owned(),
        });
    }
    if !invite.is_response(context.me) {
        return None;
    }
    let (name, email, response) = &invite.attendees[0];
    if invite.counter {
        return Some(format!(
            "{} has proposed a new time",
            responder(name, email, context.message)
        ));
    }
    let answered = match response {
        Response::Accepted => "accepted",
        Response::Tentative => "responded maybe",
        Response::Declined => "declined",
        Response::NeedsAction => return None,
    };
    Some(format!(
        "{} has {answered}",
        responder(name, email, context.message)
    ))
}

/// The responder's name: the attendee's first name (as the invite names them), else the sender's
/// name when it's them, else the address.
fn responder(name: &str, email: &str, message: &MessageDetail) -> String {
    if !name.is_empty() && !name.contains('@') {
        return display_name(
            &Person::new(name.to_owned(), email.to_owned()),
            false,
            NameFormat::First,
            false,
        );
    }
    message
        .from
        .as_ref()
        .filter(|from| from.email.eq_ignore_ascii_case(email))
        .and_then(|from| from.name.clone())
        .filter(|name| !name.is_empty())
        .unwrap_or_else(|| email.to_owned())
}

/// What clicking a linked piece does: open a web URL, or dial a phone number built by
/// [`phone_href`].
#[derive(Clone, Debug, PartialEq, Eq)]
enum PieceLink {
    Web(String),
    Dial(String),
}

impl PieceLink {
    fn as_str(&self) -> &str {
        match self {
            Self::Web(url) | Self::Dial(url) => url,
        }
    }

    fn into_string(self) -> String {
        match self {
            Self::Web(url) | Self::Dial(url) => url,
        }
    }

    fn to_action(&self) -> CardAction {
        match self {
            Self::Web(url) => CardAction::Open(url.clone()),
            Self::Dial(tel) => CardAction::Dial(tel.clone()),
        }
    }
}

/// A piece of a row's value: text, a link when `link` is set, bold when `bold`, a participant's
/// name with their tooltip when `contact` is.
#[derive(Clone, Debug, Default, PartialEq)]
struct Piece {
    text: String,
    link: Option<PieceLink>,
    bold: bool,
    contact: Option<Contact>,
}

impl Piece {
    fn text(text: impl Into<String>) -> Self {
        Self {
            text: text.into(),
            ..Self::default()
        }
    }

    fn link(text: impl Into<String>, url: impl Into<String>) -> Self {
        Self {
            text: text.into(),
            link: Some(PieceLink::Web(url.into())),
            ..Self::default()
        }
    }

    fn dial(text: impl Into<String>, tel: String) -> Self {
        Self {
            text: text.into(),
            link: Some(PieceLink::Dial(tel)),
            ..Self::default()
        }
    }
}

/// The card's rows: label and value.
struct Row {
    question: &'static str,
    value: Value,
}

enum Value {
    /// Lines of pieces, each line breaking where text breaks, indented by its first number.
    Text(Vec<(f32, Vec<Piece>)>),
    /// When's text, which breaks only after ` at`, `,` and `– ` (`white-space: nowrap` with
    /// `<wbr>`s).
    When(String),
    Rsvp(Option<Response>),
    AddToCalendar(bool),
    /// About's lines, and `Show more` (false) or `Show Less` (true) under them.
    About(Vec<(f32, Vec<Piece>)>, Option<bool>),
    Files(Vec<File>),
}

fn rows(invite: &Invite, context: &Context) -> Vec<Row> {
    let me = context.me;
    let mut rows = Vec::new();
    // My answer given here, else the invite's.
    let answer = invite.answer_of(me);
    if invite.can_respond(me) {
        rows.push(Row {
            question: "RSVP",
            value: Value::Rsvp(answer.filter(|answer| *answer != Response::NeedsAction)),
        });
    } else if invite.can_add_to_calendar(me) {
        rows.push(Row {
            question: "",
            value: Value::AddToCalendar(invite.saved == Some(Response::NeedsAction)),
        });
    }
    let (start, end) = invite.zoned(context.now.time_zone());
    let when = dates::invite_range(&start, &end, invite.all_day, invite.recurring);
    rows.push(Row {
        question: "When",
        value: Value::When(when),
    });
    if let Some(location) = location(invite) {
        rows.push(Row {
            question: "Where",
            value: Value::Text(vec![(0.0, location)]),
        });
    }
    if let Some(meeting) = &invite.meeting {
        let shown = if meeting.url.contains("teams.microsoft.com/l/meetup-join/") {
            "https://teams.microsoft.com/l/meetup-join/…"
        } else {
            &meeting.url
        };
        rows.push(Row {
            question: meeting.label,
            value: Value::Text(vec![(0.0, vec![Piece::link(shown, &meeting.url)])]),
        });
    }
    if !invite.attendees.is_empty() {
        // Each name with its contact tooltip, ", " between.
        let mut who = Vec::new();
        for (at, (name, email, _)) in invite.attendees.iter().take(MAX_WHO).enumerate() {
            if at > 0 {
                who.push(Piece::text(", "));
            }
            // The mail account is `Me`. Another calendar account of mine goes by its name.
            let is_me = email.eq_ignore_ascii_case(context.user);
            let text = display_name(
                &Person::new(name.clone(), email.clone()),
                is_me,
                NameFormat::First,
                false,
            );
            let contact = Contact {
                name: Some(name.clone()).filter(|name| !name.is_empty()),
                email: email.clone(),
                is_me,
            };
            who.push(Piece {
                text,
                contact: Some(contact),
                ..Piece::default()
            });
        }
        let more = invite.attendees.len().saturating_sub(MAX_WHO);
        if more > 0 {
            who.push(Piece::text(format!(" and {more} more")));
        }
        rows.push(Row {
            question: "Who",
            value: Value::Text(vec![(0.0, who)]),
        });
    }
    if let Some(about) = invite.about() {
        let all = crate::calendar::description::lines(&about);
        if !all.is_empty() {
            let source: Vec<&str> = about
                .split("\r\n")
                .flat_map(|line| line.split(['\r', '\n']))
                .collect();
            let long = source.len() > ABOUT_LINES;
            let shown = if long && !context.about_open {
                crate::calendar::description::lines(&source[..ABOUT_LINES].join("\n"))
            } else {
                all
            };
            let lines = shown
                .into_iter()
                .map(|line| {
                    (
                        line.indent,
                        line.runs
                            .into_iter()
                            .map(|run| Piece {
                                text: run.text,
                                link: run.link.map(PieceLink::Web),
                                bold: run.bold,
                                contact: None,
                            })
                            .collect(),
                    )
                })
                .collect();
            rows.push(Row {
                question: "About",
                value: Value::About(lines, long.then_some(context.about_open)),
            });
        }
    }
    if !invite.files.is_empty() {
        rows.push(Row {
            question: "Files",
            value: Value::Files(invite.files.clone()),
        });
    }
    rows
}

/// The Where row's pieces: nothing for a Teams call the video row has; Teams' own words as they
/// are; a phone number to call; the location's links; else a map search.
fn location(invite: &Invite) -> Option<Vec<Piece>> {
    let location = &invite.location;
    if location.is_empty() {
        return None;
    }
    let teams = location
        .trim()
        .eq_ignore_ascii_case("microsoft teams meeting");
    if teams
        && invite
            .meeting
            .as_ref()
            .is_some_and(|meeting| meeting.label == "Teams")
    {
        return None;
    }
    if teams {
        return Some(vec![Piece::text(location)]);
    }
    if let Some(tel) = phone_href(location) {
        return Some(vec![Piece::dial(location, tel)]);
    }
    let pieces = pieces_from_runs(linkify(location));
    if pieces.iter().any(|piece| piece.link.is_some()) {
        return Some(pieces);
    }
    let query: String = url::form_urlencoded::byte_serialize(location.as_bytes()).collect();
    Some(vec![Piece::link(
        location,
        format!(
            "https://maps.google.com/maps?q={}",
            query.replace('+', "%20")
        ),
    )])
}

/// A `tel:` link for a location that's only a phone number, of 7 to 15 digits.
fn phone_href(location: &str) -> Option<String> {
    static PHONE: LazyLock<Regex> =
        LazyLock::new(|| Regex::new(r"^\+?[\d\s\-().]+$").expect("valid regex"));
    let location = location.trim();
    if !PHONE.is_match(location) {
        return None;
    }
    let digits: String = location.chars().filter(char::is_ascii_digit).collect();
    (7..=15).contains(&digits.len()).then(|| {
        format!(
            "tel:{}{digits}",
            if location.starts_with('+') { "+" } else { "" }
        )
    })
}

/// Pieces broken into lines no wider than `width` at spaces, as Chromium breaks text.
fn wrap(ctx: &egui::Context, pieces: &[Piece], width: f32) -> Vec<Vec<(Piece, f32)>> {
    wrap_in(
        ctx,
        pieces,
        width,
        |piece| if piece.bold { BOLD } else { TEXT },
    )
}

/// [`wrap`] with each piece in `style`'s face.
fn wrap_in(
    ctx: &egui::Context,
    pieces: &[Piece],
    width: f32,
    style: impl Fn(&Piece) -> TextStyle,
) -> Vec<Vec<(Piece, f32)>> {
    let mut lines: Vec<Vec<(Piece, f32)>> = vec![Vec::new()];
    let mut used = 0.0;
    for piece in pieces {
        for word in piece.text.split_inclusive(' ') {
            let word_piece = Piece {
                text: word.to_owned(),
                ..piece.clone()
            };
            let fit = style(piece).width(ctx, word.trim_end_matches(' '));
            let line = lines.last_mut().expect("a line");
            if !line.is_empty() && used + fit > width {
                lines.push(Vec::new());
                used = 0.0;
            }
            let advance = style(piece).width(ctx, word);
            lines
                .last_mut()
                .expect("a line")
                .push((word_piece, advance));
            used += advance;
        }
    }
    lines
}

/// When's lines, each a list of pieces. The time splits at ` at`, `,` and `– `, and a line breaks
/// only between pieces, each shaped on its own (no kerning across them).
fn when_lines(ctx: &egui::Context, when: &str, width: f32) -> Vec<Vec<String>> {
    static SPLIT: LazyLock<Regex> =
        LazyLock::new(|| Regex::new(r"( at|,|– )").expect("valid regex"));
    let mut pieces = Vec::new();
    let mut at = 0;
    for found in SPLIT.find_iter(when) {
        pieces.extend([&when[at..found.start()], found.as_str()]);
        at = found.end();
    }
    pieces.push(&when[at..]);
    let mut lines: Vec<Vec<String>> = vec![Vec::new()];
    let mut used = 0.0;
    for piece in pieces.into_iter().filter(|piece| !piece.is_empty()) {
        let advance = TEXT.width(ctx, piece);
        if !lines.last().is_some_and(Vec::is_empty) && used + advance > width {
            // A new line drops the spaces it starts with.
            let piece = piece.trim_start();
            used = TEXT.width(ctx, piece);
            lines.push(vec![piece.to_owned()]);
        } else {
            used += advance;
            lines.last_mut().expect("a line").push(piece.to_owned());
        }
    }
    lines
}

/// The height of a row's value in a value column `width` wide.
fn value_height(ctx: &egui::Context, value: &Value, width: f32) -> f32 {
    let lines = |lines: &[(f32, Vec<Piece>)]| {
        lines
            .iter()
            .map(|(indent, pieces)| wrap(ctx, pieces, width - indent).len().max(1) as f32)
            .sum::<f32>()
            * TEXT.line_height
    };
    match value {
        Value::Text(text) => lines(text),
        Value::When(when) => when_lines(ctx, when, width).len() as f32 * TEXT.line_height,
        Value::Rsvp(_) | Value::AddToCalendar(_) => TEXT.line_height,
        Value::About(text, more) => {
            lines(text)
                + if more.is_some() {
                    SHOW_MORE_GAP + TEXT.line_height
                } else {
                    0.0
                }
        }
        Value::Files(files) => {
            let (size, _) = file_grid(width);
            files.len().div_ceil(3) as f32 * (size.y + FILE_GAP) - FILE_GAP
        }
    }
}

/// One file card's size, and the gap between, in a value column `width` wide.
fn file_grid(width: f32) -> (egui::Vec2, f32) {
    let card = ((width - 2.0 * FILE_GAP) / 3.0).clamp(120.0, FILE_WIDTH);
    let (base, _) = exo_ui::attachment_grid(4.0 * (FILE_WIDTH + 2.0 * exo_ui::CARD_MARGIN_X));
    (vec2(card, card * base.y / base.x), FILE_GAP)
}

/// The value column's width for a card `width` wide.
fn value_width(width: f32) -> f32 {
    width - 2.0 * ROWS_SIDE - QUESTION - PROPERTY_PADDING
}

/// The card's height for a column `width` wide.
pub(super) fn height(ui: &Ui, invite: &Invite, context: &Context, width: f32) -> f32 {
    let rows = rows(invite, context);
    let values: f32 = rows
        .iter()
        .map(|row| value_height(ui.ctx(), &row.value, value_width(width)))
        .sum();
    IMAGE_HEIGHT + ROWS_TOP + values + (rows.len().saturating_sub(1)) as f32 * ROW_GAP + ROWS_BOTTOM
}

/// The guest's note above the card: the notification's, else the `.ics`'s, read as HTML's text
/// with its spaces collapsed (`white-space: normal`). A note of only markup is present but empty.
pub(super) fn note(invite: &Invite, calendar: Option<&exo_mail::CalendarData>) -> Option<String> {
    let note = calendar
        .and_then(|calendar| calendar.response_comment.as_deref())
        .filter(|note| !note.is_empty())
        .unwrap_or(&invite.note);
    (!note.is_empty()).then(|| {
        exo_mail::html_text(note)
            .split_whitespace()
            .collect::<Vec<_>>()
            .join(" ")
    })
}

/// Converts [`crate::calendar::description::Run`]s into card [`Piece`]s.
fn pieces_from_runs(runs: Vec<crate::calendar::description::Run>) -> Vec<Piece> {
    runs.into_iter()
        .filter(|run| !run.text.is_empty())
        .map(|run| Piece {
            text: run.text,
            link: run.link.map(PieceLink::Web),
            bold: run.bold,
            contact: None,
        })
        .collect()
}

/// The note's pieces, with its URLs made links.
fn note_pieces(note: &str) -> Vec<Piece> {
    pieces_from_runs(linkify(note))
}

/// The note's lines in a column `width` wide, with its URLs made links.
fn note_lines(ctx: &egui::Context, note: &str, width: f32) -> Vec<Vec<(Piece, f32)>> {
    if note.is_empty() {
        return Vec::new();
    }
    wrap_in(ctx, &note_pieces(note), width, |_| NOTE)
}

/// The card as it prints, with the guest's note above it: as on screen, About as far as it's
/// open, and the rows of buttons without them.
pub(super) fn printout(
    invite: &Invite,
    context: &Context,
    note: Option<&str>,
) -> crate::print::Invite {
    use crate::print::{InviteRow, InviteValue, Run};
    let runs = |pieces: Vec<Piece>| -> Vec<Run> {
        pieces
            .into_iter()
            .map(|piece| Run {
                text: piece.text,
                link: piece.link.map(PieceLink::into_string),
                bold: piece.bold,
            })
            .collect()
    };
    let rows = rows(invite, context)
        .into_iter()
        .map(|row| InviteRow {
            question: row.question,
            value: match row.value {
                Value::Text(lines) | Value::About(lines, _) => InviteValue::Text(
                    lines
                        .into_iter()
                        .map(|(indent, pieces)| (indent, runs(pieces)))
                        .collect(),
                ),
                Value::When(when) => InviteValue::When(when),
                Value::Rsvp(_) | Value::AddToCalendar(_) => InviteValue::Buttons,
                Value::Files(files) => InviteValue::Files(
                    files
                        .iter()
                        .map(|file| crate::print::File::of(&file.name, &file.mime))
                        .collect(),
                ),
            },
        })
        .collect();
    crate::print::Invite {
        note: note.map(|note| runs(note_pieces(note))),
        status: status(invite, context),
        rows,
    }
}

/// The height the note takes above the card in a column `width` wide.
pub(super) fn note_height(ctx: &egui::Context, note: &str, width: f32) -> f32 {
    note_lines(ctx, note, width).len() as f32 * NOTE.line_height + NOTE_BOTTOM
}

/// Paints the note from `top` (the body area's top) across the column and returns the link
/// clicked. The note is black in Snow (inherited from the pane) and white at .9 in Carbon.
pub(super) fn show_note(
    ui: &Ui,
    left: f32,
    top: f32,
    width: f32,
    note: &str,
    id: egui::Id,
) -> Option<String> {
    let palette = exo_ui::palette(ui.ctx());
    let mut clicked = None;
    let mut y = top;
    for line in note_lines(ui.ctx(), note, width) {
        let mut x = left;
        for (piece, advance) in line {
            // Links in `#54acdc`.
            let color = if piece.link.is_some() {
                palette.link_text_primary
            } else {
                palette.ink(1.0, 0.9)
            };
            NOTE.paint_line(ui.painter(), pos2(x, y), &piece.text, color, None);
            if let Some(piece_link) = piece.link {
                let url = piece_link.into_string();
                let rect = Rect::from_min_size(pos2(x, y), vec2(advance, NOTE.line_height));
                if interact_link(ui, rect, id.with(("note-link", x as i32, y as i32)), &url)
                    .clicked()
                {
                    clicked = Some(url);
                }
            }
            x += advance;
        }
        y += NOTE.line_height;
    }
    clicked
}

/// What [`show`] found: what was clicked, whether the pointer is on When's text, and where
/// Who's names went, for their tooltips.
#[derive(Default)]
pub(super) struct Shown {
    pub clicked: Option<CardAction>,
    pub on_when: bool,
    pub names: super::message::Named,
}

/// Paints the card at `top` (the body area's top) across the column. `focused` is the button with
/// the keyboard.
#[expect(
    clippy::too_many_arguments,
    reason = "one card, drawn from the message and the app's state"
)]
pub(super) fn show(
    ui: &Ui,
    left: f32,
    top: f32,
    width: f32,
    invite: &Invite,
    context: &Context,
    images: &Path,
    id: egui::Id,
    focused: Option<usize>,
) -> Shown {
    let palette = exo_ui::palette(ui.ctx());
    // The card's text: black at .75, white at .85.
    let text_color = palette.ink(0.75, 0.85);
    let painter = ui.painter();
    // `top: 1px`.
    let card = Rect::from_min_size(
        pos2(left, top + 1.0),
        vec2(width, height(ui, invite, context, width)),
    );
    painter.rect_filled(
        exo_ui::pixel_snap(card),
        CornerRadius::same(4),
        palette.fill_base_accent,
    );
    // The photo covers 640×176, centred (`background-size: cover; background-position-x: 50%`).
    let photo_rect = Rect::from_min_size(card.min, vec2(width, IMAGE_HEIGHT));
    if let Some(texture) = photo(ui.ctx(), images, image_kind(invite)) {
        let [w, h] = texture.size().map(|side| side as f32);
        let scale = (photo_rect.width() / w).max(photo_rect.height() / h);
        let (u, v) = (
            photo_rect.width() / (w * scale),
            photo_rect.height() / (h * scale),
        );
        let uv = Rect::from_min_max(pos2((1.0 - u) / 2.0, 0.0), pos2((1.0 + u) / 2.0, v));
        painter.add(
            RectShape::filled(
                exo_ui::pixel_snap(photo_rect),
                CornerRadius {
                    nw: 4,
                    ne: 4,
                    sw: 0,
                    se: 0,
                },
                Color32::WHITE,
            )
            .with_texture(texture.id(), uv),
        );
    }
    if let Some(status) = status(invite, context) {
        paint_status(ui, photo_rect, &status);
    }

    let value_left = card.left() + ROWS_SIDE + QUESTION + PROPERTY_PADDING;
    let value_width = value_width(width);
    let mut row_top = photo_rect.bottom() + ROWS_TOP;
    let mut shown = Shown::default();
    let pointer = ui
        .ctx()
        .pointer_hover_pos()
        .filter(|_| ui.ui_contains_pointer());
    let link = |rect: Rect, key: &str, href: &str| {
        interact_link(
            ui,
            rect,
            id.with(("link", key, rect.min.x as i32, rect.min.y as i32)),
            href,
        )
        .clicked()
    };
    for row in rows(invite, context) {
        if !row.question.is_empty() {
            BOLD.paint_line(
                painter,
                pos2(card.left() + ROWS_SIDE, row_top),
                row.question,
                text_color,
                None,
            );
        }
        let height = value_height(ui.ctx(), &row.value, value_width);
        match &row.value {
            Value::Text(lines) | Value::About(lines, _) => {
                let mut y = row_top;
                for (indent, pieces) in lines {
                    for line in wrap(ui.ctx(), pieces, value_width - indent) {
                        let mut x = value_left + indent;
                        for (piece, advance) in line {
                            let (style, color) = (
                                if piece.bold { BOLD } else { TEXT },
                                if piece.link.is_some() {
                                    palette.link_text_primary
                                } else {
                                    text_color
                                },
                            );
                            style.paint_line(painter, pos2(x, y), &piece.text, color, None);
                            if let Some(piece_link) = &piece.link
                                && link(
                                    Rect::from_min_size(
                                        pos2(x, y),
                                        vec2(advance, TEXT.line_height),
                                    ),
                                    row.question,
                                    piece_link.as_str(),
                                )
                            {
                                shown.clicked = Some(piece_link.to_action());
                            }
                            // A contact name's box is the 17px content area of its text, 3px
                            // down the line. The boxes cover the name's words and the spaces
                            // between them.
                            if let Some(contact) = piece.contact {
                                shown.names.push((
                                    Rect::from_min_size(pos2(x, y + 3.0), vec2(advance, 17.0)),
                                    contact,
                                ));
                            }
                            x += advance;
                        }
                        y += TEXT.line_height;
                    }
                }
                if let Value::About(_, Some(open)) = row.value {
                    // `Show more`: bold, `rgba(0, 0, 0, .35)`, Carbon white at .45.
                    let label = if open { "Show Less" } else { "Show more" };
                    let top = row_top + height - TEXT.line_height;
                    let width = BOLD.paint_line(
                        painter,
                        pos2(value_left, top),
                        label,
                        palette.ink(0.35, 0.45),
                        None,
                    );
                    if ui
                        .interact(
                            Rect::from_min_size(
                                pos2(value_left, top),
                                vec2(width, TEXT.line_height),
                            ),
                            id.with("show-more"),
                            Sense::click(),
                        )
                        .on_hover_cursor(egui::CursorIcon::PointingHand)
                        .clicked()
                    {
                        shown.clicked = Some(CardAction::ToggleAbout);
                    }
                }
            }
            Value::When(when) => {
                let mut y = row_top;
                for line in when_lines(ui.ctx(), when, value_width) {
                    let mut x = value_left;
                    for piece in line {
                        TEXT.paint_line(painter, pos2(x, y), &piece, text_color, None);
                        x += TEXT.width(ui.ctx(), &piece);
                    }
                    shown.on_when |= pointer.is_some_and(|at| {
                        Rect::from_x_y_ranges(value_left..=x, y..=y + TEXT.line_height).contains(at)
                    });
                    y += TEXT.line_height;
                }
            }
            Value::Rsvp(answer) => {
                let metrics = RsvpButtonMetrics {
                    font: BOLD,
                    height: BUTTON_HEIGHT,
                    padding_x: BUTTON_PADDING,
                    gap: BUTTON_GAP,
                    text_top: 4.0,
                    animated: false,
                };
                if let Some(response) = show_rsvp_buttons(
                    ui,
                    palette,
                    id,
                    pos2(card.left() + ROWS_SIDE + QUESTION, row_top - 3.0),
                    *answer,
                    focused,
                    metrics,
                ) {
                    shown.clicked = Some(CardAction::Respond(response));
                }
            }
            Value::AddToCalendar(selected) => {
                let metrics = RsvpButtonMetrics {
                    font: BOLD,
                    height: BUTTON_HEIGHT,
                    padding_x: BUTTON_PADDING,
                    gap: BUTTON_GAP,
                    text_top: 4.0,
                    animated: false,
                };
                let (_, clicked) = show_rsvp_button(
                    ui,
                    palette,
                    id.with("add"),
                    pos2(card.left() + ROWS_SIDE + QUESTION, row_top - 3.0),
                    Response::NeedsAction.button_label(),
                    *selected,
                    focused == Some(0),
                    metrics,
                );
                if clicked && !selected {
                    shown.clicked = Some(CardAction::Respond(Response::NeedsAction));
                }
            }
            Value::Files(files) => {
                let (size, gap) = file_grid(value_width);
                for (at, file) in files.iter().enumerate() {
                    let min = pos2(
                        value_left + (at % 3) as f32 * (size.x + gap),
                        row_top + (at / 3) as f32 * (size.y + gap),
                    );
                    let rect = Rect::from_min_size(min, size);
                    let response = ui
                        .interact(rect, id.with(("file", at)), Sense::click())
                        .on_hover_cursor(egui::CursorIcon::PointingHand);
                    let hover = exo_ui::motion::transition(
                        ui.ctx(),
                        response.id.with("hover"),
                        response.hovered(),
                        0.25,
                        exo_ui::motion::CubicBezier::EASE,
                    );
                    exo_ui::paint_attachment_card(ui, rect, &file.name, &file.mime, hover);
                    if response.clicked() {
                        shown.clicked = Some(CardAction::Open(file.url.clone()));
                    }
                }
            }
        }
        row_top += height + ROW_GAP;
    }
    shown
}

/// The status over the photo: white at .95 with a hairline border, the
/// text bold at .75 in both themes.
fn paint_status(ui: &Ui, photo: Rect, status: &str) {
    let painter = ui.painter();
    let max = photo.width() - 24.0 - 2.0 * STATUS_PADDING_X - 2.0;
    let lines = BOLD.wrap(ui.ctx(), status, max);
    let width = lines
        .iter()
        .map(|line| BOLD.width(ui.ctx(), line))
        .fold(0.0, f32::max);
    let rect = Rect::from_min_size(
        photo.min + vec2(STATUS_LEFT, STATUS_TOP),
        vec2(
            width + 2.0 * STATUS_PADDING_X + 2.0,
            1.0 + STATUS_PADDING_TOP + lines.len() as f32 * BOLD.line_height + 1.0,
        ),
    );
    // The background runs under the border (`background-clip: border-box`).
    let snapped = exo_ui::pixel_snap(rect);
    painter.rect_filled(
        snapped,
        CornerRadius::same(4),
        Color32::from_white_alpha(242),
    );
    painter.rect_stroke(
        snapped,
        CornerRadius::same(4),
        Stroke::new(1.0, Color32::from_black_alpha(20)),
        egui::StrokeKind::Inside,
    );
    for (at, line) in lines.iter().enumerate() {
        BOLD.paint_line(
            painter,
            rect.min
                + vec2(
                    1.0 + STATUS_PADDING_X,
                    1.0 + STATUS_PADDING_TOP + at as f32 * BOLD.line_height,
                ),
            line,
            Color32::from_black_alpha(191),
            None,
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const ICS: &str = "BEGIN:VCALENDAR\r\nMETHOD:REQUEST\r\nBEGIN:VEVENT\r\nDTSTART:20261125T140000Z\r\nDTEND:20261125T144500Z\r\nORGANIZER;CN=Grace Liu:mailto:grace@juniperpartners.vc\r\nATTENDEE;PARTSTAT=ACCEPTED;CN=Grace Liu:mailto:grace@juniperpartners.vc\r\nATTENDEE;PARTSTAT=NEEDS-ACTION;CN=Maya Okafor:mailto:maya@brightmoor.co\r\nLOCATION:Harborlight Coffee\\, 5th Ave\r\nSUMMARY:Coffee with Grace\r\n DESCRIPTION-folded\r\nEND:VEVENT\r\nEND:VCALENDAR\r\n";

    fn context<'a>(message: &'a MessageDetail, now: &'a Zoned) -> Context<'a> {
        Context {
            me: "maya@brightmoor.co",
            user: "maya@brightmoor.co",
            now,
            message,
            calendar: None,
            about_open: false,
        }
    }

    fn message(subject: &str) -> MessageDetail {
        MessageDetail {
            id: "m".into(),
            labels: Vec::new(),
            date: Timestamp::UNIX_EPOCH,
            from: None,
            to: Vec::new(),
            cc: Vec::new(),
            bcc: Vec::new(),
            reply_to: Vec::new(),
            subject: subject.to_owned(),
            snippet: String::new(),
            rfc822_id: None,
            in_reply_to: None,
            references: Vec::new(),
            list_id: None,
            list_unsubscribe: None,
            list_unsubscribe_post: None,
            headers: Vec::new(),
            body: exo_store::Body::default(),
            attachments: Vec::new(),
            failure: None,
            scheduled: None,
            sending: false,
        }
    }

    #[test]
    fn an_invite_is_to_the_mail_account_or_else_the_calendar_account_on_it() {
        let mut invite = parse(ICS).expect("an event");
        let others = ["maya.lin@example.com".to_owned()];
        assert_eq!(
            invite.calendar_account("maya@brightmoor.co", &others),
            "maya@brightmoor.co"
        );
        let mine = invite
            .attendees
            .iter_mut()
            .find(|(_, email, _)| email == "maya@brightmoor.co")
            .expect("on it");
        mine.1 = "Maya.Lin@example.com".into();
        assert_eq!(
            invite.calendar_account("maya@brightmoor.co", &others),
            "Maya.Lin@example.com",
            "as the invite spells it"
        );
        assert_eq!(
            invite.calendar_account("maya@brightmoor.co", &[]),
            "maya@brightmoor.co",
            "none of mine: the mail account"
        );
    }

    #[test]
    fn reads_an_invitation() {
        let invite = parse(ICS).expect("an event");
        assert!(invite.request);
        assert_eq!(
            invite.summary, "Coffee with GraceDESCRIPTION-folded",
            "folded lines join"
        );
        assert_eq!(invite.location, "Harborlight Coffee, 5th Ave");
        assert_eq!(invite.attendees.len(), 2);
        assert_eq!(invite.attendees[1].2, Response::NeedsAction);
        let now: Zoned = "2026-11-17T09:00:00[America/New_York]"
            .parse()
            .expect("valid");
        assert_eq!(
            dates::calendar_range(invite.start, invite.end, &now),
            "Wednesday, November 25th, 2026 at 9:00 AM – 9:45 AM EST"
        );
        assert_eq!(image_kind(&invite), "coffee");
        assert!(
            invite.can_respond("maya@brightmoor.co")
                && !invite.can_add_to_calendar("maya@brightmoor.co")
        );
        assert!(
            !invite.can_respond("ana@example.com") && invite.can_add_to_calendar("ana@example.com"),
            "an invite I'm not on adds to my calendar"
        );
    }

    #[test]
    fn photos_follow_the_title() {
        assert_eq!(kind("Weekly planning", 3), Some("meeting"));
        assert_eq!(kind("Quick call", 2), Some("call"));
        assert_eq!(kind("Video call with Ana", 2), None);
        assert_eq!(kind("Maya <> Yusuf", 2), Some("1on1"));
        assert_eq!(kind("Board prep", 4), None);
    }

    #[test]
    fn every_kind_has_its_photo() {
        let titles = [
            "Swim",
            "Birthday",
            "Standup",
            "Quick call",
            "Coffee",
            "Dinner",
            "Drinks",
            "Gym",
            "Flight",
            "Hike",
            "Brunch",
            "Lunch",
            "Party",
            "Matcha",
            "1:1",
        ];
        let kinds: std::collections::HashSet<&str> = titles
            .iter()
            .map(|title| kind(title, 3).expect("a kind"))
            .chain(["default"])
            .collect();
        assert_eq!(kinds.len(), 16, "one title for each kind");
        let images = crate::assets_dir().join("images");
        for kind in kinds {
            assert!(
                images.join(format!("invite-{kind}.jpg")).is_file(),
                "no photo for {kind}"
            );
        }
    }

    #[test]
    fn the_status_says_what_the_notification_is() {
        let now: Zoned = "2026-11-17T09:00:00[America/New_York]"
            .parse()
            .expect("valid");
        let invite = parse(&ICS.replace("METHOD:REQUEST", "METHOD:CANCEL")).expect("an event");
        let plain = message("Invitation: Coffee");
        assert_eq!(
            status(&invite, &context(&plain, &now)).as_deref(),
            Some("Event has been canceled")
        );
        assert!(
            !invite.can_respond("maya@brightmoor.co")
                && !invite.can_add_to_calendar("maya@brightmoor.co")
        );

        let invite = parse(ICS).expect("an event");
        let updated = message("Updated Invitation: Coffee");
        assert_eq!(
            status(&invite, &context(&updated, &now)).as_deref(),
            Some("Event has been changed")
        );
        let calendar = exo_mail::CalendarData {
            gcal: true,
            updated_fields: Some("time".into()),
            ..Default::default()
        };
        let with_fields = Context {
            calendar: Some(&calendar),
            ..context(&updated, &now)
        };
        assert_eq!(
            status(&invite, &with_fields).as_deref(),
            Some("Event has been changed - time")
        );
        // What changed alone isn't an update.
        assert_eq!(
            status(
                &invite,
                &Context {
                    calendar: Some(&calendar),
                    ..context(&plain, &now)
                }
            ),
            None
        );
        assert_eq!(status(&invite, &context(&plain, &now)), None);

        let reply = "BEGIN:VCALENDAR\r\nMETHOD:REPLY\r\nBEGIN:VEVENT\r\nDTSTART:20261125T140000Z\r\nDTEND:20261125T144500Z\r\nATTENDEE;PARTSTAT=TENTATIVE;CN=Grace Liu:mailto:grace@juniperpartners.vc\r\nUID:x\r\nEND:VEVENT\r\nEND:VCALENDAR\r\n";
        let invite = parse(reply).expect("an event");
        assert_eq!(
            status(&invite, &context(&plain, &now)).as_deref(),
            Some("Grace has responded maybe")
        );
        assert!(
            !invite.can_add_to_calendar("maya@brightmoor.co"),
            "an answer isn't added"
        );
        let counter = parse(&reply.replace("METHOD:REPLY", "METHOD:COUNTER")).expect("an event");
        assert_eq!(
            status(&counter, &context(&plain, &now)).as_deref(),
            Some("Grace has proposed a new time")
        );
    }

    #[test]
    fn a_guests_note_is_their_response_comment_else_the_events_comment() {
        let reply = "BEGIN:VCALENDAR\r\nMETHOD:REPLY\r\nBEGIN:VEVENT\r\nDTSTART:20261125T140000Z\r\nDTEND:20261125T144500Z\r\nCOMMENT:Agenda\\, attached\r\nATTENDEE;CN=Grace Liu;X-RESPONSE-COMMENT=\"Late: ^'5 min^' ^^_^^ \";PARTSTAT=ACCEPTED:mailto:grace@juniperpartners.vc\r\nUID:x\r\nEND:VEVENT\r\nEND:VCALENDAR\r\n";
        let invite = parse(reply).expect("an event");
        assert_eq!(
            invite.note, "Late: \"5 min\" ^_^",
            "quoted, with its `:` and RFC 6868's escapes"
        );
        assert_eq!(
            invite.attendees[0],
            (
                "Grace Liu".to_owned(),
                "grace@juniperpartners.vc".to_owned(),
                Response::Accepted
            )
        );
        let without = parse(&reply.replace(";X-RESPONSE-COMMENT=\"Late: ^'5 min^' ^^_^^ \"", ""))
            .expect("an event");
        assert_eq!(without.note, "Agenda, attached");
        // With more than one attendee, only the event's comment counts.
        let two = parse(&reply.replace(
            "UID:x",
            "ATTENDEE;CN=Maya:mailto:maya@brightmoor.co\r\nUID:x",
        ))
        .expect("an event");
        assert_eq!(two.note, "Agenda, attached");

        // The notification's own comment wins, read as HTML's text, its spaces collapsed.
        let calendar = exo_mail::CalendarData {
            response_comment: Some("See  you\nthere &amp; then".into()),
            ..Default::default()
        };
        assert_eq!(
            note(&invite, Some(&calendar)).as_deref(),
            Some("See you there & then")
        );
        assert_eq!(note(&invite, None).as_deref(), Some("Late: \"5 min\" ^_^"));
        assert_eq!(note(&parse(ICS).expect("an event"), None), None);
    }

    #[test]
    fn finds_the_video_call_and_links_in_the_description() {
        let description = "Agenda <https://docs.example/a>\n\nJoin with Google Meet: https://meet.google.com/abc-defg-hij\nOr dial: +1 555";
        let (text, meeting) = extract_meeting(description);
        assert_eq!(
            meeting,
            Some(Meeting {
                label: "Meet",
                url: "https://meet.google.com/abc-defg-hij".into()
            })
        );
        assert!(
            text.starts_with(
                "Agenda  <a href=\"https://docs.example/a\">https://docs.example/a</a>"
            ),
            "{text}"
        );
        let teams = "________________\nMicrosoft Teams meeting\nJoin on your computer\nClick here to join the meeting<https://teams.microsoft.com/l/meetup-join/19%3a>\n________________";
        let (text, meeting) = extract_meeting(teams);
        assert_eq!(meeting.map(|meeting| meeting.label), Some("Teams"));
        assert!(text.contains("<a href=\"https://teams.microsoft.com/l/meetup-join/19%3a\">Click here to join the meeting</a>"), "{text}");
    }

    #[test]
    fn where_is_a_map_a_number_or_its_links() {
        let mut invite = parse(ICS).expect("an event");
        let link = |invite: &Invite| {
            location(invite).map(|pieces| {
                pieces
                    .into_iter()
                    .filter_map(|piece| piece.link)
                    .collect::<Vec<_>>()
            })
        };
        assert_eq!(
            link(&invite),
            Some(vec![PieceLink::Web(
                "https://maps.google.com/maps?q=Harborlight%20Coffee%2C%205th%20Ave".to_owned()
            )])
        );
        invite.location = "+1 (555) 010-2030".into();
        assert_eq!(
            link(&invite),
            Some(vec![PieceLink::Dial("tel:+15550102030".to_owned())])
        );
        invite.location = "Zoom: https://zoom.us/j/1".into();
        assert_eq!(
            link(&invite),
            Some(vec![PieceLink::Web("https://zoom.us/j/1".to_owned())])
        );
        invite.location = "Microsoft Teams Meeting".into();
        assert_eq!(link(&invite), Some(Vec::new()));
        invite.meeting = Some(Meeting {
            label: "Teams",
            url: "https://teams.microsoft.com/l/meetup-join/x".into(),
        });
        assert_eq!(link(&invite), None, "the video row has it");
    }

    #[test]
    fn whos_names_carry_their_contacts_for_tooltips() {
        let now: Zoned = "2026-11-17T09:00:00[America/New_York]"
            .parse()
            .expect("valid");
        let invite = parse(ICS).expect("an event");
        let plain = message("Invitation: Coffee");
        let who = rows(&invite, &context(&plain, &now))
            .into_iter()
            .find_map(|row| match row.value {
                Value::Text(lines) if row.question == "Who" => Some(
                    lines
                        .into_iter()
                        .flat_map(|(_, pieces)| pieces)
                        .collect::<Vec<_>>(),
                ),
                _ => None,
            });
        let who = who.expect("a Who row");
        let names: Vec<_> = who
            .iter()
            .map(|piece| {
                (
                    piece.text.as_str(),
                    piece.contact.as_ref().map(|contact| contact.email.as_str()),
                )
            })
            .collect();
        assert_eq!(
            names,
            [
                ("Grace", Some("grace@juniperpartners.vc")),
                (", ", None),
                ("Me", Some("maya@brightmoor.co"))
            ]
        );
        assert_eq!(
            who[0]
                .contact
                .as_ref()
                .and_then(|contact| contact.name.as_deref()),
            Some("Grace Liu"),
            "the tooltip's full name"
        );
    }

    #[test]
    fn about_shows_eight_lines_until_show_more() {
        let now: Zoned = "2026-11-17T09:00:00[America/New_York]"
            .parse()
            .expect("valid");
        let mut invite = parse(ICS).expect("an event");
        invite.description = (1..=10)
            .map(|n| format!("Line {n}"))
            .collect::<Vec<_>>()
            .join("\n");
        let plain = message("Invitation: Coffee");
        let about = |open: bool| {
            rows(
                &invite,
                &Context {
                    about_open: open,
                    ..context(&plain, &now)
                },
            )
            .into_iter()
            .find_map(|row| match row.value {
                Value::About(lines, more) => Some((lines.len(), more)),
                _ => None,
            })
        };
        assert_eq!(about(false), Some((8, Some(false))));
        assert_eq!(about(true), Some((10, Some(true))));
    }

    #[test]
    fn prints_its_rows_without_the_buttons() {
        use crate::print::{InviteValue, Run};
        let invite = parse(ICS).expect("an event");
        let now: Zoned = "2026-11-17T09:00:00[America/New_York]"
            .parse()
            .expect("valid");
        let message = message("Invitation: Coffee with Grace");
        let printed = printout(&invite, &context(&message, &now), Some("See you there"));
        assert_eq!(
            printed.note,
            Some(vec![Run {
                text: "See you there".into(),
                ..Run::default()
            }])
        );
        assert_eq!(printed.status, None);
        let questions: Vec<&str> = printed.rows.iter().map(|row| row.question).collect();
        assert_eq!(questions, ["RSVP", "When", "Where", "Who"]);
        assert_eq!(printed.rows[0].value, InviteValue::Buttons);
        assert_eq!(
            printed.rows[1].value,
            InviteValue::When("Wednesday, November 25th, 2026 at 9:00 AM – 9:45 AM EST".into())
        );
    }

    #[test]
    fn valarm_nested_components_never_clobber_vevent_properties_or_meeting_links() {
        let raw = concat!(
            "BEGIN:VCALENDAR\r\n",
            "METHOD:REQUEST\r\n",
            "BEGIN:VEVENT\r\n",
            "UID:valarm-1@brightmoor.co\r\n",
            "DTSTART:20261125T140000Z\r\n",
            "DTEND:20261125T150000Z\r\n",
            "SUMMARY:Design review\r\n",
            "DESCRIPTION:Docs at C:\\\\notes\\\\v1\\nJoin with Google Meet: https://meet.google.com/abc-defg-hij\r\n",
            "ATTENDEE;PARTSTAT=NEEDS-ACTION;CN=Maya Okafor:mailto:maya@brightmoor.co\r\n",
            "BEGIN:VALARM\r\n",
            "ACTION:EMAIL\r\n",
            "DESCRIPTION:Reminder\r\n",
            "SUMMARY:Alarm summary\r\n",
            "ATTENDEE:mailto:alarm@brightmoor.co\r\n",
            "END:VALARM\r\n",
            "END:VEVENT\r\n",
            "END:VCALENDAR\r\n",
        );
        let invite = parse(raw).expect("parses");
        assert_eq!(invite.summary, "Design review");
        assert!(
            invite.description.starts_with("Docs at C:\\notes\\v1\n"),
            "single-pass unescape keeps literal \\n while VALARM DESCRIPTION:Reminder is ignored: {}",
            invite.description
        );
        assert_eq!(
            invite.meeting,
            Some(Meeting {
                label: "Meet",
                url: "https://meet.google.com/abc-defg-hij".into(),
            })
        );
        assert_eq!(invite.attendees.len(), 1);
        assert_eq!(invite.attendees[0].1, "maya@brightmoor.co");
    }

    #[test]
    fn duration_and_missing_dtend_resolve_without_epoch_fallback() {
        let with_duration = concat!(
            "BEGIN:VCALENDAR\r\n",
            "BEGIN:VEVENT\r\n",
            "UID:dur-1\r\n",
            "DTSTART:20261125T140000Z\r\n",
            "DURATION:PT1H30M\r\n",
            "END:VEVENT\r\n",
            "END:VCALENDAR\r\n",
        );
        let timed = parse(with_duration).expect("timed duration");
        assert!(!timed.all_day);
        assert_eq!(
            timed.end.duration_since(timed.start),
            SignedDuration::from_mins(90)
        );

        let timed_no_end = concat!(
            "BEGIN:VCALENDAR\r\n",
            "BEGIN:VEVENT\r\n",
            "UID:noend-1\r\n",
            "DTSTART:20261125T140000Z\r\n",
            "END:VEVENT\r\n",
            "END:VCALENDAR\r\n",
        );
        let instant = parse(timed_no_end).expect("timed without DTEND");
        assert_eq!(instant.end, instant.start);
        assert_ne!(instant.end, Timestamp::UNIX_EPOCH);

        let all_day_no_end = concat!(
            "BEGIN:VCALENDAR\r\n",
            "BEGIN:VEVENT\r\n",
            "UID:date-noend\r\n",
            "DTSTART;VALUE=DATE:20261207\r\n",
            "END:VEVENT\r\n",
            "END:VCALENDAR\r\n",
        );
        let single_day = parse(all_day_no_end).expect("date without DTEND");
        assert!(single_day.all_day);
        assert_eq!(
            single_day.dates,
            Some((Date::constant(2026, 12, 7), Date::constant(2026, 12, 8)))
        );
        for zone_name in ["America/Los_Angeles", "Europe/London", "Pacific/Auckland"] {
            let tz = TimeZone::get(zone_name).expect("zone");
            let (start, end) = single_day.zoned(&tz);
            assert_eq!(
                (start.date(), end.date()),
                (Date::constant(2026, 12, 7), Date::constant(2026, 12, 8)),
                "civil dates preserved in {zone_name}"
            );
            assert_eq!(
                dates::invite_range(&start, &end, single_day.all_day, false),
                "Monday, December 7th 2026 (all day)"
            );
        }

        let all_day_duration = concat!(
            "BEGIN:VCALENDAR\r\n",
            "BEGIN:VEVENT\r\n",
            "UID:date-dur\r\n",
            "DTSTART;VALUE=DATE:20261209\r\n",
            "DURATION:P3D\r\n",
            "END:VEVENT\r\n",
            "END:VCALENDAR\r\n",
        );
        let multi_day = parse(all_day_duration).expect("date with P3D");
        assert_eq!(
            multi_day.dates,
            Some((Date::constant(2026, 12, 9), Date::constant(2026, 12, 12)))
        );
    }

    #[test]
    fn windows_exchange_tzids_map_to_valid_iana_zones() {
        for (windows, iana) in gmail::ics::WINDOWS_ZONES {
            assert!(
                TimeZone::get(iana).is_ok(),
                "unrecognized IANA zone {iana} for {windows}"
            );
            assert!(resolve_timezone(windows).is_some(), "resolves {windows}");
            assert_eq!(gmail::ics::windows_to_iana(windows), Some(*iana));
        }
        let outlook = concat!(
            "BEGIN:VCALENDAR\r\n",
            "METHOD:REQUEST\r\n",
            "BEGIN:VEVENT\r\n",
            "UID:win-tz\r\n",
            "DTSTART;TZID=\"Eastern Standard Time\":20261118T100000\r\n",
            "DTEND;TZID=Pacific Standard Time:20261118T080000\r\n",
            "END:VEVENT\r\n",
            "END:VCALENDAR\r\n",
        );
        let invite = parse(outlook).expect("parses Windows TZID");
        assert_eq!(invite.start.to_string(), "2026-11-18T15:00:00Z");
        assert_eq!(invite.end.to_string(), "2026-11-18T16:00:00Z");
    }

    #[test]
    fn ics_attach_rejects_non_web_urls_and_file_cards_never_emit_dial() {
        let with_attach = concat!(
            "BEGIN:VCALENDAR\r\n",
            "METHOD:REQUEST\r\n",
            "BEGIN:VEVENT\r\n",
            "UID:attach-check\r\n",
            "DTSTART:20261125T140000Z\r\n",
            "DTEND:20261125T144500Z\r\n",
            "ATTACH;FILENAME=call.pdf;FMTTYPE=application/pdf:tel:+15550102030\r\n",
            "ATTACH;FILENAME=local.txt;FMTTYPE=text/plain:file:///etc/passwd\r\n",
            "ATTACH;FILENAME=script.html;FMTTYPE=text/html:javascript:alert(1)\r\n",
            "ATTACH;FILENAME=agenda.pdf;FMTTYPE=application/pdf:https://drive.example/agenda.pdf\r\n",
            "END:VEVENT\r\n",
            "END:VCALENDAR\r\n",
        );
        let parsed = parse(with_attach).expect("parses");
        assert_eq!(
            parsed.files,
            vec![File {
                name: "agenda.pdf".into(),
                mime: "application/pdf".into(),
                url: "https://drive.example/agenda.pdf".into(),
            }],
            "non-http(s) ATTACH URLs including tel: are rejected at parse time"
        );

        // Even if a File carries a `tel:` URL directly, clicking it emits `CardAction::Open`
        // (never `CardAction::Dial`), whereas clicking a phone number in Where emits `Dial`.
        let ctx = egui::Context::default();
        exo_ui::install(&ctx).expect("fonts load");
        let now: Zoned = "2026-11-17T09:00:00[America/New_York]"
            .parse()
            .expect("valid");
        let plain = message("Invitation: Phone check");
        let images = crate::assets_dir().join("images");

        let mut invite = parsed;
        invite.location = "+1 (555) 010-2030".into();
        invite.files = vec![File {
            name: "spoof.pdf".into(),
            mime: "application/pdf".into(),
            url: "tel:+15550102030".into(),
        }];

        let mut where_pos = None;
        let mut file_pos = None;
        let mut prime = ctx.run_ui(
            egui::RawInput {
                screen_rect: Some(Rect::from_min_size(pos2(0.0, 0.0), vec2(800.0, 800.0))),
                ..Default::default()
            },
            |ui| {
                let mut row_top = 1.0 + IMAGE_HEIGHT + ROWS_TOP;
                let vw = value_width(640.0);
                let value_left = ROWS_SIDE + QUESTION + PROPERTY_PADDING;
                for row in rows(&invite, &context(&plain, &now)) {
                    let h = value_height(ui.ctx(), &row.value, vw);
                    if row.question == "Where" {
                        where_pos = Some(pos2(value_left + 20.0, row_top + TEXT.line_height / 2.0));
                    } else if row.question == "Files" {
                        file_pos = Some(pos2(value_left + 20.0, row_top + 20.0));
                    }
                    row_top += h + ROW_GAP;
                }
                let _ = show(
                    ui,
                    0.0,
                    0.0,
                    640.0,
                    &invite,
                    &context(&plain, &now),
                    &images,
                    egui::Id::new("test-invite"),
                    None,
                );
            },
        );
        prime.textures_delta.clear();

        let click_at = |target: egui::Pos2| -> Option<CardAction> {
            let mut action = None;
            let frame = |events: Vec<egui::Event>| {
                let mut clicked = None;
                let mut raw = egui::RawInput {
                    screen_rect: Some(Rect::from_min_size(pos2(0.0, 0.0), vec2(800.0, 800.0))),
                    ..Default::default()
                };
                raw.events = events;
                let mut out = ctx.run_ui(raw, |ui| {
                    let shown = show(
                        ui,
                        0.0,
                        0.0,
                        640.0,
                        &invite,
                        &context(&plain, &now),
                        &images,
                        egui::Id::new("test-invite"),
                        None,
                    );
                    clicked = shown.clicked;
                });
                out.textures_delta.clear();
                clicked
            };
            let _ = frame(vec![egui::Event::PointerMoved(target)]);
            let _ = frame(vec![egui::Event::PointerButton {
                pos: target,
                button: egui::PointerButton::Primary,
                pressed: true,
                modifiers: egui::Modifiers::NONE,
            }]);
            action = action.or_else(|| {
                frame(vec![egui::Event::PointerButton {
                    pos: target,
                    button: egui::PointerButton::Primary,
                    pressed: false,
                    modifiers: egui::Modifiers::NONE,
                }])
            });
            action
        };

        assert_eq!(
            click_at(where_pos.expect("Where row present")),
            Some(CardAction::Dial("tel:+15550102030".into())),
            "phone link in Where emits CardAction::Dial"
        );
        assert_eq!(
            click_at(file_pos.expect("Files row present")),
            Some(CardAction::Open("tel:+15550102030".into())),
            "file attachment card emits CardAction::Open, never CardAction::Dial"
        );
    }

    #[test]
    fn invite_photos_cache_in_egui_context_across_calls() {
        let dir = std::env::temp_dir().join(format!(
            "exo-invite-photo-cache-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("temp dir");
        let src = crate::assets_dir().join("images/invite-coffee.jpg");
        let dst = dir.join("invite-coffee.jpg");
        std::fs::copy(&src, &dst).expect("copy invite photo");

        let ctx = egui::Context::default();
        let first = photo(&ctx, &dir, "coffee").expect("decodes and caches photo");
        std::fs::remove_file(&dst).expect("remove source file after first load");
        let second = photo(&ctx, &dir, "coffee").expect("reuses cached texture without disk read");
        assert_eq!(first.id(), second.id());

        assert!(photo(&ctx, &dir, "missing").is_none());
        let missing_file = dir.join("invite-missing.jpg");
        std::fs::copy(&src, &missing_file).expect("copy after negative cache");
        assert!(
            photo(&ctx, &dir, "missing").is_none(),
            "missing photo result stays cached so frames do not re-hit disk"
        );

        let _ = std::fs::remove_dir_all(&dir);
    }
}

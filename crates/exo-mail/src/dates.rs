//! Dates in message text, marked for the "Create Event" hover, and what suggests the thread's
//! calendar tip (`Tip: Hit b to create a calendar event!`): the message's own dates and a phrase
//! that proposes a time.

use std::sync::LazyLock;

use regex::Regex;

use crate::dom::{self, NodeRef};

/// What each capture group of [`DATE`] means.
const GROUPS: &[Option<Group>] = {
    use Group::*;
    &[
        None,
        Some(IsoTimestamp),
        Some(Suffix),
        Some(Relative),
        Some(Dow),
        Some(LongDate),
        Some(Suffix),
        Some(ShortDate),
        Some(Dom),
        Some(Month),
        Some(Year),
        Some(Suffix),
        Some(Relative),
        Some(Dow),
        Some(Other),
        Some(Dom),
        Some(Other),
        Some(Month),
        Some(Relative),
        Some(Dow),
        Some(Month),
        Some(Dom),
        Some(Other),
        Some(Month),
        Some(Dom),
        Some(Suffix),
        Some(Month),
        Some(Dom),
        Some(Year),
        Some(Suffix),
        Some(Dom),
        Some(Month),
        Some(Dow),
        Some(Suffix),
        Some(Dom),
        Some(Month),
        Some(Year),
        Some(Dom),
        Some(Other),
        Some(Month),
        Some(Year),
        Some(Suffix),
        Some(Dom),
        Some(Month),
        Some(Year),
        Some(Suffix),
        Some(LongDate),
        Some(Suffix),
        Some(ShortDate),
        Some(Relative),
        Some(Dow),
        Some(Other),
        Some(Other),
        Some(Other),
        Some(Other),
        Some(Business),
        Some(Suffix),
    ]
};

#[derive(Clone, Copy, PartialEq, Eq)]
enum Group {
    IsoTimestamp,
    Relative,
    Dow,
    LongDate,
    ShortDate,
    Dom,
    Month,
    Year,
    /// `eod`, `eow`…, which aren't used to filter.
    Business,
    /// Joiners, `today` and parts of day, which aren't used to filter.
    Other,
    /// The non-word delimiter matched after a candidate date in place of a negative lookahead.
    Suffix,
}

static DATE: LazyLock<Regex> = LazyLock::new(|| {
    let r2 = r"[0-9]{1,2}";
    let r3 = r"[0-9]{2,4}";
    let r4 = r"(?:(on|this|next|last)\s+)";
    let r5 = r"(today|tonight|tomorrow)";
    let r6 = r"(?:\s?(the|of)\s+)";
    let r8 = r"(monday|tuesday|wednesday|thursday|friday|saturday|sunday|mon|tues?|weds?|thur?|thurs|fri|sat|sun)";
    let r7 = r"(morning|afternoon|evening|night)";
    let r9 = r"(jan\.?|january|feb\.?|february|mar\.?|march|apr\.?|april|may\.?|jun\.?|june|jul\.?|july|aug\.?|august|sept?\.?|september|oct\.?|october|nov\.?|november|dec\.?|december)";
    let ne = r"([0-9]{1,2})(?:rd|st|nd|th|ʳᵈ|ˢᵗ|ⁿᵈ|ᵗʰ)?";
    let nt = r"[./\\]";
    let nr = r"[./\\-]";
    let nn = r"[./\\\-\s,]*";
    let no = format!(r"({r2}{nt}{r2}|{r3}{nt}{r2})");
    let ni = format!(r"({r3}{nr}{r2}{nr}{r2}|{r2}{nr}{r2}{nr}{r3})");
    let na = format!(r"({r2}){nn}{r9}{nn}({r3})?");
    let iso = r"([0-9]{4}-[0-9]{2}-[0-9]{2}T[0-9]{2}:[0-9]{2}(?::[0-9]{2}(?:\.[0-9]+)?)?(?:Z|[+-][0-9]{2}:[0-9]{2}))";
    let s_std = r"(\z|[^A-Za-z0-9_-])";
    let s_nocolon = r"(\z|[^A-Za-z0-9_:-])";
    let s_ni = r"(\z|[./\\](?:\z|[^0-9])|[^A-Za-z0-9_./\\-])";
    Regex::new(&format!(
        r"(?i)(?:{iso}{s_std}|{r4}?{r8}[,\s]+(?:{ni}{s_ni}|(?:{no}|{na}){s_std})|(?:{r4}?{r8}[,\s]+{r6}?{ne}{r6}?{r9}?|{r4}?{r8}[,\s]+{r9}[,\s]+{ne}|{r5}[,\s]+{r9}\s+{ne}){s_std}|{r9}\s+{ne}(?:[,\s])*({r3})?{s_nocolon}|{ne}\s+{r9}\s+{r8}{s_std}|(?:{ne}\s+{r9}(?:[,\s])*({r3})?|{ne}{r6}{r9}(?:[,\s]*({r3}))?){s_nocolon}|{na}{s_std}|{ni}{s_ni}|(?:{no}|{r4}?{r8}|this\s+{r7}|{r5}\s+{r7}|{r5}|(eod|eob|eow|eom|eoq|eoy)){s_std})"
    ))
    .expect("valid regex")
});

/// Suffixes after a short date (`3/4`, `11/25`) that mark it as a time, unit or ratio instead.
static BAD_SHORT_SUFFIX: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"\A(?i)\s*(?:[ap]m|day|week|mon|year|:(?:\z|\S))").expect("valid regex")
});

/// Words near a date that make a bare one more likely to be meant.
static CONTEXT: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(
        r"(?i)(^|\s)(day|week|month|year|on|at|free|available|[0-9]{1,2}(?:am|pm|:[0-9]{2}))(?-u:\b)",
    )
    .expect("valid regex")
});
/// Reply headers, whose dates aren't plans.
static HEADER: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?i)(On[^\n]{10,100}wrote:|---\s*Forwarded message[^\n]*|Sent:[\s0-9A-Za-z_]{3})")
        .expect("valid regex")
});

/// A byte range of the searched text.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Span {
    start: usize,
    end: usize,
}

impl Span {
    /// Whether the span overlaps or abuts any of `spans`, which are in order, ends increasing.
    fn touches(self, spans: &[Span]) -> bool {
        let first = spans.partition_point(|span| span.end < self.start);
        spans.get(first).is_some_and(|span| span.start <= self.end)
    }
}

/// The parts of a detected date that say which day it is, as the date regex's named groups
/// capture them.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct DateParts {
    /// `2026-11-25T14:00Z`.
    pub iso_timestamp: Option<String>,
    /// `11/25`, `25.11`.
    pub short_date: Option<String>,
    /// `2026-11-25`, `11/25/2026`.
    pub long_date: Option<String>,
    /// `Nov`, `november`.
    pub month: Option<String>,
    /// The day of the month's digits.
    pub dom: Option<String>,
    pub year: Option<String>,
    /// `on`, `this`, `next`, `last`.
    pub relative: Option<String>,
    /// `Friday`, `fri`.
    pub dow: Option<String>,
    /// `eod`, `eob`, `eow`, `eom`, `eoq`, `eoy`.
    pub business: Option<String>,
}

/// The parts of the first date in `text`, a detected date's own text. Unlike `find`, it needs no
/// context around the date, and it only drops a short date with neither a slash nor a weekday.
pub fn date_parts(text: &str) -> Option<DateParts> {
    let (_, captures) = next_date(text, 0)?;
    let group = |wanted: Group| last_group(&captures, wanted).map(str::to_owned);
    let parts = DateParts {
        iso_timestamp: group(Group::IsoTimestamp),
        short_date: group(Group::ShortDate),
        long_date: group(Group::LongDate),
        month: group(Group::Month),
        dom: group(Group::Dom),
        year: group(Group::Year),
        relative: group(Group::Relative),
        dow: group(Group::Dow),
        business: group(Group::Business),
    };
    // A short date without a slash and without a weekday is still dropped.
    let dropped = parts.iso_timestamp.is_none()
        && parts.long_date.is_none()
        && parts
            .short_date
            .as_ref()
            .is_some_and(|short| parts.dow.is_none() && !short.contains('/'));
    (!dropped).then_some(parts)
}

/// The capture of group `wanted`. A later group of the same name wins.
fn last_group<'t>(captures: &regex::Captures<'t>, wanted: Group) -> Option<&'t str> {
    GROUPS
        .iter()
        .enumerate()
        .rev()
        .filter(|(_, g)| **g == Some(wanted))
        .filter_map(|(i, _)| captures.get(i))
        .map(|m| m.as_str())
        .find(|s| !s.is_empty())
}

fn suffix_len(captures: &regex::Captures<'_>) -> usize {
    GROUPS
        .iter()
        .enumerate()
        .filter(|(_, g)| **g == Some(Group::Suffix))
        .find_map(|(i, _)| captures.get(i))
        .map_or(0, |m| m.len())
}

fn is_prev_boundary_char(byte: u8) -> bool {
    byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'.' | b'-')
}

fn valid_start(text: &str, start: usize) -> bool {
    start == 0 || !is_prev_boundary_char(text.as_bytes()[start - 1])
}

/// Advances past a contiguous `[A-Za-z0-9_.-]+` run starting at `from`, plus the separator
/// character after it, landing on the next UTF-8 boundary that could satisfy [`valid_start`].
fn skip_word_run(text: &str, from: usize) -> usize {
    let bytes = text.as_bytes();
    let mut at = from;
    while at < bytes.len() && is_prev_boundary_char(bytes[at]) {
        at += 1;
    }
    if at < text.len() {
        at += text[at..].chars().next().map_or(0, char::len_utf8);
    }
    at
}

/// Finds the next date match in `text` at or after `pos`, enforcing the leading word boundary and
/// short-date suffix exclusions.
fn next_date<'t>(text: &'t str, mut pos: usize) -> Option<(Span, regex::Captures<'t>)> {
    while pos < text.len() {
        let captures = DATE.captures_at(text, pos)?;
        let whole = captures.get(0)?;
        let start = whole.start();
        if !valid_start(text, start) {
            pos = skip_word_run(text, start);
            continue;
        }
        let end = whole.end() - suffix_len(&captures);
        if let Some(short) = last_group(&captures, Group::ShortDate)
            && BAD_SHORT_SUFFIX.is_match(&text[end..])
        {
            if last_group(&captures, Group::Dow).is_some()
                && let Some(sep) = short.find(['.', '/', '\\'])
            {
                let truncated = end - short.len() + sep + 1;
                if let Some(fallback) = DATE.captures_at(&text[..truncated], start)
                    && let Some(fb_whole) = fallback.get(0)
                    && fb_whole.start() == start
                {
                    let fb_end = fb_whole.end() - suffix_len(&fallback);
                    return Some((Span { start, end: fb_end }, fallback));
                }
            }
            pos = skip_word_run(text, start);
            continue;
        }
        return Some((Span { start, end }, captures));
    }
    None
}

/// The dates in `text` that get marked, as byte ranges, in order. Compose's editor wraps them in
/// `span.sh-date`.
pub fn find_dates(text: &str) -> Vec<std::ops::Range<usize>> {
    find(text)
        .into_iter()
        .map(|span| span.start..span.end)
        .collect()
}

/// The dates in `text` that get marked, in order.
fn find(text: &str) -> Vec<Span> {
    let spans = |regex: &Regex, after: usize| -> Vec<Span> {
        regex
            .find_iter(text)
            .map(|found| Span {
                start: found.start(),
                end: found.end() + after,
            })
            .collect()
    };
    let context = spans(&CONTEXT, 20);
    let headers = spans(&HEADER, 0);
    let mut out = Vec::new();
    let mut pos = 0;
    while let Some((date, captures)) = next_date(text, pos) {
        pos = date.end.max(date.start + 1);
        if date.touches(&headers) {
            continue;
        }
        let group = |wanted: Group| last_group(&captures, wanted);
        let keep = if group(Group::IsoTimestamp).is_some() || group(Group::LongDate).is_some() {
            true
        } else if let Some(short) = group(Group::ShortDate) {
            group(Group::Dow).is_some()
                || (short.contains('/')
                    && (date.touches(&context)
                        || text[date.end..]
                            .strip_prefix(':')
                            .is_some_and(|rest| rest.starts_with(char::is_whitespace))))
        } else {
            let concrete = group(Group::Dom).is_some() && group(Group::Month).is_some();
            let short_weekday = group(Group::Dow).is_some_and(|dow| dow.chars().count() <= 5);
            concrete
                || !short_weekday
                || group(Group::Relative).is_some()
                || group(Group::Dom).is_some()
                || date.end < text.len()
        };
        if keep {
            out.push(date);
        }
    }
    out
}

/// Wraps every detected date that lies within one text node in `<span class="exo-date">`,
/// except inside links. Spans quoted from earlier mail go first: Exo's, and the `sh-date` ones
/// some senders' mail carries.
pub(crate) fn decorate(root: &NodeRef) {
    let existing = dom::select(root, |n| {
        dom::is(n, "span") && (dom::has_class(n, "exo-date") || dom::has_class(n, "sh-date"))
    });
    for parent in dom::unwrap_all(&existing) {
        dom::normalize(&parent);
    }
    let texts: Vec<(NodeRef, String)> = dom::descendants(root)
        .filter_map(|node| Some((node.clone(), dom::text(&node)?)))
        .collect();
    // The text nodes are searched joined with spaces.
    let joined = texts
        .iter()
        .map(|(_, text)| text.as_str())
        .collect::<Vec<_>>()
        .join(" ");
    let dates = find(&joined);
    let mut decorated = Vec::new();
    let mut start = 0;
    for (node, text) in texts {
        let node_start = start;
        start += text.len() + 1;
        let in_link = dom::closest(&node, |ancestor| {
            dom::tag(ancestor).is_some_and(|tag| matches!(tag, "a" | "title" | "style" | "script"))
        })
        .is_some();
        if in_link {
            continue;
        }
        let first = dates.partition_point(|date| date.start < node_start);
        let mut pieces = Vec::new();
        let mut cursor = 0;
        for date in dates[first..]
            .iter()
            .take_while(|date| date.end <= node_start + text.len())
        {
            let (from, to) = (date.start - node_start, date.end - node_start);
            if from > cursor {
                pieces.push(dom::text_node(&text[cursor..from]));
            }
            let span = dom::element("span", &[("class", "exo-date")]);
            dom::append(&span, &dom::text_node(&text[from..to]));
            pieces.push(span);
            cursor = to;
        }
        if pieces.is_empty() {
            continue;
        }
        if cursor < text.len() {
            pieces.push(dom::text_node(&text[cursor..]));
        }
        decorated.push((node, pieces));
    }
    dom::replace_all(decorated);
}

/// Whether `node` holds quoted mail: a `blockquote`, `#quoted-content` or `.gmail_quote`.
fn is_quote(node: &NodeRef) -> bool {
    dom::is(node, "blockquote")
        || dom::attr(node, "id").as_deref() == Some("quoted-content")
        || dom::has_class(node, "gmail_quote")
}

/// The dates marked in the new part of a message (`root`) outside a quote left in it, as written.
pub(crate) fn unquoted(root: &NodeRef) -> Vec<String> {
    dom::select(root, |node| {
        dom::is(node, "span") && dom::has_class(node, "exo-date")
    })
    .into_iter()
    .filter(|span| dom::closest(span, is_quote).is_none())
    .map(|span| dom::text_content(&span))
    .collect()
}

/// Whether the new part of a message (`root`) has a phrase that proposes or settles a time (`how
/// about`, `are you free`, `works for me`…). It's looked for in `root`'s elements one level at a
/// time, from the first: one whose text has a phrase counts unless a quote is inside it, in which
/// case the search goes on among its children instead (and never back out).
pub(crate) fn proposes_time(root: &NodeRef) -> bool {
    static PHRASE: LazyLock<Regex> = LazyLock::new(|| {
        Regex::new(
            r"(?i)(?-u:\b)(?:invite|invitation|how about|what about|are you free|are you available|calendar|would be fine|is fine|is great|is good|sounds good|sounds great|works for me|that works)(?-u:\b)",
        )
        .expect("valid regex")
    });
    let level = |node: &NodeRef| -> Vec<NodeRef> {
        dom::children(node)
            .into_iter()
            .filter(|child| dom::is_element(child) || dom::is_text(child))
            .collect()
    };
    let mut nodes = level(root);
    let mut at = 0;
    while let Some(node) = nodes.get(at) {
        if dom::is_element(node) && PHRASE.is_match(&dom::text_content(node)) {
            if dom::select_first(node, |inner| {
                dom::is(inner, "blockquote")
                    || dom::attr(inner, "id").as_deref() == Some("quoted-content")
            })
            .is_none()
            {
                return true;
            }
            nodes = level(node);
            at = 0;
        } else {
            at += 1;
        }
    }
    false
}

#[cfg(test)]
mod tests {
    use super::*;

    fn dates(text: &str) -> Vec<&str> {
        find(text)
            .into_iter()
            .map(|d| &text[d.start..d.end])
            .collect()
    }

    #[test]
    fn finds_dates_but_not_numbers_or_quote_headers() {
        assert_eq!(
            dates("Heads up: today's design crit moves to 3pm"),
            ["today"]
        );
        assert_eq!(
            dates("scorecards in by tomorrow noon? deadline on Friday, so"),
            ["tomorrow", "on Friday"]
        );
        assert_eq!(dates("Wed Nov 25, 2026 9:00am"), ["Wed Nov 25"]);
        assert_eq!(dates("Nov 25 10:00am"), ["Nov 25"]);
        assert_eq!(dates("14:30 11/25"), ["11/25"]);
        assert_eq!(
            dates("foo-tomorrow bar_friday v1.2026-11-25"),
            Vec::<&str>::new()
        );
        assert_eq!(dates("a_mon, 2026-11-25"), ["2026-11-25"]);
        assert_eq!(dates("version 1.2.3 and 3/4 cup"), Vec::<&str>::new());
        assert_eq!(
            dates("On Mon, Nov 16, 2026 at 5:54 PM Tom <t@x.co> wrote:"),
            Vec::<&str>::new()
        );
    }

    #[test]
    fn reads_a_dates_parts() {
        assert_eq!(
            GROUPS.len(),
            DATE.captures_len(),
            "a group for every capture"
        );
        let parts = date_parts("Wed Nov 25").expect("a date");
        assert_eq!(
            (
                parts.dow.as_deref(),
                parts.month.as_deref(),
                parts.dom.as_deref()
            ),
            (Some("Wed"), Some("Nov"), Some("25"))
        );
        assert_eq!(
            date_parts("EOW")
                .and_then(|parts| parts.business)
                .as_deref(),
            Some("EOW")
        );
        assert_eq!(
            date_parts("next Friday")
                .and_then(|parts| parts.relative)
                .as_deref(),
            Some("next")
        );
    }

    #[test]
    fn searches_long_text_to_the_end() {
        // The backtracking engine used to give up on long stretches without a date, and the
        // search then retried the same position forever.
        let text = "x ".repeat(20_000) + "see you tomorrow";
        assert_eq!(dates(&text), ["tomorrow"]);
    }

    #[test]
    fn wraps_dates_within_text_nodes() {
        let document = dom::parse(
            "<p>Let's meet tomorrow, <a href=x>or Friday</a>, <a href=y><b>next Monday</b></a></p>",
        );
        let body = dom::body(&document).unwrap();
        decorate(&body);
        assert_eq!(
            dom::inner_html(&body),
            "<p>Let's meet <span class=\"exo-date\">tomorrow</span>, <a href=\"x\">or Friday</a>, <a href=\"y\"><b>next Monday</b></a></p>"
        );
    }

    #[test]
    fn lists_the_dates_outside_quotes() {
        let document = dom::parse(
            "<p>How about tomorrow?</p><blockquote>Or Friday</blockquote><div class=\"gmail_quote\">Monday</div>",
        );
        let body = dom::body(&document).unwrap();
        decorate(&body);
        assert_eq!(unquoted(&body), ["tomorrow"]);
    }

    #[test]
    fn finds_a_phrase_that_proposes_a_time_outside_quotes() {
        let proposes = |html: &str| proposes_time(&dom::body(&dom::parse(html)).unwrap());
        assert!(proposes("<div>Are you free on Friday?</div>"));
        assert!(proposes("<p>Thursday WORKS FOR ME.</p>"));
        // Words, not parts of words.
        assert!(!proposes("<p>Calendaring went well</p>"));
        // Only elements count.
        assert!(!proposes("How about Friday?"));
        // Around a quote, the search goes into the element, and it stops at the first one with
        // no quote inside that has a phrase, which can be the quote itself.
        assert!(proposes(
            "<div><p>That works.</p><blockquote>How about Friday?</blockquote></div>"
        ));
        // Text of its own beside a quote isn't an element, so it doesn't count.
        assert!(!proposes(
            "<div>How about Friday? <blockquote>Thanks!</blockquote></div><p>Is fine</p>"
        ));
    }

    #[test]
    fn redetects_dates_in_quoted_sh_date_spans() {
        let document = dom::parse(
            "<p>Heads up: <span class=\"sh-date\" data-date-isostring=\"2026-11-17\">today</span>'s crit</p>",
        );
        let body = dom::body(&document).unwrap();
        decorate(&body);
        assert_eq!(
            dom::inner_html(&body),
            "<p>Heads up: <span class=\"exo-date\">today</span>'s crit</p>"
        );
    }
}

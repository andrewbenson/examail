//! Recurrence as Outlook models it (`patternedRecurrence`: a pattern and a range), to and from
//! the RRULEs the calendar keeps.
//!
//! Outlook's patterns are daily, weekly (on given days), absolute and relative monthly, and
//! absolute and relative yearly, each with an interval, and its ranges end on a date, after a
//! count, or never. RRULEs outside that (hourly and finer frequencies, several month days,
//! several ordinal weekdays, `BYYEARDAY`, `BYWEEKNO`, `BYHOUR`, `BYSETPOS` beyond one weekday,
//! `RDATE`) have no Outlook pattern, and [`to_graph`] gives `None` for them.

use jiff::civil::{Date, Weekday};
use jiff::tz::TimeZone;
use serde_json::{Value, json};

const DAYS: &[(&str, &str, Weekday)] = &[
    ("SU", "sunday", Weekday::Sunday),
    ("MO", "monday", Weekday::Monday),
    ("TU", "tuesday", Weekday::Tuesday),
    ("WE", "wednesday", Weekday::Wednesday),
    ("TH", "thursday", Weekday::Thursday),
    ("FR", "friday", Weekday::Friday),
    ("SA", "saturday", Weekday::Saturday),
];

const INDEXES: &[(i8, &str)] = &[
    (1, "first"),
    (2, "second"),
    (3, "third"),
    (4, "fourth"),
    (-1, "last"),
];

fn day_name(weekday: Weekday) -> &'static str {
    DAYS.iter()
        .find(|(.., w)| *w == weekday)
        .map_or("sunday", |(_, name, _)| name)
}

/// `recurrence`'s RRULE as Outlook's `patternedRecurrence`, for a series starting on `start`
/// (in `zone`, which the range names). `None` when the rule has no Outlook equivalent.
pub fn to_graph(recurrence: &[String], start: Date, zone: &str, tz: &TimeZone) -> Option<Value> {
    if recurrence
        .iter()
        .any(|line| line.to_ascii_uppercase().starts_with("RDATE"))
    {
        return None;
    }
    let rule = recurrence.iter().find_map(|line| {
        line.strip_prefix("RRULE:")
            .or_else(|| line.strip_prefix("rrule:"))
    })?;
    let mut parts: Vec<(String, String)> = Vec::new();
    for part in rule.split(';') {
        let (key, value) = part.split_once('=')?;
        parts.push((key.to_ascii_uppercase(), value.to_ascii_uppercase()));
    }
    let get = |key: &str| {
        parts
            .iter()
            .find(|(k, _)| k == key)
            .map(|(_, v)| v.as_str())
    };
    if parts.iter().any(|(key, _)| {
        !matches!(
            key.as_str(),
            "FREQ"
                | "INTERVAL"
                | "COUNT"
                | "UNTIL"
                | "BYDAY"
                | "BYMONTHDAY"
                | "BYMONTH"
                | "BYSETPOS"
                | "WKST"
        )
    }) {
        return None;
    }
    let interval: i64 = get("INTERVAL").map_or(Some(1), |n| n.parse().ok())?;
    let by_day: Vec<(Option<i8>, Weekday)> = match get("BYDAY") {
        Some(days) => days.split(',').map(parse_day).collect::<Option<_>>()?,
        None => Vec::new(),
    };
    let set_pos: Option<i8> = get("BYSETPOS").map(str::parse).transpose().ok()?;
    let month_day: Option<i8> = match get("BYMONTHDAY") {
        Some(days) if days.contains(',') => return None,
        Some(day) => Some(day.parse().ok().filter(|d: &i8| *d > 0)?),
        None => None,
    };
    let month: Option<i8> = match get("BYMONTH") {
        Some(months) if months.contains(',') => return None,
        Some(month) => Some(month.parse().ok()?),
        None => None,
    };
    let first_day = get("WKST")
        .and_then(parse_day)
        .map_or("sunday", |(_, w)| day_name(w));
    let mut pattern = json!({
        "type": "daily",
        "interval": interval,
        "month": 0,
        "dayOfMonth": 0,
        "firstDayOfWeek": first_day,
        "index": "first",
    });
    let days: Vec<&str> = by_day.iter().map(|(_, w)| day_name(*w)).collect();
    // A relative pattern: one weekday with an ordinal (`2TU`), or weekdays with `BYSETPOS`.
    let plain = by_day.iter().all(|(n, _)| n.is_none());
    let relative = match (by_day.as_slice(), set_pos) {
        ([(Some(n), _)], None) => Some(*n),
        ([_, ..], Some(n)) if plain => Some(n),
        (_, None) if plain => None,
        _ => return None,
    };
    let index = |n: i8| INDEXES.iter().find(|(i, _)| *i == n).map(|(_, name)| *name);
    match get("FREQ")? {
        "DAILY" if by_day.is_empty() && month_day.is_none() && month.is_none() => {}
        "WEEKLY" if month_day.is_none() && month.is_none() && relative.is_none() => {
            pattern["type"] = "weekly".into();
            pattern["daysOfWeek"] = if days.is_empty() {
                json!([day_name(start.weekday())])
            } else {
                json!(days)
            };
        }
        "MONTHLY" if month.is_none() => match relative {
            Some(n) if month_day.is_none() => {
                pattern["type"] = "relativeMonthly".into();
                pattern["daysOfWeek"] = json!(days);
                pattern["index"] = index(n)?.into();
            }
            None if by_day.is_empty() => {
                pattern["type"] = "absoluteMonthly".into();
                pattern["dayOfMonth"] = month_day.unwrap_or(start.day()).into();
            }
            _ => return None,
        },
        "YEARLY" => match relative {
            Some(n) if month_day.is_none() => {
                pattern["type"] = "relativeYearly".into();
                pattern["daysOfWeek"] = json!(days);
                pattern["index"] = index(n)?.into();
                pattern["month"] = month.unwrap_or(start.month()).into();
            }
            None if by_day.is_empty() => {
                pattern["type"] = "absoluteYearly".into();
                pattern["dayOfMonth"] = month_day.unwrap_or(start.day()).into();
                pattern["month"] = month.unwrap_or(start.month()).into();
            }
            _ => return None,
        },
        _ => return None,
    }
    if pattern.get("daysOfWeek").is_none() {
        pattern["daysOfWeek"] = json!([]);
    }
    let mut range = json!({
        "type": "noEnd",
        "startDate": start.to_string(),
        "endDate": "0001-01-01",
        "recurrenceTimeZone": zone,
        "numberOfOccurrences": 0,
    });
    if let Some(count) = get("COUNT") {
        range["type"] = "numbered".into();
        range["numberOfOccurrences"] = count.parse::<i64>().ok()?.into();
    } else if let Some(until) = get("UNTIL") {
        range["type"] = "endDate".into();
        range["endDate"] = until_date(until, tz)?.to_string().into();
    }
    Some(json!({ "pattern": pattern, "range": range }))
}

/// Outlook's `patternedRecurrence` as an RRULE line, for a series in `tz` that is all-day or
/// not (which decides how `UNTIL` is written: a date, or the end date's last second in `tz`), or
/// what's wrong with it. The zero values Outlook writes for properties a pattern doesn't use are
/// accepted, so a recurrence read from an event can be sent back.
pub fn from_graph(recurrence: &Value, tz: &TimeZone, all_day: bool) -> Result<String, String> {
    let pattern = &recurrence["pattern"];
    let range = &recurrence["range"];
    let interval = pattern["interval"].as_i64().unwrap_or(1);
    if interval < 1 {
        return Err("The recurrence pattern's interval must be at least 1.".into());
    }
    let days = weekdays(pattern)?;
    let codes = |days: &[Weekday]| -> String {
        days.iter()
            .filter_map(|w| DAYS.iter().find(|(.., d)| d == w).map(|(code, ..)| *code))
            .collect::<Vec<_>>()
            .join(",")
    };
    let need_days = || {
        if days.is_empty() {
            Err("The recurrence pattern needs daysOfWeek.".to_owned())
        } else {
            Ok(())
        }
    };
    let day_of_month = || match pattern["dayOfMonth"].as_i64() {
        Some(day @ 1..=31) => Ok(day),
        _ => Err("The recurrence pattern needs a dayOfMonth from 1 to 31.".to_owned()),
    };
    let month = || match pattern["month"].as_i64() {
        Some(month @ 1..=12) => Ok(month),
        _ => Err("The recurrence pattern needs a month from 1 to 12.".to_owned()),
    };
    let relative = |rule: String| -> Result<String, String> {
        need_days()?;
        let n = index(pattern)?;
        Ok(match days.as_slice() {
            [one] => format!("{rule};BYDAY={n}{}", codes(&[*one])),
            several => format!("{rule};BYDAY={};BYSETPOS={n}", codes(several)),
        })
    };
    let mut rule = match pattern["type"].as_str().unwrap_or_default() {
        "daily" => "FREQ=DAILY".to_owned(),
        "weekly" => {
            need_days()?;
            format!("FREQ=WEEKLY;BYDAY={}", codes(&days))
        }
        "absoluteMonthly" => format!("FREQ=MONTHLY;BYMONTHDAY={}", day_of_month()?),
        "relativeMonthly" => relative("FREQ=MONTHLY".to_owned())?,
        "absoluteYearly" => format!(
            "FREQ=YEARLY;BYMONTH={};BYMONTHDAY={}",
            month()?,
            day_of_month()?
        ),
        "relativeYearly" => relative(format!("FREQ=YEARLY;BYMONTH={}", month()?))?,
        other => return Err(format!("'{other}' isn't a recurrence pattern type.")),
    };
    if interval > 1 {
        rule.push_str(&format!(";INTERVAL={interval}"));
    }
    if let Some(first) = pattern["firstDayOfWeek"].as_str() {
        let (code, ..) = DAYS
            .iter()
            .find(|(_, name, _)| name.eq_ignore_ascii_case(first))
            .ok_or_else(|| format!("'{first}' isn't a day of the week."))?;
        if *code != "SU" {
            rule.push_str(&format!(";WKST={code}"));
        }
    }
    match range["type"].as_str().unwrap_or_default() {
        "noEnd" => {}
        "numbered" => match range["numberOfOccurrences"].as_i64() {
            Some(count) if count > 0 => rule.push_str(&format!(";COUNT={count}")),
            _ => return Err("A numbered range needs a positive numberOfOccurrences.".into()),
        },
        "endDate" => {
            let end: Date = range["endDate"]
                .as_str()
                .and_then(|date| date.parse().ok())
                .ok_or_else(|| "An endDate range needs its endDate.".to_owned())?;
            if all_day {
                rule.push_str(&format!(";UNTIL={}", end.strftime("%Y%m%d")));
            } else {
                let last = end
                    .tomorrow()
                    .ok()
                    .and_then(|next| next.to_zoned(tz.clone()).ok())
                    .map(|zoned| zoned.timestamp() - jiff::SignedDuration::from_secs(1))
                    .ok_or_else(|| "The range's endDate is out of range.".to_owned())?;
                rule.push_str(&format!(";UNTIL={}", last.strftime("%Y%m%dT%H%M%SZ")));
            }
        }
        other => return Err(format!("'{other}' isn't a recurrence range type.")),
    }
    Ok(format!("RRULE:{rule}"))
}

/// The range's `startDate`.
pub fn range_start(recurrence: &Value) -> Option<Date> {
    recurrence["range"]["startDate"].as_str()?.parse().ok()
}

/// The first date on or after `from` that `recurrence`'s pattern falls on, where Outlook starts
/// a series whose start doesn't fit its pattern.
pub fn first_date(recurrence: &Value, from: Date) -> Option<Date> {
    let pattern = &recurrence["pattern"];
    let days = weekdays(pattern).ok()?;
    let month = pattern["month"].as_i64().and_then(|m| i8::try_from(m).ok());
    let day = pattern["dayOfMonth"]
        .as_i64()
        .and_then(|d| i8::try_from(d).ok());
    let index = index(pattern).ok();
    // The `n`th of `days` in `date`'s month (`-1` for the last).
    let nth = |date: Date, n: i8| -> Option<Date> {
        let first = date.first_of_month();
        let matching: Vec<Date> = (0..first.days_in_month())
            .filter_map(|offset| first.checked_add(jiff::Span::new().days(offset)).ok())
            .filter(|d| days.contains(&d.weekday()))
            .collect();
        if n < 0 {
            matching.last().copied()
        } else {
            matching.get(usize::try_from(n - 1).ok()?).copied()
        }
    };
    let mut date = from;
    // Four years of days covers every pattern, the 29th of February included.
    for _ in 0..1461 {
        let fits = match pattern["type"].as_str()? {
            "daily" => true,
            "weekly" => days.contains(&date.weekday()),
            "absoluteMonthly" => Some(date.day()) == day,
            "absoluteYearly" => Some(date.month()) == month && Some(date.day()) == day,
            "relativeMonthly" => nth(date, index?) == Some(date),
            "relativeYearly" => Some(date.month()) == month && nth(date, index?) == Some(date),
            _ => return None,
        };
        if fits {
            return Some(date);
        }
        date = date.tomorrow().ok()?;
    }
    None
}

/// A pattern's `daysOfWeek`, in any case.
fn weekdays(pattern: &Value) -> Result<Vec<Weekday>, String> {
    pattern["daysOfWeek"]
        .as_array()
        .map(Vec::as_slice)
        .unwrap_or_default()
        .iter()
        .map(|name| {
            let name = name.as_str().unwrap_or_default();
            DAYS.iter()
                .find(|(_, n, _)| n.eq_ignore_ascii_case(name))
                .map(|(.., w)| *w)
                .ok_or_else(|| format!("'{name}' isn't a day of the week."))
        })
        .collect()
}

/// A pattern's `index` as an ordinal: `first` is 1, `last` is -1.
fn index(pattern: &Value) -> Result<i8, String> {
    let name = pattern["index"].as_str().unwrap_or("first");
    INDEXES
        .iter()
        .find(|(_, n)| n.eq_ignore_ascii_case(name))
        .map(|(i, _)| *i)
        .ok_or_else(|| format!("'{name}' isn't a week index."))
}

/// `2TU` → (Some(2), Tuesday); `TU` → (None, Tuesday).
fn parse_day(text: &str) -> Option<(Option<i8>, Weekday)> {
    if !text.is_ascii() {
        return None;
    }
    let split = text.len().checked_sub(2)?;
    let (ordinal, day) = text.split_at(split);
    let weekday = DAYS
        .iter()
        .find(|(code, ..)| *code == day)
        .map(|(.., w)| *w)?;
    let ordinal = if ordinal.is_empty() {
        None
    } else {
        Some(ordinal.trim_start_matches('+').parse().ok()?)
    };
    Some((ordinal, weekday))
}

/// `UNTIL`'s date in the series' zone: `20261130` or `20261130T045959Z`.
fn until_date(value: &str, tz: &TimeZone) -> Option<Date> {
    if value.len() == 8 {
        return Date::strptime("%Y%m%d", value).ok();
    }
    let civil =
        jiff::civil::DateTime::strptime("%Y%m%dT%H%M%S", value.trim_end_matches('Z')).ok()?;
    let at = civil.to_zoned(TimeZone::UTC).ok()?.timestamp();
    Some(at.to_zoned(tz.clone()).date())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn graph(rule: &str) -> Option<Value> {
        let tz = TimeZone::get("America/New_York").unwrap();
        to_graph(
            &[rule.to_owned()],
            Date::constant(2026, 11, 16),
            "Eastern Standard Time",
            &tz,
        )
    }

    #[test]
    fn rrules_become_patterns() {
        let weekly = graph("RRULE:FREQ=WEEKLY;BYDAY=WE,FR").unwrap();
        assert_eq!(weekly["pattern"]["type"], "weekly");
        assert_eq!(
            weekly["pattern"]["daysOfWeek"],
            json!(["wednesday", "friday"])
        );
        assert_eq!(weekly["range"]["type"], "noEnd");
        assert_eq!(weekly["range"]["startDate"], "2026-11-16");
        let monthly = graph("RRULE:FREQ=MONTHLY;BYDAY=-1FR;COUNT=6").unwrap();
        assert_eq!(
            (
                monthly["pattern"]["type"].as_str(),
                monthly["pattern"]["index"].as_str()
            ),
            (Some("relativeMonthly"), Some("last"))
        );
        assert_eq!(monthly["range"]["numberOfOccurrences"], 6);
        let set_pos = graph("RRULE:FREQ=MONTHLY;BYDAY=TU;BYSETPOS=2").unwrap();
        assert_eq!(set_pos["pattern"]["index"], "second");
        let yearly = graph("RRULE:FREQ=YEARLY;UNTIL=20301116T045959Z").unwrap();
        assert_eq!(
            (
                yearly["pattern"]["month"].as_i64(),
                yearly["pattern"]["dayOfMonth"].as_i64()
            ),
            (Some(11), Some(16))
        );
        assert_eq!(
            yearly["range"]["endDate"], "2030-11-15",
            "UNTIL in the series' zone"
        );
        let daily = graph("RRULE:FREQ=DAILY;INTERVAL=3").unwrap();
        assert_eq!(daily["pattern"]["interval"], 3);
        assert_eq!(
            graph("RRULE:FREQ=WEEKLY").unwrap()["pattern"]["daysOfWeek"],
            json!(["monday"])
        );
    }

    #[test]
    fn patterns_round_trip() {
        let tz = TimeZone::get("America/New_York").unwrap();
        for rule in [
            "RRULE:FREQ=WEEKLY;BYDAY=WE,FR",
            "RRULE:FREQ=DAILY;INTERVAL=2;COUNT=10",
            "RRULE:FREQ=MONTHLY;BYDAY=-1FR",
            "RRULE:FREQ=MONTHLY;BYDAY=TH,FR;BYSETPOS=2",
            "RRULE:FREQ=MONTHLY;BYMONTHDAY=16",
            "RRULE:FREQ=YEARLY;BYMONTH=11;BYDAY=4TH",
            "RRULE:FREQ=WEEKLY;BYDAY=MO;WKST=MO;UNTIL=20261231T045959Z",
        ] {
            let pattern = graph(rule).unwrap();
            let back = from_graph(&pattern, &tz, false).unwrap();
            assert_eq!(graph(&back), Some(pattern), "{rule} → {back}");
        }
        let weekly =
            json!({ "pattern": { "type": "weekly", "interval": 1 }, "range": { "type": "noEnd" } });
        assert!(
            from_graph(&weekly, &tz, false).is_err(),
            "weekly needs its days"
        );
        let all_day = json!({
            "pattern": { "type": "daily", "interval": 1 },
            "range": { "type": "endDate", "startDate": "2026-11-16", "endDate": "2026-11-20" },
        });
        assert_eq!(
            from_graph(&all_day, &tz, true).unwrap(),
            "RRULE:FREQ=DAILY;UNTIL=20261120"
        );
    }

    #[test]
    fn series_start_on_their_first_occurrence() {
        let from = Date::constant(2026, 11, 18); // a Wednesday
        let first = |pattern: Value| first_date(&json!({ "pattern": pattern }), from).unwrap();
        assert_eq!(first(json!({ "type": "daily" })), from);
        assert_eq!(
            first(json!({ "type": "weekly", "daysOfWeek": ["Monday"] })),
            Date::constant(2026, 11, 23)
        );
        assert_eq!(
            first(json!({ "type": "absoluteMonthly", "dayOfMonth": 5 })),
            Date::constant(2026, 12, 5)
        );
        assert_eq!(
            first(
                json!({ "type": "relativeYearly", "month": 11, "daysOfWeek": ["thursday"], "index": "fourth" })
            ),
            Date::constant(2026, 11, 26)
        );
        assert_eq!(
            first(json!({ "type": "relativeMonthly", "daysOfWeek": ["friday"], "index": "last" })),
            Date::constant(2026, 11, 27)
        );
    }

    #[test]
    fn some_rrules_have_no_pattern() {
        for rule in [
            "RRULE:FREQ=HOURLY",
            "RRULE:FREQ=MONTHLY;BYMONTHDAY=1,15",
            "RRULE:FREQ=MONTHLY;BYDAY=1MO,3WE",
            "RRULE:FREQ=YEARLY;BYYEARDAY=100",
            "RRULE:FREQ=MONTHLY;BYMONTHDAY=-1",
            "RRULE:FREQ=WEEKLY;BYHOUR=9",
            "RRULE:FREQ=WEEKLY;BYDAY=éa",
            "RRULE:FREQ=WEEKLY;BYDAY=1é",
        ] {
            assert_eq!(graph(rule), None, "{rule}");
        }
        assert_eq!(parse_day("éa"), None);
        assert_eq!(parse_day("1é"), None);
        // Non-ASCII WKST falls back cleanly without panicking.
        let non_ascii_wkst = graph("RRULE:FREQ=WEEKLY;BYDAY=MO;WKST=éa").unwrap();
        assert_eq!(non_ascii_wkst["pattern"]["firstDayOfWeek"], "sunday");
        let non_ascii_wkst2 = graph("RRULE:FREQ=WEEKLY;BYDAY=MO;WKST=1é").unwrap();
        assert_eq!(non_ascii_wkst2["pattern"]["firstDayOfWeek"], "sunday");
    }
}

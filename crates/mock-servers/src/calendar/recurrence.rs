//! Expansion of the RRULE subset that fixtures and clients use: `FREQ=DAILY|WEEKLY|MONTHLY|YEARLY`
//! with `INTERVAL`, `COUNT`, `UNTIL`, `WKST`, `BYDAY` (including ordinals `1..=5` / `-5..=-1`),
//! `BYMONTHDAY`, `BYMONTH`, and `EXDATE`. Wall-clock times are kept across DST changes by stepping
//! in the event's own time zone, and an all-day series' instances are all-day.

use gmail::calendar::{Event, EventDateTime};
use jiff::civil::{Date, DateTime, Weekday};
use jiff::tz::TimeZone;
use jiff::{Timestamp, ToSpan, Zoned};

/// Most instances generated per series, so open-ended rules stay bounded.
const MAX_INSTANCES: usize = 730;
/// Most calendar days stepped per expansion (~100 years), so unsatisfiable rules never spin.
const MAX_STEPS: usize = 36_600;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Freq {
    Daily,
    Weekly,
    Monthly,
    Yearly,
}

/// A rule's `UNTIL`: a date, or a UTC date-time.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Until {
    Date(Date),
    Time(Timestamp),
}

/// An `EXDATE` entry: either a whole date or a concrete instant.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ExDate {
    Date(Date),
    Time(Timestamp),
}

struct Rule {
    freq: Freq,
    interval: i64,
    count: Option<usize>,
    until: Option<Until>,
    by_day: Vec<(Option<i8>, Weekday)>,
    by_month_day: Vec<i8>,
    by_month: Vec<i8>,
    wkst: Weekday,
}

/// Whether every rule's `UNTIL` takes the type of its series' start (a date for an all-day series
/// and a date-time otherwise, as RFC 5545 §3.3.10 requires) and its `BYDAY`/`WKST`/`BYMONTHDAY`/
/// `BYMONTH` parts are well-formed.
pub fn valid(event: &Event) -> bool {
    let all_day = event
        .start
        .as_ref()
        .is_some_and(|start| start.date.is_some());
    event
        .recurrence
        .iter()
        .filter_map(|line| line.strip_prefix("RRULE:"))
        .all(|rule| {
            rule.split(';').all(|part| {
                let Some((key, value)) = part.split_once('=') else {
                    return false;
                };
                match key {
                    "UNTIL" => matches!(
                        (parse_until(value), all_day),
                        (Some(Until::Date(_)), true) | (Some(Until::Time(_)), false)
                    ),
                    "BYDAY" => parse_by_day(value).is_some(),
                    "WKST" => parse_weekday(value).is_some(),
                    "BYMONTHDAY" => parse_by_month_day(value).is_some(),
                    "BYMONTH" => parse_by_month(value).is_some(),
                    _ => true,
                }
            })
        })
}

/// The instances of a recurring `event` starting before `limit`, as Calendar returns them with
/// `singleEvents=true`.
pub fn expand(
    event: &Event,
    calendar_tz: &TimeZone,
    limit: Timestamp,
) -> impl Iterator<Item = Event> {
    let instances = instances(event, calendar_tz, limit).unwrap_or_default();
    let master = event.clone();
    let all_day = master
        .start
        .as_ref()
        .is_some_and(|start| start.date.is_some());
    instances.into_iter().map(move |(start, end)| {
        let tz_name = master.start.as_ref().and_then(|s| s.time_zone.clone());
        let at = |time: &Zoned| {
            if all_day {
                EventDateTime {
                    date: Some(time.date().to_string()),
                    ..EventDateTime::default()
                }
            } else {
                EventDateTime {
                    date: None,
                    date_time: Some(time.strftime("%Y-%m-%dT%H:%M:%S%:z").to_string()),
                    time_zone: tz_name.clone(),
                }
            }
        };
        let suffix = if all_day {
            start.strftime("%Y%m%d").to_string()
        } else {
            start.timestamp().strftime("%Y%m%dT%H%M%SZ").to_string()
        };
        Event {
            id: master.id.as_ref().map(|id| format!("{id}_{suffix}")),
            recurring_event_id: master.id.clone(),
            original_start_time: Some(at(&start)),
            start: Some(at(&start)),
            end: Some(at(&end)),
            recurrence: Vec::new(),
            ..master.clone()
        }
    })
}

fn instances(
    event: &Event,
    calendar_tz: &TimeZone,
    limit: Timestamp,
) -> Option<Vec<(Zoned, Zoned)>> {
    let rule = event
        .recurrence
        .iter()
        .find_map(|line| line.strip_prefix("RRULE:"))
        .and_then(parse)?;
    let start_time = event.start.as_ref()?;
    let tz = start_time
        .time_zone
        .as_deref()
        .and_then(|name| TimeZone::get(name).ok())
        .unwrap_or_else(|| calendar_tz.clone());
    let first = start_time.timestamp(&tz)?.to_zoned(tz.clone());
    let last = event.end.as_ref()?.timestamp(&tz)?.to_zoned(tz.clone());
    // An all-day series lasts whole days, whatever DST does to them.
    let span = match start_time.date {
        Some(_) => (last.date() - first.date()).get_days().days(),
        None => last
            .timestamp()
            .duration_since(first.timestamp())
            .try_into()
            .ok()?,
    };

    let exdates = parse_exdates(&event.recurrence, &tz);
    let mut out = Vec::new();
    let mut generated = 0usize;
    let mut day = first.clone();
    for _ in 0..MAX_STEPS {
        if out.len() >= MAX_INSTANCES {
            break;
        }
        let ended = match rule.until {
            Some(Until::Date(until)) => day.date() > until,
            Some(Until::Time(until)) => day.timestamp() > until,
            None => false,
        };
        if day.timestamp() >= limit || ended {
            break;
        }
        if rule.count.is_some_and(|count| generated >= count) {
            break;
        }
        let date = day.date();
        let days = (date - first.date()).get_days();
        let month_ok = rule.by_month.is_empty() || rule.by_month.contains(&date.month());
        let month_day_ok =
            rule.by_month_day.is_empty() || matches_month_day(date, &rule.by_month_day);
        let matches = match rule.freq {
            Freq::Daily => {
                i64::from(days) % rule.interval == 0
                    && month_ok
                    && month_day_ok
                    && (rule.by_day.is_empty()
                        || rule.by_day.iter().any(|&(_, wd)| date.weekday() == wd))
            }
            Freq::Weekly => {
                let week_start = |d: Date| {
                    let offset = (d.weekday().to_monday_zero_offset()
                        - rule.wkst.to_monday_zero_offset())
                    .rem_euclid(7);
                    d - i64::from(offset).days()
                };
                let weeks = (week_start(date) - week_start(first.date())).get_days() / 7;
                let day_ok = if rule.by_day.is_empty() {
                    date.weekday() == first.weekday()
                } else {
                    rule.by_day.iter().any(|&(_, wd)| date.weekday() == wd)
                };
                day_ok && month_ok && month_day_ok && i64::from(weeks) % rule.interval == 0
            }
            Freq::Monthly => {
                let months = i64::from(date.year() - first.year()) * 12
                    + i64::from(date.month() - first.month());
                let on_day = match (rule.by_day.is_empty(), rule.by_month_day.is_empty()) {
                    (false, false) => {
                        month_day_ok
                            && rule
                                .by_day
                                .iter()
                                .any(|item| matches_by_day_in_month(date, *item))
                    }
                    (false, true) => rule
                        .by_day
                        .iter()
                        .any(|item| matches_by_day_in_month(date, *item)),
                    (true, false) => month_day_ok,
                    (true, true) => date.day() == first.day(),
                };
                month_ok && on_day && months % rule.interval == 0
            }
            Freq::Yearly => {
                let in_month = if !rule.by_month.is_empty() {
                    rule.by_month.contains(&date.month())
                } else if rule.by_day.is_empty() {
                    date.month() == first.month()
                } else {
                    true
                };
                let on_day = match (rule.by_day.is_empty(), rule.by_month_day.is_empty()) {
                    (false, false) => {
                        let by_day_ok = if !rule.by_month.is_empty() {
                            rule.by_day
                                .iter()
                                .any(|item| matches_by_day_in_month(date, *item))
                        } else {
                            rule.by_day
                                .iter()
                                .any(|item| matches_by_day_in_year(date, *item))
                        };
                        month_day_ok && by_day_ok
                    }
                    (false, true) => {
                        if !rule.by_month.is_empty() {
                            rule.by_day
                                .iter()
                                .any(|item| matches_by_day_in_month(date, *item))
                        } else {
                            rule.by_day
                                .iter()
                                .any(|item| matches_by_day_in_year(date, *item))
                        }
                    }
                    (true, false) => month_day_ok,
                    (true, true) => date.day() == first.day(),
                };
                in_month && on_day && i64::from(date.year() - first.year()) % rule.interval == 0
            }
        };
        if matches {
            generated += 1;
            let excluded = exdates.iter().any(|ex| match ex {
                ExDate::Date(d) => date == *d,
                ExDate::Time(ts) => day.timestamp() == *ts,
            });
            if !excluded {
                let end = day.checked_add(span).ok()?;
                out.push((day.clone(), end));
            }
        }
        day = day.checked_add(1.day()).ok()?;
    }
    Some(out)
}

fn matches_month_day(date: Date, by_month_day: &[i8]) -> bool {
    let day = i16::from(date.day());
    let dim = i16::from(date.days_in_month());
    by_month_day.iter().any(|&d| {
        if d > 0 {
            day == i16::from(d)
        } else {
            day == dim + 1 + i16::from(d)
        }
    })
}

fn matches_by_day_in_month(date: Date, (nth, weekday): (Option<i8>, Weekday)) -> bool {
    if date.weekday() != weekday {
        return false;
    }
    match nth {
        None => true,
        Some(n) if n > 0 => (date.day() - 1) / 7 + 1 == n,
        Some(n) => (date.days_in_month() - date.day()) / 7 + 1 == -n,
    }
}

fn matches_by_day_in_year(date: Date, (nth, weekday): (Option<i8>, Weekday)) -> bool {
    if date.weekday() != weekday {
        return false;
    }
    match nth {
        None => true,
        Some(n) if n > 0 => (date.day_of_year() - 1) / 7 + 1 == i16::from(n),
        Some(n) => (date.days_in_year() - date.day_of_year()) / 7 + 1 == i16::from(-n),
    }
}

fn parse_exdates(recurrence: &[String], fallback_tz: &TimeZone) -> Vec<ExDate> {
    let mut out = Vec::new();
    for line in recurrence {
        let Some((name, params, value)) = gmail::ics::content_line(line) else {
            continue;
        };
        if name != "EXDATE" {
            continue;
        }
        let tzid = params.get("TZID").map(String::as_str);
        let kind = params.get("VALUE").map(String::as_str);
        for item in value.split(',').map(str::trim).filter(|s| !s.is_empty()) {
            if kind.is_some_and(|k| k.eq_ignore_ascii_case("DATE")) || item.len() == 8 {
                if let Ok(date) = Date::strptime("%Y%m%d", item) {
                    out.push(ExDate::Date(date));
                }
                continue;
            }
            let (raw, tz) = if let Some(utc) = item.strip_suffix('Z') {
                (utc, TimeZone::UTC)
            } else {
                let tz = tzid
                    .and_then(gmail::ics::resolve_timezone)
                    .unwrap_or_else(|| fallback_tz.clone());
                (item, tz)
            };
            if let Ok(civil) = DateTime::strptime("%Y%m%dT%H%M%S", raw)
                && let Ok(zoned) = civil.to_zoned(tz)
            {
                out.push(ExDate::Time(zoned.timestamp()));
            }
        }
    }
    out
}

fn parse(rule: &str) -> Option<Rule> {
    let mut parsed = Rule {
        freq: Freq::Daily,
        interval: 1,
        count: None,
        until: None,
        by_day: Vec::new(),
        by_month_day: Vec::new(),
        by_month: Vec::new(),
        wkst: Weekday::Monday,
    };
    let mut freq = None;
    for part in rule.split(';') {
        let (key, value) = part.split_once('=')?;
        match key {
            "FREQ" => {
                freq = Some(match value {
                    "DAILY" => Freq::Daily,
                    "WEEKLY" => Freq::Weekly,
                    "MONTHLY" => Freq::Monthly,
                    "YEARLY" => Freq::Yearly,
                    _ => return None,
                });
            }
            "INTERVAL" => parsed.interval = value.parse().ok().filter(|&n: &i64| n > 0)?,
            "COUNT" => parsed.count = Some(value.parse().ok()?),
            "UNTIL" => parsed.until = Some(parse_until(value)?),
            "BYDAY" => parsed.by_day = parse_by_day(value)?,
            "BYMONTHDAY" => parsed.by_month_day = parse_by_month_day(value)?,
            "BYMONTH" => parsed.by_month = parse_by_month(value)?,
            "WKST" => parsed.wkst = parse_weekday(value)?,
            _ => {}
        }
    }
    parsed.freq = freq?;
    Some(parsed)
}

fn parse_weekday(code: &str) -> Option<Weekday> {
    if !code.is_ascii() {
        return None;
    }
    match code {
        "MO" => Some(Weekday::Monday),
        "TU" => Some(Weekday::Tuesday),
        "WE" => Some(Weekday::Wednesday),
        "TH" => Some(Weekday::Thursday),
        "FR" => Some(Weekday::Friday),
        "SA" => Some(Weekday::Saturday),
        "SU" => Some(Weekday::Sunday),
        _ => None,
    }
}

fn parse_by_day_token(token: &str) -> Option<(Option<i8>, Weekday)> {
    if !token.is_ascii() {
        return None;
    }
    let at = token.len().checked_sub(2)?;
    let weekday = parse_weekday(&token[at..])?;
    let ordinal = if at == 0 {
        None
    } else {
        let n: i8 = token[..at].parse().ok()?;
        if !matches!(n, 1..=5 | -5..=-1) {
            return None;
        }
        Some(n)
    };
    Some((ordinal, weekday))
}

fn parse_by_day(value: &str) -> Option<Vec<(Option<i8>, Weekday)>> {
    if value.is_empty() {
        return None;
    }
    value.split(',').map(parse_by_day_token).collect()
}

fn parse_by_month_day(value: &str) -> Option<Vec<i8>> {
    if value.is_empty() {
        return None;
    }
    value
        .split(',')
        .map(|token| {
            token
                .parse::<i8>()
                .ok()
                .filter(|&d| matches!(d, 1..=31 | -31..=-1))
        })
        .collect()
}

fn parse_by_month(value: &str) -> Option<Vec<i8>> {
    if value.is_empty() {
        return None;
    }
    value
        .split(',')
        .map(|token| token.parse::<i8>().ok().filter(|&m| (1..=12).contains(&m)))
        .collect()
}

/// `UNTIL=20261231T235959Z` or `UNTIL=20261231`, the last day the series may start on.
fn parse_until(value: &str) -> Option<Until> {
    if value.len() == 8 {
        return Date::strptime("%Y%m%d", value).ok().map(Until::Date);
    }
    DateTime::strptime("%Y%m%dT%H%M%SZ", value)
        .ok()?
        .to_zoned(TimeZone::UTC)
        .ok()
        .map(|zoned| Until::Time(zoned.timestamp()))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn weekly(rule: &str) -> Event {
        Event {
            id: Some("abc".into()),
            start: Some(EventDateTime {
                date_time: Some("2026-10-27T10:00:00-04:00".into()),
                time_zone: Some("America/New_York".into()),
                ..EventDateTime::default()
            }),
            end: Some(EventDateTime {
                date_time: Some("2026-10-27T10:30:00-04:00".into()),
                time_zone: Some("America/New_York".into()),
                ..EventDateTime::default()
            }),
            recurrence: vec![rule.into()],
            ..Event::default()
        }
    }

    #[test]
    fn keeps_wall_clock_across_dst() {
        let tz = TimeZone::get("America/New_York").unwrap();
        let instances: Vec<Event> =
            expand(&weekly("RRULE:FREQ=WEEKLY;COUNT=3"), &tz, Timestamp::MAX).collect();
        let starts: Vec<_> = instances
            .iter()
            .map(|e| e.start.clone().unwrap().date_time.unwrap())
            .collect();
        // DST ends on 2026-11-01, so it's still 10:00 local, now at -05:00.
        assert_eq!(
            starts,
            [
                "2026-10-27T10:00:00-04:00",
                "2026-11-03T10:00:00-05:00",
                "2026-11-10T10:00:00-05:00"
            ]
        );
        assert_eq!(instances[1].id.as_deref(), Some("abc_20261103T150000Z"));
        assert_eq!(instances[1].recurring_event_id.as_deref(), Some("abc"));
    }

    #[test]
    fn an_all_day_series_has_all_day_instances_and_ends_on_a_date() {
        let day = |date: &str| EventDateTime {
            date: Some(date.into()),
            ..EventDateTime::default()
        };
        let all_day = |rule: &str| Event {
            id: Some("abc".into()),
            start: Some(day("2026-10-27")),
            end: Some(day("2026-10-28")),
            recurrence: vec![rule.into()],
            ..Event::default()
        };
        let tz = TimeZone::get("America/New_York").unwrap();
        let series = all_day("RRULE:FREQ=WEEKLY;UNTIL=20261110");
        let instances: Vec<Event> = expand(&series, &tz, Timestamp::MAX).collect();
        let days: Vec<_> = instances
            .iter()
            .map(|e| {
                (
                    e.start.clone().unwrap().date.unwrap(),
                    e.end.clone().unwrap().date.unwrap(),
                )
            })
            .collect();
        // Across the end of DST (Nov 1) too, each lasts the day.
        assert_eq!(
            days,
            [
                ("2026-10-27".to_owned(), "2026-10-28".to_owned()),
                ("2026-11-03".to_owned(), "2026-11-04".to_owned()),
                ("2026-11-10".to_owned(), "2026-11-11".to_owned())
            ]
        );
        assert_eq!(instances[1].id.as_deref(), Some("abc_20261103"));
        assert!(valid(&series));
        assert!(
            !valid(&all_day("RRULE:FREQ=WEEKLY;UNTIL=20261110T045959Z")),
            "an all-day series ends on a date"
        );
        assert!(
            !valid(&weekly("RRULE:FREQ=WEEKLY;UNTIL=20261110")),
            "a timed one at a time"
        );
    }

    #[test]
    fn weekly_by_day_with_until() {
        let tz = TimeZone::get("America/New_York").unwrap();
        let count = expand(
            &weekly("RRULE:FREQ=WEEKLY;BYDAY=TU,TH;UNTIL=20261106T000000Z"),
            &tz,
            Timestamp::MAX,
        )
        .count();
        assert_eq!(count, 4); // Oct 27, Oct 29, Nov 3, Nov 5
    }

    #[test]
    fn rejects_non_ascii_and_out_of_range_byday_ordinals() {
        for bad in [
            "RRULE:FREQ=WEEKLY;BYDAY=éa",
            "RRULE:FREQ=WEEKLY;BYDAY=1é",
            "RRULE:FREQ=WEEKLY;WKST=éa",
            "RRULE:FREQ=WEEKLY;WKST=1é",
            "RRULE:FREQ=MONTHLY;BYDAY=9MO",
            "RRULE:FREQ=MONTHLY;BYDAY=0MO",
            "RRULE:FREQ=MONTHLY;BYDAY=-6FR",
            "RRULE:FREQ=MONTHLY;BYDAY=+6TU",
        ] {
            let rule = bad.strip_prefix("RRULE:").unwrap();
            assert!(parse(rule).is_none(), "parse should reject {bad}");
            assert!(!valid(&weekly(bad)), "valid should reject {bad}");
        }
        assert_eq!(parse_by_day_token("éa"), None);
        assert_eq!(parse_by_day_token("1é"), None);
        for ok in [
            "RRULE:FREQ=MONTHLY;BYDAY=1MO",
            "RRULE:FREQ=MONTHLY;BYDAY=5MO",
            "RRULE:FREQ=MONTHLY;BYDAY=-1FR",
            "RRULE:FREQ=MONTHLY;BYDAY=-5FR",
        ] {
            let rule = ok.strip_prefix("RRULE:").unwrap();
            assert!(parse(rule).is_some(), "parse should accept {ok}");
            assert!(valid(&weekly(ok)), "valid should accept {ok}");
        }
    }

    #[test]
    fn honors_wkst_bymonthday_bymonth_and_exdate() {
        let tz = TimeZone::get("America/New_York").unwrap();

        // WKST=SU vs WKST=MO changes which Sunday falls in the first biweekly week.
        let mon_event = |rule: &str| Event {
            id: Some("wk".into()),
            start: Some(EventDateTime {
                date_time: Some("2026-11-02T10:00:00-05:00".into()),
                time_zone: Some("America/New_York".into()),
                ..EventDateTime::default()
            }),
            end: Some(EventDateTime {
                date_time: Some("2026-11-02T10:30:00-05:00".into()),
                time_zone: Some("America/New_York".into()),
                ..EventDateTime::default()
            }),
            recurrence: vec![rule.into()],
            ..Event::default()
        };
        let dates = |event: &Event| -> Vec<String> {
            expand(event, &tz, Timestamp::MAX)
                .map(|e| e.start.unwrap().date_time.unwrap()[..10].to_owned())
                .collect()
        };
        assert_eq!(
            dates(&mon_event(
                "RRULE:FREQ=WEEKLY;INTERVAL=2;BYDAY=SU,MO;WKST=MO;COUNT=4"
            )),
            ["2026-11-02", "2026-11-08", "2026-11-16", "2026-11-22"]
        );
        assert_eq!(
            dates(&mon_event(
                "RRULE:FREQ=WEEKLY;INTERVAL=2;BYDAY=SU,MO;WKST=SU;COUNT=4"
            )),
            ["2026-11-02", "2026-11-15", "2026-11-16", "2026-11-29"]
        );

        // BYMONTHDAY (positive and negative) and BYMONTH.
        assert_eq!(
            dates(&mon_event("RRULE:FREQ=MONTHLY;BYMONTHDAY=-1;COUNT=3")),
            ["2026-11-30", "2026-12-31", "2027-01-31"]
        );
        assert_eq!(
            dates(&mon_event("RRULE:FREQ=YEARLY;BYMONTH=11;BYDAY=4TH;COUNT=3")),
            ["2026-11-26", "2027-11-25", "2028-11-23"]
        );
        assert_eq!(
            dates(&mon_event(
                "RRULE:FREQ=MONTHLY;BYMONTH=1,7;BYMONTHDAY=15;COUNT=3"
            )),
            ["2027-01-15", "2027-07-15", "2028-01-15"]
        );

        // EXDATE removes excluded instances (UTC and TZID) without extending COUNT.
        let mut with_exdate = weekly("RRULE:FREQ=WEEKLY;COUNT=4");
        with_exdate
            .recurrence
            .push("EXDATE:20261103T150000Z".into());
        with_exdate
            .recurrence
            .push("EXDATE;TZID=America/New_York:20261110T100000".into());
        assert_eq!(dates(&with_exdate), ["2026-10-27", "2026-11-17"]);

        // EXDATE;VALUE=DATE removes all-day occurrences.
        let all_day = Event {
            id: Some("ad".into()),
            start: Some(EventDateTime {
                date: Some("2026-11-01".into()),
                ..EventDateTime::default()
            }),
            end: Some(EventDateTime {
                date: Some("2026-11-02".into()),
                ..EventDateTime::default()
            }),
            recurrence: vec![
                "RRULE:FREQ=DAILY;COUNT=4".into(),
                "EXDATE;VALUE=DATE:20261102,20261103".into(),
            ],
            ..Event::default()
        };
        let all_day_dates: Vec<String> = expand(&all_day, &tz, Timestamp::MAX)
            .map(|e| e.start.unwrap().date.unwrap())
            .collect();
        assert_eq!(all_day_dates, ["2026-11-01", "2026-11-04"]);
    }

    #[test]
    fn unsatisfiable_rule_terminates_without_spinning() {
        let tz = TimeZone::get("America/New_York").unwrap();
        // Feb 31 never occurs; MAX_STEPS bounds expansion even with Timestamp::MAX.
        let impossible = weekly("RRULE:FREQ=MONTHLY;BYMONTH=2;BYMONTHDAY=31");
        assert!(valid(&impossible));
        assert_eq!(expand(&impossible, &tz, Timestamp::MAX).count(), 0);
        // Invalid BYDAY=9MO is rejected outright before stepping.
        let invalid_ordinal = weekly("RRULE:FREQ=MONTHLY;BYDAY=9MO");
        assert_eq!(expand(&invalid_ordinal, &tz, Timestamp::MAX).count(), 0);
    }
}

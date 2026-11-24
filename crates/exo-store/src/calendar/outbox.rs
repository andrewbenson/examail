//! The calendar's changes until Calendar has them: new events, edits, deletions and answers,
//! queued in the store, so they're made offline, outlast a quit and go when the connection is
//! back.
//!
//! [`Store::queue_calendar_change`] writes a row, and [`Store::events`] shows it at once, because
//! every read lays the queued changes over Calendar's copy. A change can be held first, for
//! Undo's ten seconds, and [`Store::unqueue_calendar_change`] takes back one that hasn't gone.
//! The caller sends the changes whose turn it is ([`Store::due_calendar_changes`],
//! [`Store::send_calendar_change`]). There's one queue per event and its series, so a change
//! waits for the changes before it to the same event and for no other. Being offline, signed out
//! or rate limited postpones a change. An answer from Calendar that retrying won't change gives
//! the change up, and the calendar shows Calendar's copy again.

use std::collections::{HashMap, HashSet};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Mutex, PoisonError};

use gmail::calendar::{Event, EventDateTime};
use jiff::tz::TimeZone;
use jiff::{SignedDuration, Timestamp};
use serde::{Deserialize, Serialize};
use toasty::{Executor, Json};

use super::{StoredEvent, put};
use crate::db::{CalendarChangeRow, EventRow};
use crate::error::{Error, Result};
use crate::queue::{self, Disposition, FlushGate};
use crate::store::Store;

/// Per-store flush and in-flight row tracking for the calendar outbox, so a concurrent Undo or
/// Delete of an in-flight `Create` waits for the send to finish and queues a compensating
/// `Delete` instead of leaving a live event on Calendar.
#[derive(Default)]
pub(crate) struct CalendarOutboxSync {
    flush: FlushGate,
    in_flight: Mutex<InFlight>,
}

#[derive(Default)]
struct InFlight {
    sending: Option<u64>,
    watched_creates: HashMap<u64, bool>,
}

/// Clears `in_flight.sending` if `send_calendar_change` exits early on an error or cancellation.
struct SendingGuard<'a> {
    sync: &'a CalendarOutboxSync,
    active: bool,
}

impl Drop for SendingGuard<'_> {
    fn drop(&mut self) {
        if self.active {
            let mut state = self
                .sync
                .in_flight
                .lock()
                .unwrap_or_else(PoisonError::into_inner);
            state.sending = None;
        }
    }
}

fn fresh_event_id() -> String {
    static NEXT: AtomicU64 = AtomicU64::new(1);
    let nanos = u128::try_from(Timestamp::now().as_nanosecond()).unwrap_or(0);
    let seq = NEXT.fetch_add(1, Ordering::Relaxed);
    format!("e{:016x}{:08x}", nanos as u64, seq as u32)
}

/// A change to the calendar, as queued for Calendar.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum CalendarChange {
    /// Adds `event`, whose id is chosen, to `calendar`, inviting its guests.
    Create { calendar: String, event: Event },
    /// Sets the fields `patch` has on event `id` of `calendar` (an instance, or a series),
    /// telling the guests when `notify`. For a series, `shift` moves its start and end as far as
    /// the instance's moved. `shown` is how the cached events it changes look meanwhile.
    Edit {
        calendar: String,
        id: String,
        patch: Event,
        #[serde(default)]
        clear_attendees: bool,
        notify: bool,
        shift: Option<(SignedDuration, SignedDuration)>,
        shown: Vec<Event>,
    },
    /// Deletes event `id` of `calendar` (an instance, or a series and all its instances).
    Delete {
        calendar: String,
        id: String,
        notify: bool,
    },
    /// Ends series `series` before its instance at `from`.
    Truncate {
        calendar: String,
        series: String,
        from: Cut,
        notify: bool,
    },
    /// Edits series `series` from its instance at `from` on. The series ends before that
    /// instance, and a new series (with `patch`'s id) takes over with the old series' rule and
    /// what `patch` sets. The new series starts at `patch`'s start, or else at the instance's
    /// (`start` and `end`). From the series' first instance, the series itself is edited. `shown`
    /// is how the instances it changes look meanwhile.
    Split {
        calendar: String,
        series: String,
        from: Cut,
        patch: Event,
        #[serde(default)]
        clear_attendees: bool,
        start: EventDateTime,
        end: EventDateTime,
        notify: bool,
        shown: Vec<Event>,
    },
    /// Sets my answer (`accepted`, `tentative`, `declined`) to `event` on `calendar`, whose id is
    /// my address. The answer goes to the instance, or to its whole series when `all`.
    Respond {
        calendar: String,
        event: Event,
        status: String,
        all: bool,
    },
    /// Answers an invite on `calendar` from conversation `thread`. `invite` is its event as the
    /// `.ics` describes it. The answer goes to the calendar's event with the invite's UID, which
    /// is imported first when there's none, unless the invitation mail has `links` for a sender
    /// Calendar doesn't know.
    RespondToInvite {
        calendar: String,
        invite: Event,
        status: String,
        thread: Option<String>,
        links: Option<InviteLinks>,
    },
}

/// The links of an invitation from a sender Calendar doesn't know, which it keeps off the
/// calendar: the one that marks the sender as known, and the answers' own.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct InviteLinks {
    pub mark_sender_known: String,
    pub accepted: Option<String>,
    pub declined: Option<String>,
    pub tentative: Option<String>,
}

impl InviteLinks {
    /// The link that answers `status` (`accepted`, `declined`, `tentative`).
    pub fn answer(&self, status: &str) -> Option<&str> {
        match status {
            "accepted" => self.accepted.as_deref(),
            "declined" => self.declined.as_deref(),
            "tentative" => self.tentative.as_deref(),
            _ => None,
        }
    }
}

/// Why Calendar refused a change.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Refusal {
    /// The event it's for has been deleted or cancelled.
    Gone,
    /// An answer to an invite from a sender Calendar doesn't know, whose event it doesn't have
    /// ([`InviteLinks`]).
    UnknownSender,
    /// Only the event's organizer can make it (`forbiddenForNonOrganizer`).
    NonOrganizer,
    /// Anything else.
    Other,
}

/// Where "this and following" cuts a series: an all-day series at an instance's day, any other
/// series at the instance's start.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Cut {
    /// The instance's day.
    Day(jiff::civil::Date),
    /// The instance's start.
    At(Timestamp),
}

/// A queued change whose turn it is, for [`Store::send_calendar_change`].
#[derive(Clone, Debug)]
pub struct QueuedCalendarChange {
    id: u64,
    /// What it was queued under.
    pub token: u64,
    pub change: CalendarChange,
}

/// How sending a queued change went.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum CalendarOutcome {
    /// Calendar has it, and the cache has Calendar's answer.
    Sent,
    /// Offline, signed out or rate limited, so the change waits and goes again later.
    Postponed,
    /// Calendar refused it with this error, so it's given up and its overlay is gone.
    Refused { error: String, refusal: Refusal },
}

impl Cut {
    /// Whether an instance starting at `start` stays, which it does when it comes before the cut
    /// (or its start can't be read).
    #[must_use]
    pub fn keeps(self, start: &EventDateTime) -> bool {
        match self {
            Cut::Day(day) => start
                .date
                .as_deref()
                .and_then(|date| date.parse::<jiff::civil::Date>().ok())
                .is_none_or(|date| date < day),
            Cut::At(at) => start
                .timestamp(&TimeZone::UTC)
                .is_none_or(|start| start < at),
        }
    }

    /// The rule's `UNTIL`. For an all-day series it's the day before, as a date (RFC 5545
    /// §3.3.10 has `UNTIL` take the start's type, and Calendar's own all-day example ends
    /// `UNTIL=20150628`). For any other series it's the second before, in UTC.
    fn until(self) -> String {
        match self {
            Cut::Day(day) => day
                .yesterday()
                .unwrap_or(day)
                .strftime("%Y%m%d")
                .to_string(),
            Cut::At(at) => at
                .checked_sub(SignedDuration::from_secs(1))
                .unwrap_or(at)
                .strftime("%Y%m%dT%H%M%SZ")
                .to_string(),
        }
    }
}

/// Shifts a date-only or timed [`EventDateTime`] by civil wall-clock delta `by`, preserving local
/// wall-clock time across DST transitions.
#[must_use]
pub fn shift_event_time(time: Option<&EventDateTime>, by: SignedDuration) -> Option<EventDateTime> {
    retime_event_time(time, by, None)
}

/// Shifts `time` by civil wall-clock delta `by`, adopting `template`'s all-day/timed representation
/// (and zone, when given) when toggling `all_day` on a series.
#[must_use]
pub fn retime_event_time(
    time: Option<&EventDateTime>,
    by: SignedDuration,
    template: Option<&EventDateTime>,
) -> Option<EventDateTime> {
    let time = time?;
    let wall = civil_time(time)?.checked_add(by).ok()?;
    let all_day = template.map_or(time.date.is_some(), |target| target.date.is_some());
    if all_day {
        return Some(EventDateTime {
            date: Some(wall.date().to_string()),
            ..EventDateTime::default()
        });
    }
    let zone_name = template
        .and_then(|target| target.time_zone.as_deref())
        .or(time.time_zone.as_deref());
    let zone = zone_name
        .and_then(|name| TimeZone::get(name).ok())
        .or_else(|| {
            template
                .and_then(|target| target.date_time.as_deref())
                .and_then(super::recurrence::rfc3339_zone)
        })
        .or_else(|| {
            time.date_time
                .as_deref()
                .and_then(super::recurrence::rfc3339_zone)
        })
        .unwrap_or(TimeZone::UTC);
    let zoned = wall.to_zoned(zone).ok()?;
    Some(EventDateTime {
        date: None,
        date_time: Some(zoned.strftime("%Y-%m-%dT%H:%M:%S%:z").to_string()),
        time_zone: zone_name.map(str::to_owned),
    })
}

fn civil_time(time: &EventDateTime) -> Option<jiff::civil::DateTime> {
    if let Some(date) = &time.date {
        return Some(date.parse::<jiff::civil::Date>().ok()?.at(0, 0, 0, 0));
    }
    let date_time = time.date_time.as_deref()?;
    let at: Timestamp = date_time.parse().ok()?;
    let zone = time
        .time_zone
        .as_deref()
        .and_then(|name| TimeZone::get(name).ok())
        .or_else(|| super::recurrence::rfc3339_zone(date_time))
        .unwrap_or(TimeZone::UTC);
    Some(at.to_zoned(zone).datetime())
}

impl CalendarChange {
    /// Shifts `time` by civil wall-clock delta `by`, preserving local wall-clock time across DST.
    #[must_use]
    pub fn shift_event_time(
        time: Option<&EventDateTime>,
        by: SignedDuration,
    ) -> Option<EventDateTime> {
        shift_event_time(time, by)
    }

    /// Shifts `time` by civil wall-clock delta `by`, adopting `template`'s all-day/timed kind.
    #[must_use]
    pub fn retime_event_time(
        time: Option<&EventDateTime>,
        by: SignedDuration,
        template: Option<&EventDateTime>,
    ) -> Option<EventDateTime> {
        retime_event_time(time, by, template)
    }

    /// The calendar it changes.
    #[must_use]
    pub fn calendar(&self) -> &str {
        match self {
            Self::Create { calendar, .. }
            | Self::Edit { calendar, .. }
            | Self::Delete { calendar, .. }
            | Self::Truncate { calendar, .. }
            | Self::Split { calendar, .. }
            | Self::Respond { calendar, .. }
            | Self::RespondToInvite { calendar, .. } => calendar,
        }
    }

    /// Its queue, which is the event's. An instance (`{series}_{start}`) shares its series' queue.
    fn queue(&self) -> String {
        let id = match self {
            Self::Create { event, .. } => event.id.as_deref().unwrap_or_default(),
            Self::Edit { id, .. }
            | Self::Delete { id, .. }
            | Self::Truncate { series: id, .. }
            | Self::Split { series: id, .. } => id,
            Self::Respond { event, .. } => event.id.as_deref().unwrap_or_default(),
            Self::RespondToInvite { invite, .. } => {
                return format!(
                    "{}/{}",
                    self.calendar(),
                    invite.ical_uid.as_deref().unwrap_or_default()
                );
            }
        };
        format!("{}/{}", self.calendar(), id.split('_').next().unwrap_or(id))
    }

    /// `events` as they are once this change is made. A new event, or one an edit moves, joins
    /// them when `shown` says it belongs. For a new series, its instances starting in `window`
    /// do.
    pub(crate) fn apply(
        &self,
        events: &mut Vec<StoredEvent>,
        shown: &dyn Fn(&str, &Event) -> bool,
        window: Option<(Timestamp, Timestamp)>,
    ) {
        let on = |stored: &StoredEvent, calendar: &str| stored.calendar_id == calendar;
        match self {
            Self::Create { calendar, event } if !event.recurrence.is_empty() => {
                if let Some((from, to)) = window {
                    for instance in super::recurrence::instances(event, from, to) {
                        upsert(events, calendar, &instance, true, shown);
                    }
                }
            }
            Self::Create { calendar, event } => upsert(events, calendar, event, true, shown),
            Self::Edit {
                calendar,
                shown: changed,
                ..
            } => {
                for event in changed {
                    upsert(events, calendar, event, false, shown);
                }
            }
            Self::Delete { calendar, id, .. } => events.retain(|stored| {
                !on(stored, calendar)
                    || (stored.event.id.as_deref() != Some(id)
                        && stored.event.recurring_event_id.as_deref() != Some(id))
            }),
            Self::Truncate {
                calendar,
                series,
                from,
                ..
            } => truncate(events, calendar, series, *from),
            Self::Split {
                calendar,
                series,
                from,
                shown: changed,
                ..
            } => {
                truncate(events, calendar, series, *from);
                for event in changed {
                    upsert(events, calendar, event, false, shown);
                }
            }
            Self::Respond {
                calendar,
                event,
                status,
                all,
            } => {
                let series = event.recurring_event_id.as_deref().filter(|_| *all);
                for stored in events.iter_mut().filter(|stored| on(stored, calendar)) {
                    if stored.event.id == event.id
                        || (series.is_some()
                            && stored.event.recurring_event_id.as_deref() == series)
                    {
                        answer(&mut stored.event, calendar, status);
                    }
                }
            }
            Self::RespondToInvite {
                calendar,
                invite,
                status,
                ..
            } => {
                for stored in events.iter_mut().filter(|stored| {
                    on(stored, calendar)
                        && stored.event.ical_uid.is_some()
                        && stored.event.ical_uid == invite.ical_uid
                }) {
                    answer(&mut stored.event, calendar, status);
                }
            }
        }
    }
}

/// `events` without the instances of series `series` on `calendar` from `from` on.
fn truncate(events: &mut Vec<StoredEvent>, calendar: &str, series: &str, from: Cut) {
    events.retain(|stored| {
        stored.calendar_id != calendar
            || stored.event.recurring_event_id.as_deref() != Some(series)
            || stored
                .event
                .start
                .as_ref()
                .is_none_or(|start| from.keeps(start))
    });
}

/// `event` in place of its copy in `events`, or added to them when `shown` says so.
fn upsert(
    events: &mut Vec<StoredEvent>,
    calendar: &str,
    event: &Event,
    unsynced: bool,
    shown: &dyn Fn(&str, &Event) -> bool,
) {
    match events
        .iter_mut()
        .find(|stored| stored.calendar_id == calendar && stored.event.id == event.id)
    {
        Some(stored) => stored.event = event.clone(),
        None if shown(calendar, event) => events.push(StoredEvent {
            calendar_id: calendar.to_owned(),
            event: event.clone(),
            unsynced,
        }),
        None => {}
    }
}

/// Sets my answer to `event` on the attendee that's me or has my calendar's address.
fn answer(event: &mut Event, calendar: &str, status: &str) {
    for attendee in &mut event.attendees {
        if attendee.is_self == Some(true)
            || attendee
                .email
                .as_deref()
                .is_some_and(|email| email.eq_ignore_ascii_case(calendar))
        {
            attendee.response_status = Some(status.to_owned());
        }
    }
}

/// Every queued change, oldest first.
pub(crate) async fn queued(exec: &mut dyn Executor) -> Result<Vec<CalendarChange>> {
    let rows = CalendarChangeRow::all()
        .order_by(CalendarChangeRow::fields().id().asc())
        .exec(exec)
        .await?;
    Ok(rows.into_iter().map(|row| row.change.0).collect())
}

impl Store {
    /// Queues `change` under `token` for Calendar, shown by [`events`](Self::events) at once and
    /// sent once `hold` has passed. A deletion of a new event still queued takes it back
    /// instead, so Calendar and its guests never hear of it; if that new event is already in
    /// flight to Calendar, it waits for the send and queues a compensating deletion if needed.
    ///
    /// # Errors
    /// On database failures.
    pub async fn queue_calendar_change(
        &self,
        token: u64,
        mut change: CalendarChange,
        hold: Option<Timestamp>,
    ) -> Result<()> {
        if let CalendarChange::Split { patch, .. } = &mut change
            && patch.id.is_none()
        {
            patch.id = Some(fresh_event_id());
        }
        let sync = &self.inner.calendar_outbox;
        let queue = change.queue();
        let mut db = self.inner.db.clone();
        let mut write = self.inner.begin(&mut db).await?;
        if let Some(row_id) = find_queued_create(write.exec(), &queue, &change).await? {
            let is_sending = {
                let mut state = sync
                    .in_flight
                    .lock()
                    .unwrap_or_else(PoisonError::into_inner);
                if state.sending == Some(row_id) {
                    state.watched_creates.entry(row_id).or_insert(false);
                    true
                } else {
                    false
                }
            };
            if !is_sending {
                CalendarChangeRow::filter(CalendarChangeRow::fields().queue().eq(&queue))
                    .delete()
                    .exec(write.exec())
                    .await?;
                return write.publish().await;
            }
            drop(write);
            let _flush = sync.flush.lock().await;
            let sent = sync
                .in_flight
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .watched_creates
                .remove(&row_id)
                == Some(true);
            let mut write = self.inner.begin(&mut db).await?;
            CalendarChangeRow::filter(CalendarChangeRow::fields().queue().eq(&queue))
                .delete()
                .exec(write.exec())
                .await?;
            if sent {
                CalendarChangeRow::create()
                    .token(token)
                    .queue(queue)
                    .change(Json(change))
                    .failures(0)
                    .not_before(hold)
                    .exec(write.exec())
                    .await?;
            }
            return write.publish().await;
        }
        CalendarChangeRow::create()
            .token(token)
            .queue(queue)
            .change(Json(change))
            .failures(0)
            .not_before(hold)
            .exec(write.exec())
            .await?;
        write.publish().await
    }

    /// Takes back the change queued under `token`, unless it's gone. For a new event, it also
    /// takes back what was queued for the event since (its edits); if the new event's creation is
    /// already in flight to Calendar and lands, a compensating deletion is queued in its place.
    /// Returns whether the change was taken back.
    ///
    /// # Errors
    /// On database failures.
    pub async fn unqueue_calendar_change(&self, token: u64) -> Result<bool> {
        let sync = &self.inner.calendar_outbox;
        let mut db = self.inner.db.clone();
        let mut write = self.inner.begin(&mut db).await?;
        let rows = CalendarChangeRow::filter(CalendarChangeRow::fields().token().eq(token))
            .exec(write.exec())
            .await?;
        if rows.is_empty() {
            return Ok(false);
        }
        let in_flight = {
            let mut state = sync
                .in_flight
                .lock()
                .unwrap_or_else(PoisonError::into_inner);
            rows.iter().find_map(|row| {
                if state.sending != Some(row.id) {
                    return None;
                }
                if matches!(row.change.0, CalendarChange::Create { .. }) {
                    state.watched_creates.entry(row.id).or_insert(false);
                }
                Some((row.id, row.queue.clone(), row.change.0.clone()))
            })
        };
        if let Some((row_id, queue_key, in_flight_change)) = in_flight {
            drop(write);
            let _flush = sync.flush.lock().await;
            let sent_create = sync
                .in_flight
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .watched_creates
                .remove(&row_id)
                == Some(true);
            let mut write = self.inner.begin(&mut db).await?;
            if sent_create
                && let CalendarChange::Create { calendar, event } = in_flight_change
                && let Some(event_id) = event.id
            {
                CalendarChangeRow::filter(CalendarChangeRow::fields().queue().eq(&queue_key))
                    .delete()
                    .exec(write.exec())
                    .await?;
                let notify = event.attendees.iter().any(|attendee| {
                    attendee.is_self != Some(true)
                        && !attendee
                            .email
                            .as_deref()
                            .is_some_and(|email| email.eq_ignore_ascii_case(&calendar))
                });
                let compensating = CalendarChange::Delete {
                    calendar,
                    id: event_id,
                    notify,
                };
                CalendarChangeRow::create()
                    .token(0)
                    .queue(queue_key)
                    .change(Json(compensating))
                    .failures(0)
                    .not_before(None)
                    .exec(write.exec())
                    .await?;
                write.publish().await?;
                return Ok(true);
            }
            let remaining =
                CalendarChangeRow::filter(CalendarChangeRow::fields().token().eq(token))
                    .exec(write.exec())
                    .await?;
            if remaining.is_empty() {
                return Ok(false);
            }
            delete_unstarted_rows(write.exec(), &remaining, token).await?;
            write.publish().await?;
            return Ok(true);
        }
        delete_unstarted_rows(write.exec(), &rows, token).await?;
        write.publish().await?;
        Ok(true)
    }

    /// The queued changes whose turn it is, oldest first. That's each queue's first change,
    /// unless it's held or backing off. With `held_too`, it's each queue's first change, whatever
    /// it waits for. Also returns when the next waiting change is due.
    ///
    /// # Errors
    /// On database failures.
    pub async fn due_calendar_changes(
        &self,
        held_too: bool,
    ) -> Result<(Vec<QueuedCalendarChange>, Option<Timestamp>)> {
        let mut db = self.inner.db.clone();
        let rows = CalendarChangeRow::all()
            .order_by(CalendarChangeRow::fields().id().asc())
            .exec(&mut db)
            .await?;
        let batch = queue::select_due(
            rows,
            Timestamp::now(),
            held_too,
            |row| row.queue.clone(),
            |row| row.not_before,
        );
        let due = batch
            .due
            .into_iter()
            .map(|row| QueuedCalendarChange {
                id: row.id,
                token: row.token,
                change: row.change.0,
            })
            .collect();
        Ok((due, batch.retry_at))
    }

    /// How many queued changes Calendar doesn't have yet.
    ///
    /// # Errors
    /// On database failures.
    pub async fn calendar_pending(&self) -> Result<usize> {
        let mut db = self.inner.db.clone();
        Ok(
            usize::try_from(CalendarChangeRow::all().count().exec(&mut db).await?)
                .unwrap_or(usize::MAX),
        )
    }

    /// Sends `queued` to Calendar, caches Calendar's answer, and drops the change from the queue.
    /// Otherwise it postpones the change, or gives it up.
    ///
    /// # Errors
    /// On database failures.
    pub async fn send_calendar_change(
        &self,
        queued: &QueuedCalendarChange,
    ) -> Result<CalendarOutcome> {
        let sync = &self.inner.calendar_outbox;
        let _flush = sync.flush.lock().await;
        let current_failures = {
            let mut db = self.inner.db.clone();
            let mut write = self.inner.begin(&mut db).await?;
            let Some(row) = CalendarChangeRow::filter_by_id(queued.id)
                .first()
                .exec(write.exec())
                .await?
            else {
                return Ok(CalendarOutcome::Sent);
            };
            sync.in_flight
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .sending = Some(queued.id);
            row.failures
        };
        let mut guard = SendingGuard { sync, active: true };
        let is_create = matches!(queued.change, CalendarChange::Create { .. });
        let finish = |sent_create: bool, guard: &mut SendingGuard<'_>| {
            guard.active = false;
            let mut state = sync
                .in_flight
                .lock()
                .unwrap_or_else(PoisonError::into_inner);
            state.sending = None;
            if sent_create && let Some(flag) = state.watched_creates.get_mut(&queued.id) {
                *flag = true;
            }
        };
        let failure = match self.make(&queued.change).await {
            Ok(()) => {
                let outcome = self.dequeue(queued.id, CalendarOutcome::Sent).await?;
                finish(is_create, &mut guard);
                return Ok(outcome);
            }
            Err(failure) => failure,
        };
        let failures = current_failures.saturating_add(1);
        let not_before = match queue::dispose_error(&self.inner.api, failure, failures) {
            // Calendar's quota is its own, so only Calendar's own pushback holds a change.
            Ok(Disposition::Postponed(until)) => until,
            Ok(Disposition::Refused(error)) => {
                log::warn!("Calendar refused a change, giving it up: {error}");
                let organizer_only = matches!(&error, gmail::Error::Api(api)
                    if api.errors.iter().any(|item| item.reason == "forbiddenForNonOrganizer"));
                let refusal = match error.status() {
                    Some(404 | 410) => Refusal::Gone,
                    Some(403) if organizer_only => Refusal::NonOrganizer,
                    _ => Refusal::Other,
                };
                let outcome = self
                    .dequeue(
                        queued.id,
                        CalendarOutcome::Refused {
                            error: error.to_string(),
                            refusal,
                        },
                    )
                    .await?;
                finish(false, &mut guard);
                return Ok(outcome);
            }
            Err(error @ (Error::EventCancelled | Error::UnknownSender)) => {
                log::warn!("giving up an answer to an invite: {error}");
                let refusal = if matches!(error, Error::EventCancelled) {
                    Refusal::Gone
                } else {
                    Refusal::UnknownSender
                };
                let outcome = self
                    .dequeue(
                        queued.id,
                        CalendarOutcome::Refused {
                            error: error.to_string(),
                            refusal,
                        },
                    )
                    .await?;
                finish(false, &mut guard);
                return Ok(outcome);
            }
            Err(error) => return Err(error),
        };
        let mut db = self.inner.db.clone();
        let mut write = self.inner.begin(&mut db).await?;
        CalendarChangeRow::update_by_id(queued.id)
            .failures(failures)
            .not_before(not_before)
            .exec(write.exec())
            .await?;
        write.commit().await?;
        finish(false, &mut guard);
        Ok(CalendarOutcome::Postponed)
    }

    /// Drops change `id` from its queue, which ended in `outcome`.
    async fn dequeue(&self, id: u64, outcome: CalendarOutcome) -> Result<CalendarOutcome> {
        let mut db = self.inner.db.clone();
        let mut write = self.inner.begin(&mut db).await?;
        CalendarChangeRow::filter_by_id(id)
            .delete()
            .exec(write.exec())
            .await?;
        write.publish().await?;
        Ok(outcome)
    }

    /// Makes `change` at Calendar, and caches what it changed. That's Calendar's answer, or for a
    /// series, its instances as they were shown, until the next fetch brings Calendar's.
    async fn make(&self, change: &CalendarChange) -> Result<()> {
        match change {
            CalendarChange::Create { calendar, event } => {
                self.insert_event(calendar, event, true).await.map(drop)
            }
            CalendarChange::Edit {
                calendar,
                id,
                patch,
                clear_attendees,
                notify,
                shift,
                ..
            } => {
                let mut patch = patch.clone();
                patch.clear_attendees = *clear_attendees;
                if let Some(shift) = shift {
                    shifted(&mut patch, &self.fetch_event(calendar, id).await?, *shift);
                }
                let answer = self.patch_event(calendar, id, &patch, *notify).await?;
                if answer.recurrence.is_empty() {
                    Ok(())
                } else {
                    self.fold_into_cache(change).await
                }
            }
            CalendarChange::Delete {
                calendar,
                id,
                notify,
            } => self.delete_event(calendar, id, *notify).await,
            CalendarChange::Split {
                calendar,
                series,
                from,
                patch,
                clear_attendees,
                start,
                end,
                notify,
                ..
            } => {
                let master = self.fetch_event(calendar, series).await?;
                let mut patch = patch.clone();
                patch.clear_attendees = *clear_attendees;
                let (start, end) = (
                    patch.start.clone().unwrap_or_else(|| start.clone()),
                    patch.end.clone().unwrap_or_else(|| end.clone()),
                );
                if master
                    .start
                    .as_ref()
                    .is_none_or(|first| !from.keeps(first) || first == &start)
                {
                    // From the first instance, the series itself changes.
                    let mut first_patch = Event {
                        id: None,
                        start: Some(start.clone()),
                        end: Some(end),
                        ..patch
                    };
                    if let Some(old_start) = &master.start
                        && old_start.date.is_some() != start.date.is_some()
                        && first_patch.recurrence.is_empty()
                        && !master.recurrence.is_empty()
                        && let Some((old_zoned, _)) =
                            super::recurrence::zoned(old_start, &TimeZone::UTC)
                        && let Some((target_zoned, all_day)) =
                            super::recurrence::zoned(&start, old_zoned.time_zone())
                    {
                        first_patch.recurrence = super::recurrence::retime_until(
                            &master.recurrence,
                            &target_zoned,
                            all_day,
                            old_zoned.time_zone(),
                        );
                    }
                    self.patch_event(calendar, series, &first_patch, *notify)
                        .await?;
                    return self.fold_into_cache(change).await;
                }
                // Insert the new series first (with its chosen `patch.id`) so a retry after a
                // partial failure gets 409 Conflict instead of copying the truncated `UNTIL` from
                // `master`.
                let mut new_series = remainder(&master, &patch, start, end, *from);
                if new_series.id.is_none() {
                    new_series.id = Some(fresh_event_id());
                }
                self.insert_event(calendar, &new_series, *notify).await?;
                let truncated = Event {
                    recurrence: ended(&master.recurrence, *from),
                    ..Event::default()
                };
                self.patch_event(calendar, series, &truncated, *notify)
                    .await?;
                self.fold_into_cache(change).await
            }
            CalendarChange::Truncate {
                calendar,
                series,
                from,
                notify,
            } => {
                let master = self.fetch_event(calendar, series).await?;
                if master.start.as_ref().is_none_or(|first| !from.keeps(first)) {
                    return self.delete_event(calendar, series, *notify).await;
                }
                let patch = Event {
                    recurrence: ended(&master.recurrence, *from),
                    ..Event::default()
                };
                self.patch_event(calendar, series, &patch, *notify).await?;
                self.fold_into_cache(change).await
            }
            CalendarChange::Respond {
                calendar,
                event,
                status,
                all,
            } => {
                let series = event.recurring_event_id.as_deref().filter(|_| *all);
                self.respond(
                    calendar,
                    series.or(event.id.as_deref()).unwrap_or_default(),
                    status,
                )
                .await?;
                if series.is_some() {
                    self.fold_into_cache(change).await
                } else {
                    Ok(())
                }
            }
            CalendarChange::RespondToInvite {
                calendar,
                invite,
                status,
                links,
                ..
            } => {
                let uid = invite.ical_uid.clone().unwrap_or_default();
                let event = match self.event_by_uid(calendar, &uid).await? {
                    Some(event) if event.status.as_deref() == Some("cancelled") => {
                        return Err(Error::EventCancelled);
                    }
                    Some(event) => event,
                    // Calendar won't take an unknown sender's event, which the mail answers.
                    None if links.is_some() => return Err(Error::UnknownSender),
                    None => self.import_event(calendar, invite).await?,
                };
                self.respond(
                    calendar,
                    event
                        .recurring_event_id
                        .as_deref()
                        .or(event.id.as_deref())
                        .unwrap_or_default(),
                    status,
                )
                .await?;
                self.fold_into_cache(change).await
            }
        }
    }

    /// Makes `change` to the cached events, as Calendar now has it.
    async fn fold_into_cache(&self, change: &CalendarChange) -> Result<()> {
        let calendar = change.calendar();
        let mut db = self.inner.db.clone();
        let rows = EventRow::filter(EventRow::fields().calendar_id().eq(calendar))
            .exec(&mut db)
            .await?;
        let before: HashMap<String, Event> =
            rows.into_iter().map(|row| (row.id, row.event.0)).collect();
        let mut after: Vec<StoredEvent> = before
            .values()
            .map(|event| StoredEvent {
                calendar_id: calendar.to_owned(),
                event: event.clone(),
                unsynced: false,
            })
            .collect();
        // A split truncates the old series' future instances and replaces them with `shown`
        // instances carrying new ids, so those replacements must be inserted into the cache.
        let include_new = matches!(change, CalendarChange::Split { .. });
        change.apply(&mut after, &|_, _| include_new, None);
        let kept: HashSet<&str> = after
            .iter()
            .filter_map(|stored| stored.event.id.as_deref())
            .collect();
        let gone: Vec<String> = before
            .keys()
            .filter(|id| !kept.contains(id.as_str()))
            .cloned()
            .collect();
        let mut write = self.inner.begin(&mut db).await?;
        for ids in gone.chunks(500) {
            EventRow::filter(
                EventRow::fields()
                    .calendar_id()
                    .eq(calendar)
                    .and(EventRow::fields().id().in_list(ids)),
            )
            .delete()
            .exec(write.exec())
            .await?;
        }
        for stored in &after {
            if stored.event.id.as_ref().and_then(|id| before.get(id)) != Some(&stored.event) {
                put(write.exec(), calendar, &stored.event).await?;
            }
        }
        write.publish().await
    }
}

/// Returns the row ID of a queued `Create` in `queue` matching a `Delete` `change`, if any.
async fn find_queued_create(
    exec: &mut dyn Executor,
    queue: &str,
    change: &CalendarChange,
) -> Result<Option<u64>> {
    let CalendarChange::Delete { calendar, id, .. } = change else {
        return Ok(None);
    };
    let rows = CalendarChangeRow::filter(CalendarChangeRow::fields().queue().eq(queue))
        .exec(&mut *exec)
        .await?;
    Ok(rows.into_iter().find_map(|row| match &row.change.0 {
        CalendarChange::Create {
            calendar: on,
            event,
        } if on == calendar && event.id.as_deref() == Some(id.as_str()) => Some(row.id),
        _ => None,
    }))
}

async fn delete_unstarted_rows(
    exec: &mut dyn Executor,
    rows: &[CalendarChangeRow],
    token: u64,
) -> Result<()> {
    for row in rows
        .iter()
        .filter(|row| matches!(row.change.0, CalendarChange::Create { .. }))
    {
        let queue = CalendarChangeRow::fields().queue().eq(&row.queue);
        CalendarChangeRow::filter(queue.and(CalendarChangeRow::fields().id().gt(row.id)))
            .delete()
            .exec(&mut *exec)
            .await?;
    }
    CalendarChangeRow::filter(CalendarChangeRow::fields().token().eq(token))
        .delete()
        .exec(exec)
        .await?;
    Ok(())
}

/// `recurrence` with each rule ending before `from` (`UNTIL`, in place of a count or an
/// earlier end).
fn ended(recurrence: &[String], from: Cut) -> Vec<String> {
    let until = from.until();
    recurrence
        .iter()
        .map(|line| match line.strip_prefix("RRULE:") {
            Some(rule) => {
                let mut parts: Vec<String> = rule
                    .split(';')
                    .filter(|part| !part.starts_with("COUNT=") && !part.starts_with("UNTIL="))
                    .map(str::to_owned)
                    .collect();
                parts.push(format!("UNTIL={until}"));
                format!("RRULE:{}", parts.join(";"))
            }
            None => line.clone(),
        })
        .collect()
}

/// The new series that takes over series `master` from `from`. It has the master's fields with
/// `patch`'s laid over them, a new id (`patch`'s), and Calendar's own fields left to Calendar.
/// It starts at `start`, and its rule counts only what's left.
fn remainder(
    master: &Event,
    patch: &Event,
    start: EventDateTime,
    end: EventDateTime,
    from: Cut,
) -> Event {
    let mut event = Event {
        id: patch.id.clone(),
        summary: patch.summary.clone().or_else(|| master.summary.clone()),
        description: patch
            .description
            .clone()
            .or_else(|| master.description.clone()),
        location: patch.location.clone().or_else(|| master.location.clone()),
        attendees: if patch.attendees.is_empty() && !patch.clear_attendees {
            master.attendees.clone()
        } else {
            patch.attendees.clone()
        },
        conference_data: match &patch.conference_data {
            Some(serde_json::Value::Null) => None,
            Some(data) => Some(data.clone()),
            None => master.conference_data.clone(),
        },
        color_id: patch.color_id.clone().or_else(|| master.color_id.clone()),
        transparency: patch
            .transparency
            .clone()
            .or_else(|| master.transparency.clone()),
        visibility: patch
            .visibility
            .clone()
            .or_else(|| master.visibility.clone()),
        reminders: patch.reminders.clone().or_else(|| master.reminders.clone()),
        guests_can_invite_others: patch
            .guests_can_invite_others
            .or(master.guests_can_invite_others),
        guests_can_modify: patch.guests_can_modify.or(master.guests_can_modify),
        guests_can_see_other_guests: patch
            .guests_can_see_other_guests
            .or(master.guests_can_see_other_guests),
        recurrence: if patch.recurrence.is_empty() {
            master.recurrence.clone()
        } else {
            patch.recurrence.clone()
        },
        ..Event::default()
    };
    let all_day = start.date.is_some();
    if patch.recurrence.is_empty() {
        let first = master
            .start
            .as_ref()
            .and_then(|time| super::recurrence::zoned(time, &TimeZone::UTC));
        let new_start = super::recurrence::zoned(&start, &TimeZone::UTC);
        if let (Some((first, _)), Some((new_start, _))) = (first, new_start)
            && let Some(rule) =
                super::recurrence::Recurrence::from_rules_for(&master.recurrence, &first)
        {
            let day = match from {
                Cut::Day(day) => day,
                Cut::At(at) => at.to_zoned(first.time_zone().clone()).date(),
            };
            let line = rule.from_day(&first, day).rule(&new_start, all_day);
            event.recurrence = master
                .recurrence
                .iter()
                .filter(|line| !line.starts_with("RRULE"))
                .cloned()
                .chain([line])
                .collect();
        }
    }
    event.start = Some(start);
    event.end = Some(end);
    event
}

/// Sets `patch`'s start and end to series `master`'s, moved by `shift` (an instance's civil
/// wall-clock move, applied to every instance), in the master's own time zone, and rewrites
/// `UNTIL` when `all_day` toggles.
fn shifted(patch: &mut Event, master: &Event, (start, end): (SignedDuration, SignedDuration)) {
    let start_template = patch.start.clone();
    let end_template = patch.end.clone();
    let new_start = retime_event_time(master.start.as_ref(), start, start_template.as_ref());
    let new_end = retime_event_time(master.end.as_ref(), end, end_template.as_ref());
    if let (Some(old_start), Some(target_start)) = (&master.start, &new_start)
        && old_start.date.is_some() != target_start.date.is_some()
        && patch.recurrence.is_empty()
        && !master.recurrence.is_empty()
        && let Some((old_zoned, _)) = super::recurrence::zoned(old_start, &TimeZone::UTC)
        && let Some((target_zoned, all_day)) =
            super::recurrence::zoned(target_start, old_zoned.time_zone())
    {
        patch.recurrence = super::recurrence::retime_until(
            &master.recurrence,
            &target_zoned,
            all_day,
            old_zoned.time_zone(),
        );
    }
    patch.start = new_start.or(start_template);
    patch.end = new_end.or(end_template);
}

#[cfg(test)]
mod tests {
    use gmail::calendar::EventAttendee;
    use jiff::civil::Date;

    use super::*;

    fn at(text: &str) -> EventDateTime {
        EventDateTime {
            date_time: Some(text.into()),
            time_zone: Some("America/New_York".into()),
            ..EventDateTime::default()
        }
    }

    fn on_date(text: &str) -> EventDateTime {
        EventDateTime {
            date: Some(text.into()),
            ..EventDateTime::default()
        }
    }

    #[test]
    fn a_series_moves_by_its_instance() {
        let master = Event {
            start: Some(at("2026-11-03T11:00:00-05:00")),
            end: Some(at("2026-11-03T11:30:00-05:00")),
            ..Event::default()
        };
        let mut patch = Event::default();
        shifted(
            &mut patch,
            &master,
            (SignedDuration::from_mins(60), SignedDuration::from_mins(90)),
        );
        assert_eq!(
            patch.start.and_then(|start| start.date_time).as_deref(),
            Some("2026-11-03T12:00:00-05:00")
        );
        assert_eq!(
            patch.end.and_then(|end| end.date_time).as_deref(),
            Some("2026-11-03T13:00:00-05:00")
        );
    }

    #[test]
    fn series_shifts_preserve_wall_clock_across_dst_and_support_all_day_and_toggles() {
        // Master starts before fall-back (EDT, -04:00); shifting wall-clock +1h keeps 11:00 EDT.
        let master = Event {
            start: Some(at("2026-10-27T10:00:00-04:00")),
            end: Some(at("2026-10-27T10:30:00-04:00")),
            recurrence: vec!["RRULE:FREQ=WEEKLY;UNTIL=20261202T045959Z".into()],
            ..Event::default()
        };
        let mut patch = Event::default();
        shifted(
            &mut patch,
            &master,
            (SignedDuration::from_hours(1), SignedDuration::from_hours(1)),
        );
        assert_eq!(
            patch.start.and_then(|start| start.date_time).as_deref(),
            Some("2026-10-27T11:00:00-04:00")
        );

        // Toggling timed -> all-day on `Scope::All` converts master to date and rewrites `UNTIL`.
        let mut to_all_day = Event {
            start: Some(on_date("2026-11-17")),
            end: Some(on_date("2026-11-18")),
            ..Event::default()
        };
        shifted(
            &mut to_all_day,
            &master,
            (
                SignedDuration::from_hours(-10),
                SignedDuration::from_mins(13 * 60 + 30),
            ),
        );
        assert_eq!(
            to_all_day.start.and_then(|start| start.date).as_deref(),
            Some("2026-10-27")
        );
        assert_eq!(
            to_all_day.end.and_then(|end| end.date).as_deref(),
            Some("2026-10-28")
        );
        assert_eq!(
            to_all_day.recurrence,
            ["RRULE:FREQ=WEEKLY;UNTIL=20261201"],
            "rewrites UNTIL in America/New_York without +1 day UTC drift"
        );

        // All-day series shifts civil dates.
        let all_day_master = Event {
            start: Some(on_date("2026-10-27")),
            end: Some(on_date("2026-10-28")),
            ..Event::default()
        };
        assert_eq!(
            shift_event_time(all_day_master.end.as_ref(), SignedDuration::from_hours(24))
                .and_then(|end| end.date)
                .as_deref(),
            Some("2026-10-29")
        );
    }

    #[test]
    fn remainder_clears_attendees_when_requested_and_keeps_master_attendees_otherwise() {
        let guest = EventAttendee {
            email: Some("lena@brightmoor.co".into()),
            ..EventAttendee::default()
        };
        let master = Event {
            id: Some("series1".into()),
            summary: Some("Sync".into()),
            start: Some(at("2026-11-10T10:00:00-05:00")),
            end: Some(at("2026-11-10T10:30:00-05:00")),
            attendees: vec![guest.clone()],
            recurrence: vec!["RRULE:FREQ=WEEKLY;BYDAY=TU;UNTIL=20261209T045959Z".into()],
            ..Event::default()
        };
        let cut = Cut::At("2026-11-17T15:00:00Z".parse().expect("time"));
        let untouched = remainder(
            &master,
            &Event {
                id: Some("series2".into()),
                summary: Some("Sync, renamed".into()),
                ..Event::default()
            },
            at("2026-11-17T10:00:00-05:00"),
            at("2026-11-17T10:30:00-05:00"),
            cut,
        );
        assert_eq!(untouched.attendees, vec![guest]);
        assert_eq!(
            untouched.recurrence,
            ["RRULE:FREQ=WEEKLY;BYDAY=TU;WKST=SU;UNTIL=20261209T045959Z"],
            "UNTIL stays Dec 8 in America/New_York rather than drifting +1 day"
        );

        let cleared = remainder(
            &master,
            &Event {
                id: Some("series3".into()),
                clear_attendees: true,
                ..Event::default()
            },
            at("2026-11-17T10:00:00-05:00"),
            at("2026-11-17T10:30:00-05:00"),
            cut,
        );
        assert!(cleared.attendees.is_empty());
    }

    #[test]
    fn a_series_ends_before_an_instance() {
        let from = Cut::At("2026-11-24T16:00:00Z".parse().expect("time"));
        let rules = ended(
            &[
                "RRULE:FREQ=WEEKLY;COUNT=10;BYDAY=TU".to_owned(),
                "EXDATE:20261110T160000Z".to_owned(),
            ],
            from,
        );
        assert_eq!(
            rules,
            [
                "RRULE:FREQ=WEEKLY;BYDAY=TU;UNTIL=20261124T155959Z",
                "EXDATE:20261110T160000Z"
            ]
        );
        assert_eq!(
            ended(
                &["RRULE:FREQ=WEEKLY".to_owned()],
                Cut::Day(Date::constant(2026, 11, 17))
            ),
            ["RRULE:FREQ=WEEKLY;UNTIL=20261116"],
            "an all-day series ends on a date"
        );
    }

    #[test]
    fn queued_changes_show_on_the_calendar() {
        let event = |id: &str, series: Option<&str>| StoredEvent {
            calendar_id: "me".into(),
            event: Event {
                id: Some(id.into()),
                recurring_event_id: series.map(Into::into),
                ..Event::default()
            },
            unsynced: false,
        };
        let everywhere = |_: &str, _: &Event| true;
        let mut events = vec![
            event("a", None),
            event("s_1", Some("s")),
            event("s_2", Some("s")),
        ];
        CalendarChange::Delete {
            calendar: "me".into(),
            id: "s".into(),
            notify: false,
        }
        .apply(&mut events, &everywhere, None);
        assert_eq!(events.len(), 1, "a series takes its instances");
        let renamed = Event {
            id: Some("a".into()),
            summary: Some("Renamed".into()),
            ..Event::default()
        };
        CalendarChange::Edit {
            calendar: "me".into(),
            id: "a".into(),
            patch: Event::default(),
            clear_attendees: false,
            notify: false,
            shift: None,
            shown: vec![renamed.clone()],
        }
        .apply(&mut events, &everywhere, None);
        assert_eq!(events[0].event, renamed);
        let new = CalendarChange::Create {
            calendar: "me".into(),
            event: Event {
                id: Some("b".into()),
                ..Event::default()
            },
        };
        new.apply(&mut events, &|_, _| false, None);
        assert_eq!(events.len(), 1, "a new event off the days shown isn't");
        new.apply(&mut events, &everywhere, None);
        assert_eq!(
            (events.len(), events[1].unsynced),
            (2, true),
            "a new event on them is, and Calendar hasn't got it"
        );
    }

    #[test]
    fn an_instance_shares_its_series_queue() {
        let respond = |id: &str| CalendarChange::Respond {
            calendar: "me".into(),
            event: Event {
                id: Some(id.into()),
                ..Event::default()
            },
            status: "accepted".into(),
            all: false,
        };
        let delete = CalendarChange::Delete {
            calendar: "me".into(),
            id: "s".into(),
            notify: false,
        };
        assert_eq!(respond("s_20261117T150000Z").queue(), delete.queue());
        assert_ne!(respond("t").queue(), delete.queue());
    }

    #[tokio::test]
    async fn in_flight_creates_wait_and_queue_compensating_deletes_and_splits_get_deterministic_ids()
     {
        let dir = std::env::temp_dir().join(format!(
            "exo-cal-outbox-test-{}-{}",
            std::process::id(),
            fresh_event_id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("temp dir");
        let store = Store::open(
            &dir,
            "maya@brightmoor.co",
            gmail::Endpoints::google(),
            gmail::StaticToken("token".into()),
        )
        .await
        .expect("opens store");

        // 1. Queueing a Split without a `patch.id` assigns a deterministic id in the row.
        store
            .queue_calendar_change(
                1,
                CalendarChange::Split {
                    calendar: "maya@brightmoor.co".into(),
                    series: "series1".into(),
                    from: Cut::Day(Date::constant(2026, 11, 17)),
                    patch: Event::default(),
                    clear_attendees: false,
                    start: on_date("2026-11-17"),
                    end: on_date("2026-11-18"),
                    notify: false,
                    shown: Vec::new(),
                },
                None,
            )
            .await
            .expect("queues split");
        let (due_first, _) = store.due_calendar_changes(true).await.expect("reads");
        let (due_second, _) = store.due_calendar_changes(true).await.expect("reads again");
        let CalendarChange::Split { patch: p1, .. } = &due_first[0].change else {
            panic!("expected Split");
        };
        let CalendarChange::Split { patch: p2, .. } = &due_second[0].change else {
            panic!("expected Split");
        };
        assert!(p1.id.is_some());
        assert_eq!(p1.id, p2.id);
        assert!(store.unqueue_calendar_change(1).await.expect("unqueues"));

        // `ended` is idempotent on retry even if `master` already carries `UNTIL`.
        let once = ended(
            &["RRULE:FREQ=WEEKLY;COUNT=6;BYDAY=TU".into()],
            Cut::At("2026-11-17T15:00:00Z".parse().expect("time")),
        );
        let twice = ended(
            &once,
            Cut::At("2026-11-17T15:00:00Z".parse().expect("time")),
        );
        assert_eq!(once, twice);

        // 2. Simulate an in-flight `Create` when `unqueue_calendar_change` runs concurrently.
        store
            .queue_calendar_change(
                2,
                CalendarChange::Create {
                    calendar: "maya@brightmoor.co".into(),
                    event: Event {
                        id: Some("e0inflight1".into()),
                        summary: Some("Sync".into()),
                        start: Some(at("2026-11-20T09:00:00-05:00")),
                        end: Some(at("2026-11-20T10:00:00-05:00")),
                        ..Event::default()
                    },
                },
                None,
            )
            .await
            .expect("queues create");
        let (due, _) = store.due_calendar_changes(true).await.expect("reads");
        let row_id = due[0].id;
        let sync = &store.inner.calendar_outbox;
        let flush_guard = sync.flush.lock().await;
        sync.in_flight
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .sending = Some(row_id);

        let store_for_undo = store.clone();
        let undo_task = tokio::spawn(async move {
            store_for_undo
                .unqueue_calendar_change(2)
                .await
                .expect("undoes")
        });
        tokio::task::yield_now().await;
        tokio::time::sleep(std::time::Duration::from_millis(15)).await;

        // Finish the simulated in-flight `Create` (dequeue + mark watched create as sent).
        store
            .dequeue(row_id, CalendarOutcome::Sent)
            .await
            .expect("dequeues");
        {
            let mut state = sync
                .in_flight
                .lock()
                .unwrap_or_else(PoisonError::into_inner);
            state.sending = None;
            if let Some(flag) = state.watched_creates.get_mut(&row_id) {
                *flag = true;
            }
        }
        drop(flush_guard);

        assert!(undo_task.await.expect("joins"));
        let (compensating, _) = store.due_calendar_changes(true).await.expect("reads");
        assert_eq!(
            compensating.len(),
            1,
            "compensating Delete was queued instead of leaving a phantom event"
        );
        assert_eq!(
            compensating[0].change,
            CalendarChange::Delete {
                calendar: "maya@brightmoor.co".into(),
                id: "e0inflight1".into(),
                notify: false,
            }
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn remainder_overlays_patch_metadata_and_recurrence() {
        let master = Event {
            id: Some("series1".into()),
            summary: Some("Weekly".into()),
            start: Some(at("2026-11-10T10:00:00-05:00")),
            end: Some(at("2026-11-10T10:30:00-05:00")),
            color_id: Some("1".into()),
            transparency: Some("opaque".into()),
            visibility: Some("default".into()),
            reminders: Some(gmail::calendar::EventReminders {
                use_default: Some(true),
                overrides: Vec::new(),
            }),
            guests_can_invite_others: Some(true),
            guests_can_modify: Some(false),
            guests_can_see_other_guests: Some(true),
            recurrence: vec!["RRULE:FREQ=WEEKLY;BYDAY=TU;COUNT=6".into()],
            ..Event::default()
        };
        let patch = Event {
            id: Some("series2".into()),
            color_id: Some("5".into()),
            transparency: Some("transparent".into()),
            visibility: Some("private".into()),
            reminders: Some(gmail::calendar::EventReminders {
                use_default: Some(false),
                overrides: Vec::new(),
            }),
            guests_can_invite_others: Some(false),
            guests_can_modify: Some(true),
            guests_can_see_other_guests: Some(false),
            recurrence: vec!["RRULE:FREQ=DAILY;COUNT=3".into()],
            ..Event::default()
        };
        let cut = Cut::At("2026-11-17T15:00:00Z".parse().expect("time"));
        let built = remainder(
            &master,
            &patch,
            at("2026-11-17T10:00:00-05:00"),
            at("2026-11-17T10:30:00-05:00"),
            cut,
        );
        assert_eq!(built.color_id.as_deref(), Some("5"));
        assert_eq!(built.transparency.as_deref(), Some("transparent"));
        assert_eq!(built.visibility.as_deref(), Some("private"));
        assert_eq!(
            built.reminders,
            Some(gmail::calendar::EventReminders {
                use_default: Some(false),
                overrides: Vec::new(),
            })
        );
        assert_eq!(built.guests_can_invite_others, Some(false));
        assert_eq!(built.guests_can_modify, Some(true));
        assert_eq!(built.guests_can_see_other_guests, Some(false));
        assert_eq!(built.recurrence, ["RRULE:FREQ=DAILY;COUNT=3"]);

        // Omitted patch metadata falls back to master.
        let fallback = remainder(
            &master,
            &Event {
                id: Some("series3".into()),
                ..Event::default()
            },
            at("2026-11-17T10:00:00-05:00"),
            at("2026-11-17T10:30:00-05:00"),
            cut,
        );
        assert_eq!(fallback.color_id.as_deref(), Some("1"));
        assert_eq!(fallback.transparency.as_deref(), Some("opaque"));
        assert_eq!(fallback.visibility.as_deref(), Some("default"));
        assert_eq!(fallback.reminders, master.reminders);
        assert_eq!(fallback.guests_can_invite_others, Some(true));
        assert_eq!(fallback.guests_can_modify, Some(false));
        assert_eq!(fallback.guests_can_see_other_guests, Some(true));
        assert_eq!(
            fallback.recurrence,
            ["RRULE:FREQ=WEEKLY;BYDAY=TU;WKST=SU;COUNT=5"]
        );
    }

    #[tokio::test]
    async fn fold_into_cache_persists_split_replacements() {
        let dir = std::env::temp_dir().join(format!(
            "exo-cal-split-fold-{}-{}",
            std::process::id(),
            fresh_event_id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("temp dir");
        let store = Store::open(
            &dir,
            "maya@brightmoor.co",
            gmail::Endpoints::google(),
            gmail::StaticToken("token".into()),
        )
        .await
        .expect("opens store");
        let cal = "maya@brightmoor.co";

        let inst1 = Event {
            id: Some("s1_20261110T150000Z".into()),
            recurring_event_id: Some("s1".into()),
            summary: Some("Standup".into()),
            start: Some(at("2026-11-10T10:00:00-05:00")),
            end: Some(at("2026-11-10T10:30:00-05:00")),
            ..Event::default()
        };
        let inst2 = Event {
            id: Some("s1_20261117T150000Z".into()),
            recurring_event_id: Some("s1".into()),
            summary: Some("Standup".into()),
            start: Some(at("2026-11-17T10:00:00-05:00")),
            end: Some(at("2026-11-17T10:30:00-05:00")),
            ..Event::default()
        };
        let inst3 = Event {
            id: Some("s1_20261124T150000Z".into()),
            recurring_event_id: Some("s1".into()),
            summary: Some("Standup".into()),
            start: Some(at("2026-11-24T10:00:00-05:00")),
            end: Some(at("2026-11-24T10:30:00-05:00")),
            ..Event::default()
        };
        {
            let mut db = store.inner.db.clone();
            let mut write = store.inner.begin(&mut db).await.expect("tx");
            for ev in [&inst1, &inst2, &inst3] {
                put(write.exec(), cal, ev).await.expect("puts");
            }
            write.commit().await.expect("commits");
        }

        let repl2 = Event {
            id: Some("s2_20261117T160000Z".into()),
            recurring_event_id: Some("s2".into()),
            summary: Some("Standup (moved)".into()),
            start: Some(at("2026-11-17T11:00:00-05:00")),
            end: Some(at("2026-11-17T11:30:00-05:00")),
            ..Event::default()
        };
        let repl3 = Event {
            id: Some("s2_20261124T160000Z".into()),
            recurring_event_id: Some("s2".into()),
            summary: Some("Standup (moved)".into()),
            start: Some(at("2026-11-24T11:00:00-05:00")),
            end: Some(at("2026-11-24T11:30:00-05:00")),
            ..Event::default()
        };
        let split = CalendarChange::Split {
            calendar: cal.into(),
            series: "s1".into(),
            from: Cut::At("2026-11-17T15:00:00Z".parse().expect("time")),
            patch: Event {
                id: Some("s2".into()),
                summary: Some("Standup (moved)".into()),
                ..Event::default()
            },
            clear_attendees: false,
            start: at("2026-11-17T11:00:00-05:00"),
            end: at("2026-11-17T11:30:00-05:00"),
            notify: false,
            shown: vec![repl2.clone(), repl3.clone()],
        };
        store.fold_into_cache(&split).await.expect("folds");

        let mut db = store.inner.db.clone();
        let mut cached: Vec<String> = EventRow::filter(EventRow::fields().calendar_id().eq(cal))
            .exec(&mut db)
            .await
            .expect("reads")
            .into_iter()
            .map(|row| row.id)
            .collect();
        cached.sort();
        assert_eq!(
            cached,
            vec![
                "s1_20261110T150000Z".to_owned(),
                "s2_20261117T160000Z".to_owned(),
                "s2_20261124T160000Z".to_owned(),
            ],
            "old instances from cut on are removed and replacement instances are persisted"
        );

        drop(db);
        drop(store);
        let _ = std::fs::remove_dir_all(&dir);
    }
}

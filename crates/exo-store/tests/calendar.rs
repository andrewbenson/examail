//! The calendar cache against mock-servers. It covers the calendar list, a window of events, a
//! window fetched again after the calendar changed, and the changes queued for Calendar, which
//! are shown at once, held, taken back, sent, postponed offline and across a restart, and given
//! up when refused.

mod support;

use exo_store::{CalendarChange, CalendarOutcome, InviteLinks, Refusal, StoredEvent};
use gmail::calendar::{CalendarListEntry, Event, EventDateTime};
use jiff::{SignedDuration, Timestamp, ToSpan, civil::date, tz::TimeZone};
use support::{ACCOUNT, Harness};

fn new_york() -> TimeZone {
    TimeZone::get("America/New_York").expect("zone")
}

#[tokio::test]
async fn calendars_list_without_hidden_ones() {
    let harness = Harness::start().await;
    let calendars = harness.store.sync_calendars(ACCOUNT).await.expect("lists");
    let names: Vec<&str> = calendars
        .iter()
        .filter_map(|calendar| calendar.summary.as_deref())
        .collect();
    assert_eq!(
        names,
        [ACCOUNT, "Brightmoor Team", "Holidays in United States"],
        "Birthdays is hidden"
    );
    let stored: Vec<CalendarListEntry> = harness
        .store
        .calendars()
        .await
        .expect("reads")
        .into_iter()
        .map(|calendar| calendar.entry)
        .collect();
    assert_eq!(stored, calendars, "stored in order");
}

#[tokio::test]
async fn a_window_stores_instances_and_all_day_events_by_date() {
    let harness = Harness::start().await;
    let tz = new_york();
    // The mock's now is Tuesday 2026-11-17.
    let week = date(2026, 11, 15)..date(2026, 11, 22);
    harness
        .store
        .sync_events(ACCOUNT, week.clone(), &tz)
        .await
        .expect("syncs");
    let team = "c_8f3b2a1d9e7c4b6a5f01@group.calendar.google.com";
    harness
        .store
        .sync_events(team, week.clone(), &tz)
        .await
        .expect("syncs");

    let tuesday = harness
        .store
        .events(
            &[ACCOUNT.to_owned()],
            date(2026, 11, 17)..date(2026, 11, 18),
            &tz,
        )
        .await
        .expect("reads");
    let titles: Vec<&str> = tuesday
        .iter()
        .filter_map(|stored| stored.event.summary.as_deref())
        .collect();
    assert_eq!(
        titles,
        ["Priya / Maya 1:1", "Design crit"],
        "recurring events come as instances"
    );
    assert!(
        tuesday
            .iter()
            .all(|stored| stored.event.recurring_event_id.is_some())
    );

    let friday = harness
        .store
        .events(
            &[team.to_owned()],
            date(2026, 11, 20)..date(2026, 11, 21),
            &tz,
        )
        .await
        .expect("reads");
    let titles: Vec<&str> = friday
        .iter()
        .filter_map(|stored| stored.event.summary.as_deref())
        .collect();
    assert_eq!(titles, ["Company all-hands"]);
    assert!(
        harness
            .store
            .events(
                &[team.to_owned()],
                date(2026, 11, 19)..date(2026, 11, 20),
                &tz
            )
            .await
            .expect("reads")
            .is_empty(),
        "only the calendars asked for"
    );
}

#[tokio::test]
async fn fetching_a_window_again_replaces_it() {
    let harness = Harness::start().await;
    let tz = new_york();
    let day = date(2026, 11, 19)..date(2026, 11, 20);
    harness
        .store
        .sync_events(ACCOUNT, day.clone(), &tz)
        .await
        .expect("syncs");
    let before = harness
        .store
        .events(&[ACCOUNT.to_owned()], day.clone(), &tz)
        .await
        .expect("reads");
    let dinner = before
        .iter()
        .find(|stored| stored.event.summary.as_deref() == Some("Dinner with Jess 🍕"))
        .expect("listed")
        .event
        .clone();

    // Elsewhere, dinner is cancelled and a lunch is added.
    let calendar = harness.gmail.calendar();
    calendar
        .delete_event(ACCOUNT, dinner.id.as_deref().expect("id"), None)
        .await
        .expect("deletes");
    let at = |hour: i8| EventDateTime {
        date_time: Some(
            date(2026, 11, 19)
                .at(hour, 0, 0, 0)
                .to_zoned(tz.clone())
                .expect("exists")
                .strftime("%Y-%m-%dT%H:%M:%S%:z")
                .to_string(),
        ),
        ..EventDateTime::default()
    };
    let lunch = Event {
        summary: Some("Lunch".into()),
        start: Some(at(12)),
        end: Some(at(13)),
        ..Event::default()
    };
    calendar
        .insert_event(ACCOUNT, &lunch, None)
        .await
        .expect("inserts");

    harness
        .store
        .sync_events(ACCOUNT, day.clone(), &tz)
        .await
        .expect("syncs");
    let after = harness
        .store
        .events(&[ACCOUNT.to_owned()], day, &tz)
        .await
        .expect("reads");
    let titles: Vec<&str> = after
        .iter()
        .filter_map(|stored| stored.event.summary.as_deref())
        .collect();
    assert_eq!(titles, ["Lunch", "Calloway: Atlas feedback call"]);
    let next_week = date(2026, 11, 19) + 7.days();
    assert!(
        harness
            .store
            .events(&[ACCOUNT.to_owned()], next_week..next_week + 1.day(), &tz)
            .await
            .expect("reads")
            .is_empty(),
        "nothing outside the windows fetched"
    );
}

/// Friday the 20th at `hour`, in New York.
fn friday_at(hour: i8) -> EventDateTime {
    let at = date(2026, 11, 20)
        .at(hour, 0, 0, 0)
        .to_zoned(new_york())
        .expect("exists");
    EventDateTime {
        date_time: Some(at.strftime("%Y-%m-%dT%H:%M:%S%:z").to_string()),
        ..EventDateTime::default()
    }
}

async fn friday(harness: &Harness) -> Vec<StoredEvent> {
    harness
        .store
        .events(
            &[ACCOUNT.to_owned()],
            date(2026, 11, 20)..date(2026, 11, 21),
            &new_york(),
        )
        .await
        .expect("reads")
}

fn titles(events: &[StoredEvent]) -> Vec<String> {
    events
        .iter()
        .filter_map(|stored| stored.event.summary.clone())
        .collect()
}

/// Sends every queued change whose turn it is, as the calendar's task does, and says how each
/// went.
async fn flush(harness: &Harness) -> Vec<CalendarOutcome> {
    let mut outcomes = Vec::new();
    loop {
        let (due, _) = harness
            .store
            .due_calendar_changes(false)
            .await
            .expect("reads");
        if due.is_empty() {
            return outcomes;
        }
        let mut sent = false;
        for queued in due {
            let outcome = harness
                .store
                .send_calendar_change(&queued)
                .await
                .expect("sends");
            sent |= outcome != CalendarOutcome::Postponed;
            outcomes.push(outcome);
        }
        if !sent {
            return outcomes;
        }
    }
}

/// Friday's events of mine titled `title`, as Calendar has them.
async fn friday_titled(harness: &Harness, title: &str) -> usize {
    let query = gmail::calendar::EventsQuery {
        q: Some(title.to_owned()),
        single_events: true,
        ..gmail::calendar::EventsQuery::default()
    };
    let page = harness
        .gmail
        .calendar()
        .events(ACCOUNT, &query)
        .await
        .expect("events.list");
    page.items
        .iter()
        .filter(|event| {
            event.summary.as_deref() == Some(title) && event.status.as_deref() != Some("cancelled")
        })
        .count()
}

fn mine(event: &Event) -> Option<String> {
    event
        .attendees
        .iter()
        .find(|attendee| attendee.email.as_deref() == Some(ACCOUNT))
        .and_then(|attendee| attendee.response_status.clone())
}

#[tokio::test]
async fn an_answer_shows_at_once_and_calendar_gets_it() {
    let harness = Harness::start().await;
    let tz = new_york();
    let thursday = date(2026, 11, 19)..date(2026, 11, 20);
    harness
        .store
        .sync_events(ACCOUNT, thursday.clone(), &tz)
        .await
        .expect("syncs");
    let read = async || {
        harness
            .store
            .events(&[ACCOUNT.to_owned()], thursday.clone(), &tz)
            .await
            .expect("reads")
            .into_iter()
            .find(|stored| stored.event.summary.as_deref() == Some("Calloway: Atlas feedback call"))
            .expect("listed")
            .event
    };
    let call = read().await;
    assert_eq!(mine(&call).as_deref(), Some("needsAction"));

    let change = CalendarChange::Respond {
        calendar: ACCOUNT.into(),
        event: call.clone(),
        status: "accepted".into(),
        all: false,
    };
    harness
        .store
        .queue_calendar_change(1, change, None)
        .await
        .expect("queues");
    assert_eq!(
        mine(&read().await).as_deref(),
        Some("accepted"),
        "shown before Calendar has it"
    );
    assert_eq!(
        harness.store.unsent().await.expect("counts").changes,
        1,
        "and counted as unsent"
    );
    assert_eq!(flush(&harness).await, [CalendarOutcome::Sent]);
    let answered = harness
        .gmail
        .calendar()
        .event(ACCOUNT, call.id.as_deref().expect("id"))
        .await
        .expect("events.get");
    assert_eq!(mine(&answered).as_deref(), Some("accepted"));
    assert_eq!(
        answered.attendees.len(),
        call.attendees.len(),
        "the others are kept"
    );
    assert_eq!(
        mine(&read().await).as_deref(),
        Some("accepted"),
        "the cache has Calendar's answer"
    );
    assert_eq!(harness.store.unsent().await.expect("counts").changes, 0);
}

#[tokio::test]
async fn events_are_created_changed_and_deleted_through_the_queue() {
    let harness = Harness::start().await;
    let tz = new_york();

    // A client-chosen id is kept, as Calendar keeps it.
    let lunch = Event {
        id: Some("e0lunch0001".into()),
        summary: Some("Lunch".into()),
        start: Some(friday_at(12)),
        end: Some(friday_at(13)),
        ..Event::default()
    };
    harness
        .store
        .queue_calendar_change(
            1,
            CalendarChange::Create {
                calendar: ACCOUNT.into(),
                event: lunch.clone(),
            },
            None,
        )
        .await
        .expect("queues");
    let shown = friday(&harness).await;
    assert_eq!(
        (titles(&shown), shown[0].unsynced),
        (vec!["Lunch".to_owned()], true),
        "shown, though Calendar hasn't got it"
    );
    assert_eq!(flush(&harness).await, [CalendarOutcome::Sent]);
    let shown = friday(&harness).await;
    assert_eq!(
        (titles(&shown), shown[0].unsynced),
        (vec!["Lunch".to_owned()], false)
    );
    assert_eq!(friday_titled(&harness, "Lunch").await, 1);

    let patch = Event {
        summary: Some("Long lunch".into()),
        end: Some(friday_at(14)),
        ..Event::default()
    };
    let long = Event {
        summary: patch.summary.clone(),
        end: patch.end.clone(),
        ..lunch.clone()
    };
    let edit = CalendarChange::Edit {
        calendar: ACCOUNT.into(),
        id: "e0lunch0001".into(),
        patch,
        clear_attendees: false,
        notify: false,
        shift: None,
        shown: vec![long],
    };
    harness
        .store
        .queue_calendar_change(2, edit, None)
        .await
        .expect("queues");
    assert_eq!(titles(&friday(&harness).await), ["Long lunch"]);
    assert_eq!(flush(&harness).await, [CalendarOutcome::Sent]);
    let changed = harness
        .gmail
        .calendar()
        .event(ACCOUNT, "e0lunch0001")
        .await
        .expect("events.get");
    assert_eq!(
        (changed.summary.as_deref(), changed.end.as_ref()),
        (Some("Long lunch"), Some(&friday_at(14)))
    );
    assert_eq!(titles(&friday(&harness).await), ["Long lunch"]);

    harness
        .store
        .queue_calendar_change(
            3,
            CalendarChange::Delete {
                calendar: ACCOUNT.into(),
                id: "e0lunch0001".into(),
                notify: false,
            },
            None,
        )
        .await
        .expect("queues");
    assert!(friday(&harness).await.is_empty());
    assert_eq!(flush(&harness).await, [CalendarOutcome::Sent]);
    assert_eq!(friday_titled(&harness, "Long lunch").await, 0);
    assert!(friday(&harness).await.is_empty());

    // Deleting a series drops its cached instances.
    let tuesday = date(2026, 11, 17)..date(2026, 11, 18);
    harness
        .store
        .sync_events(ACCOUNT, tuesday.clone(), &tz)
        .await
        .expect("syncs");
    let series = harness
        .store
        .events(&[ACCOUNT.to_owned()], tuesday.clone(), &tz)
        .await
        .expect("reads")[0]
        .event
        .recurring_event_id
        .clone()
        .expect("an instance");
    harness
        .store
        .queue_calendar_change(
            4,
            CalendarChange::Delete {
                calendar: ACCOUNT.into(),
                id: series,
                notify: false,
            },
            None,
        )
        .await
        .expect("queues");
    assert_eq!(flush(&harness).await, [CalendarOutcome::Sent]);
    assert_eq!(
        titles(
            &harness
                .store
                .events(&[ACCOUNT.to_owned()], tuesday, &tz)
                .await
                .expect("reads")
        )
        .len(),
        1,
        "the other series stays"
    );
}

#[tokio::test]
async fn a_held_change_waits_for_its_hold_and_undo_takes_it_back() {
    let harness = Harness::start().await;
    let hold = Timestamp::now() + SignedDuration::from_secs(10);
    let review = Event {
        id: Some("e0event0001".into()),
        summary: Some("Review".into()),
        start: Some(friday_at(9)),
        end: Some(friday_at(10)),
        ..Event::default()
    };
    harness
        .store
        .queue_calendar_change(
            7,
            CalendarChange::Create {
                calendar: ACCOUNT.into(),
                event: review,
            },
            Some(hold),
        )
        .await
        .expect("queues");
    let (due, next) = harness
        .store
        .due_calendar_changes(false)
        .await
        .expect("reads");
    assert_eq!((due.len(), next), (0, Some(hold)), "held until then");
    let (due, _) = harness
        .store
        .due_calendar_changes(true)
        .await
        .expect("reads");
    assert_eq!(
        due.iter().map(|queued| queued.token).collect::<Vec<_>>(),
        [7],
        "unless everything goes now, as quitting sends it"
    );
    assert_eq!(titles(&friday(&harness).await), ["Review"]);

    assert!(
        harness
            .store
            .unqueue_calendar_change(7)
            .await
            .expect("takes back")
    );
    assert!(
        !harness
            .store
            .unqueue_calendar_change(7)
            .await
            .expect("takes back"),
        "once"
    );
    assert!(friday(&harness).await.is_empty(), "and it's gone");
    assert_eq!(
        friday_titled(&harness, "Review").await,
        0,
        "Calendar never heard of it"
    );
}

#[tokio::test]
async fn undoing_a_new_event_takes_back_its_edits_too() {
    let harness = Harness::start().await;
    let review = Event {
        id: Some("e0event0003".into()),
        summary: Some("Review".into()),
        start: Some(friday_at(9)),
        end: Some(friday_at(10)),
        ..Event::default()
    };
    let hold = Some(Timestamp::now() + SignedDuration::from_secs(10));
    harness
        .store
        .queue_calendar_change(
            1,
            CalendarChange::Create {
                calendar: ACCOUNT.into(),
                event: review.clone(),
            },
            hold,
        )
        .await
        .expect("queues");
    let renamed = Event {
        summary: Some("Review, renamed".into()),
        ..review
    };
    let edit = CalendarChange::Edit {
        calendar: ACCOUNT.into(),
        id: "e0event0003".into(),
        patch: Event {
            summary: renamed.summary.clone(),
            ..Event::default()
        },
        clear_attendees: false,
        notify: false,
        shift: None,
        shown: vec![renamed],
    };
    harness
        .store
        .queue_calendar_change(2, edit, hold)
        .await
        .expect("queues");
    assert_eq!(
        titles(&friday(&harness).await),
        ["Review, renamed"],
        "the edit shows on the new event"
    );
    assert!(
        harness
            .store
            .unqueue_calendar_change(1)
            .await
            .expect("takes back")
    );
    assert_eq!(
        harness.store.calendar_pending().await.expect("counts"),
        0,
        "its edit goes with it"
    );
    assert!(friday(&harness).await.is_empty());
}

#[tokio::test]
async fn deleting_a_new_event_still_queued_takes_it_back() {
    let harness = Harness::start().await;
    let review = Event {
        id: Some("e0event0002".into()),
        summary: Some("Review".into()),
        start: Some(friday_at(9)),
        end: Some(friday_at(10)),
        ..Event::default()
    };
    let hold = Some(Timestamp::now() + SignedDuration::from_secs(10));
    harness
        .store
        .queue_calendar_change(
            1,
            CalendarChange::Create {
                calendar: ACCOUNT.into(),
                event: review.clone(),
            },
            hold,
        )
        .await
        .expect("queues");
    let answer = CalendarChange::Respond {
        calendar: ACCOUNT.into(),
        event: review,
        status: "tentative".into(),
        all: false,
    };
    harness
        .store
        .queue_calendar_change(2, answer, None)
        .await
        .expect("queues");
    assert_eq!(harness.store.calendar_pending().await.expect("counts"), 2);
    harness
        .store
        .queue_calendar_change(
            3,
            CalendarChange::Delete {
                calendar: ACCOUNT.into(),
                id: "e0event0002".into(),
                notify: true,
            },
            None,
        )
        .await
        .expect("queues");
    assert_eq!(
        harness.store.calendar_pending().await.expect("counts"),
        0,
        "it, and what came after it, are taken back"
    );
    assert!(friday(&harness).await.is_empty());
    let (due, _) = harness
        .store
        .due_calendar_changes(true)
        .await
        .expect("reads");
    assert!(due.is_empty());
    assert_eq!(
        friday_titled(&harness, "Review").await,
        0,
        "Calendar never heard of it"
    );
}

#[tokio::test]
async fn a_change_made_offline_waits_through_a_restart_and_goes_once_calendar_answers() {
    let mut harness = Harness::start().await;
    let review = Event {
        id: Some("e0event0003".into()),
        summary: Some("Review".into()),
        start: Some(friday_at(9)),
        end: Some(friday_at(10)),
        ..Event::default()
    };
    harness.server.disconnect().await;
    harness
        .store
        .queue_calendar_change(
            1,
            CalendarChange::Create {
                calendar: ACCOUNT.into(),
                event: review,
            },
            None,
        )
        .await
        .expect("queues");
    assert_eq!(flush(&harness).await, [CalendarOutcome::Postponed]);
    let (due, next) = harness
        .store
        .due_calendar_changes(false)
        .await
        .expect("reads");
    assert!(due.is_empty() && next.is_some(), "it backs off");
    assert_eq!(titles(&friday(&harness).await), ["Review"], "still shown");

    harness.reopen().await;
    assert_eq!(
        titles(&friday(&harness).await),
        ["Review"],
        "and still there after a restart"
    );
    harness
        .server
        .reconnect()
        .await
        .expect("the mock comes back");
    let (due, _) = harness
        .store
        .due_calendar_changes(true)
        .await
        .expect("reads");
    assert_eq!(
        harness
            .store
            .send_calendar_change(&due[0])
            .await
            .expect("sends"),
        CalendarOutcome::Sent
    );
    assert_eq!(friday_titled(&harness, "Review").await, 1);
}

#[tokio::test]
async fn a_refused_change_is_given_up_and_the_calendar_shows_calendars_copy() {
    let harness = Harness::start().await;
    let review = Event {
        id: Some("e0event0004".into()),
        summary: Some("Review".into()),
        start: Some(friday_at(9)),
        end: Some(friday_at(10)),
        ..Event::default()
    };
    harness.fault(serde_json::json!({"method": "POST", "path": "/calendar/v3/calendars/", "status": 400, "message": "Invalid attendee email.", "reason": "invalid"})).await;
    harness
        .store
        .queue_calendar_change(
            1,
            CalendarChange::Create {
                calendar: ACCOUNT.into(),
                event: review,
            },
            None,
        )
        .await
        .expect("queues");
    let outcomes = flush(&harness).await;
    assert!(
        matches!(
            &outcomes[..],
            [CalendarOutcome::Refused {
                refusal: Refusal::Other,
                ..
            }]
        ),
        "{outcomes:?}"
    );
    assert!(friday(&harness).await.is_empty(), "the new event is gone");
    assert_eq!(harness.store.calendar_pending().await.expect("counts"), 0);
}

#[tokio::test]
async fn an_event_whose_creation_answer_was_lost_is_made_once_and_not_a_failure() {
    let harness = Harness::start().await;
    let review = Event {
        id: Some("e0budget0001".into()),
        summary: Some("Budget review".into()),
        start: Some(friday_at(15)),
        end: Some(friday_at(16)),
        ..Event::default()
    };
    // Calendar makes it, but the answer is lost, so the retry finds its id taken.
    harness.fault(serde_json::json!({"method": "POST", "path": "/calendar/v3/calendars/", "status": 503, "after": true})).await;
    harness
        .store
        .queue_calendar_change(
            1,
            CalendarChange::Create {
                calendar: ACCOUNT.into(),
                event: review,
            },
            None,
        )
        .await
        .expect("queues");
    assert_eq!(
        flush(&harness).await,
        [CalendarOutcome::Sent],
        "it was made, so it didn't fail (Try Again would make it twice)"
    );
    assert_eq!(friday_titled(&harness, "Budget review").await, 1);
}

#[tokio::test]
async fn an_event_whose_deletion_answer_was_lost_is_not_a_failure() {
    let harness = Harness::start().await;
    let review = Event {
        id: Some("e0budget0002".into()),
        summary: Some("Budget review".into()),
        start: Some(friday_at(15)),
        end: Some(friday_at(16)),
        ..Event::default()
    };
    harness
        .store
        .queue_calendar_change(
            1,
            CalendarChange::Create {
                calendar: ACCOUNT.into(),
                event: review,
            },
            None,
        )
        .await
        .expect("queues");
    assert_eq!(flush(&harness).await, [CalendarOutcome::Sent]);
    harness.fault(serde_json::json!({"method": "DELETE", "path": "/calendar/v3/calendars/", "status": 503, "after": true})).await;
    harness
        .store
        .queue_calendar_change(
            2,
            CalendarChange::Delete {
                calendar: ACCOUNT.into(),
                id: "e0budget0002".into(),
                notify: false,
            },
            None,
        )
        .await
        .expect("queues");
    assert_eq!(
        flush(&harness).await,
        [CalendarOutcome::Sent],
        "it's gone, so deleting it didn't fail"
    );
    assert_eq!(friday_titled(&harness, "Budget review").await, 0);
    assert!(
        friday(&harness)
            .await
            .iter()
            .all(|stored| stored.event.id.as_deref() != Some("e0budget0002"))
    );
}

#[tokio::test]
async fn an_answer_to_a_cancelled_invite_is_refused_as_gone() {
    let harness = Harness::start().await;
    let review = Event {
        id: Some("e0event0005".into()),
        ical_uid: Some("e0event0005@google.com".into()),
        summary: Some("Review".into()),
        start: Some(friday_at(9)),
        end: Some(friday_at(10)),
        ..Event::default()
    };
    let made = harness
        .gmail
        .calendar()
        .insert_event(ACCOUNT, &review, None)
        .await
        .expect("inserts");
    harness
        .gmail
        .calendar()
        .delete_event(ACCOUNT, made.id.as_deref().expect("id"), None)
        .await
        .expect("cancels");
    let invite = Event {
        ical_uid: made.ical_uid.clone(),
        ..review
    };
    let answer = CalendarChange::RespondToInvite {
        calendar: ACCOUNT.into(),
        invite,
        status: "accepted".into(),
        thread: Some("t1".into()),
        links: None,
    };
    harness
        .store
        .queue_calendar_change(1, answer, None)
        .await
        .expect("queues");
    let outcomes = flush(&harness).await;
    assert!(
        matches!(
            &outcomes[..],
            [CalendarOutcome::Refused {
                refusal: Refusal::Gone,
                ..
            }]
        ),
        "not imported again: {outcomes:?}"
    );
    assert_eq!(friday_titled(&harness, "Review").await, 0);
}

#[tokio::test]
async fn an_unknown_senders_invite_is_not_imported() {
    let harness = Harness::start().await;
    let invite = Event {
        ical_uid: Some("pitch-1120@juniperpartners.vc".into()),
        summary: Some("Pitch".into()),
        start: Some(friday_at(11)),
        end: Some(friday_at(12)),
        ..Event::default()
    };
    let links = InviteLinks {
        mark_sender_known: "https://calendar.google.com/calendar/marksenderasknown?eid=x".into(),
        accepted: Some("https://calendar.google.com/calendar/event?rst=1".into()),
        ..InviteLinks::default()
    };
    let answer = CalendarChange::RespondToInvite {
        calendar: ACCOUNT.into(),
        invite,
        status: "accepted".into(),
        thread: Some("t1".into()),
        links: Some(links),
    };
    harness
        .store
        .queue_calendar_change(1, answer, None)
        .await
        .expect("queues");
    let outcomes = flush(&harness).await;
    assert!(
        matches!(
            &outcomes[..],
            [CalendarOutcome::Refused {
                refusal: Refusal::UnknownSender,
                ..
            }]
        ),
        "{outcomes:?}"
    );
    assert_eq!(friday_titled(&harness, "Pitch").await, 0, "not imported");
}

#[tokio::test]
async fn an_answer_only_the_organizer_may_give_is_refused_as_such() {
    let harness = Harness::start().await;
    let review = Event {
        id: Some("e0event0006".into()),
        ical_uid: Some("e0event0006@google.com".into()),
        summary: Some("Review".into()),
        start: Some(friday_at(9)),
        end: Some(friday_at(10)),
        ..Event::default()
    };
    harness
        .gmail
        .calendar()
        .insert_event(ACCOUNT, &review, None)
        .await
        .expect("inserts");
    harness.fault(serde_json::json!({"method": "PATCH", "path": "/calendar/v3/calendars/", "status": 403, "message": "Forbidden", "reason": "forbiddenForNonOrganizer"})).await;
    let answer = CalendarChange::RespondToInvite {
        calendar: ACCOUNT.into(),
        invite: review,
        status: "accepted".into(),
        thread: Some("t1".into()),
        links: None,
    };
    harness
        .store
        .queue_calendar_change(1, answer, None)
        .await
        .expect("queues");
    let outcomes = flush(&harness).await;
    assert!(
        matches!(
            &outcomes[..],
            [CalendarOutcome::Refused {
                refusal: Refusal::NonOrganizer,
                ..
            }]
        ),
        "{outcomes:?}"
    );
}

/// Another Google account of the user's, signed in for its calendars (mock-servers gives it a
/// primary calendar of its own and the holidays calendar).
const PERSONAL: &str = "maya.lin@example.com";

fn personal_tokens() -> gmail::StaticToken {
    gmail::StaticToken(format!("{}:{PERSONAL}", mock_servers::DEV_ACCESS_TOKEN))
}

#[tokio::test]
async fn a_guests_calendar_is_read_through_whichever_account_can() {
    let harness = Harness::start().await;
    let (from, to) = (
        "2026-11-15T05:00:00Z".parse().expect("time"),
        "2026-11-22T05:00:00Z".parse().expect("time"),
    );
    // Someone at the calendar account's workspace, whose calendar only that account sees.
    let neighbour = "bea@example.com";
    assert_eq!(
        harness
            .store
            .attendee_events(neighbour, from, to)
            .await
            .expect("reads"),
        None,
        "the mail account can't list it, nor see its free/busy"
    );
    harness
        .store
        .add_calendar_account(PERSONAL, personal_tokens());
    assert_eq!(
        harness
            .store
            .attendee_events(neighbour, from, to)
            .await
            .expect("reads"),
        Some(Vec::new()),
        "listed through the calendar account"
    );
    // A colleague of the mail account's still comes through the mail account.
    let colleague = harness
        .store
        .attendee_events("priya@brightmoor.co", from, to)
        .await
        .expect("reads")
        .expect("listed");
    assert!(!colleague.is_empty());
}

#[tokio::test]
async fn a_calendar_account_lists_beside_the_mail_account_and_its_calendars_go_through_it() {
    let harness = Harness::start().await;
    let tz = new_york();
    harness.store.sync_calendars(ACCOUNT).await.expect("lists");
    harness
        .store
        .add_calendar_account(PERSONAL, personal_tokens());
    let theirs = harness.store.sync_calendars(PERSONAL).await.expect("lists");
    assert_eq!(
        theirs
            .iter()
            .filter_map(|calendar| calendar.id.as_deref())
            .collect::<Vec<_>>(),
        [PERSONAL, "en.usa#holiday@group.v.calendar.google.com"]
    );
    let listed = |calendars: Vec<exo_store::AccountCalendar>| {
        calendars
            .into_iter()
            .map(|calendar| (calendar.account, calendar.entry.id.unwrap_or_default()))
            .collect::<Vec<_>>()
    };
    let holidays = "en.usa#holiday@group.v.calendar.google.com";
    let team = "c_8f3b2a1d9e7c4b6a5f01@group.calendar.google.com";
    assert_eq!(
        listed(harness.store.calendars().await.expect("reads")),
        [
            (ACCOUNT.to_owned(), ACCOUNT.to_owned()),
            (ACCOUNT.to_owned(), team.to_owned()),
            (ACCOUNT.to_owned(), holidays.to_owned()),
            (PERSONAL.to_owned(), PERSONAL.to_owned())
        ],
        "the holidays both list show once, from the mail account"
    );

    // The events on the calendar account's primary calendar come through its own grant.
    let wednesday = date(2026, 11, 18)..date(2026, 11, 19);
    harness
        .store
        .sync_events(PERSONAL, wednesday.clone(), &tz)
        .await
        .expect("syncs");
    let events = harness
        .store
        .events(&[PERSONAL.to_owned()], wednesday, &tz)
        .await
        .expect("reads");
    assert_eq!(titles(&events), ["Dentist"]);

    // A new event on the calendar account goes to its calendar, not the mail account's.
    let lunch = Event {
        id: Some("e0lunch0personal1".into()),
        summary: Some("Lunch".into()),
        start: Some(friday_at(12)),
        end: Some(friday_at(13)),
        ..Event::default()
    };
    harness
        .store
        .queue_calendar_change(
            1,
            CalendarChange::Create {
                calendar: PERSONAL.into(),
                event: lunch,
            },
            None,
        )
        .await
        .expect("queues");
    assert_eq!(flush(&harness).await, [CalendarOutcome::Sent]);
    let theirs = gmail::Client::new(harness.server.endpoints(), personal_tokens());
    let query = gmail::calendar::EventsQuery {
        q: Some("Lunch".into()),
        ..gmail::calendar::EventsQuery::default()
    };
    assert_eq!(
        theirs
            .calendar()
            .events(PERSONAL, &query)
            .await
            .expect("lists")
            .items
            .len(),
        1
    );
    assert_eq!(
        friday_titled(&harness, "Lunch").await,
        0,
        "the mail account's calendar never heard of it"
    );

    // Once the calendar account signs out, its calendars leave the list, and a change to one of
    // them waits for the account.
    harness
        .store
        .remove_calendar_account(PERSONAL, false)
        .await
        .expect("signs out");
    assert_eq!(
        listed(harness.store.calendars().await.expect("reads")).len(),
        3
    );
    let yoga = Event {
        id: Some("e0dance0personal2".into()),
        summary: Some("Yoga".into()),
        start: Some(friday_at(18)),
        end: Some(friday_at(19)),
        ..Event::default()
    };
    harness
        .store
        .queue_calendar_change(
            2,
            CalendarChange::Create {
                calendar: PERSONAL.into(),
                event: yoga,
            },
            None,
        )
        .await
        .expect("queues");
    let send_all = async || {
        let (due, _) = harness
            .store
            .due_calendar_changes(true)
            .await
            .expect("reads");
        let mut outcomes = Vec::new();
        for queued in due {
            outcomes.push(
                harness
                    .store
                    .send_calendar_change(&queued)
                    .await
                    .expect("sends"),
            );
        }
        outcomes
    };
    assert_eq!(
        send_all().await,
        [CalendarOutcome::Postponed],
        "not refused through the mail account"
    );
    harness
        .store
        .add_calendar_account(PERSONAL, personal_tokens());
    assert_eq!(
        listed(harness.store.calendars().await.expect("reads")).len(),
        4,
        "its list was kept"
    );
    assert_eq!(
        send_all().await,
        [CalendarOutcome::Sent],
        "it goes once the account is back"
    );
    harness
        .store
        .remove_calendar_account(PERSONAL, true)
        .await
        .expect("removes");
    harness
        .store
        .add_calendar_account(PERSONAL, personal_tokens());
    assert_eq!(
        listed(harness.store.calendars().await.expect("reads")).len(),
        3,
        "its list went"
    );
    let friday = harness
        .store
        .events(
            &[PERSONAL.to_owned()],
            date(2026, 11, 20)..date(2026, 11, 21),
            &tz,
        )
        .await
        .expect("reads");
    assert!(friday.is_empty(), "and its calendar's events");
}

#[tokio::test]
async fn clearing_all_guests_sends_an_empty_attendee_list_and_drops_them_on_split() {
    let harness = Harness::start().await;
    let guest = gmail::calendar::EventAttendee {
        email: Some("priya@brightmoor.co".into()),
        response_status: Some("accepted".into()),
        ..gmail::calendar::EventAttendee::default()
    };
    let review = Event {
        id: Some("e0guestclear1".into()),
        summary: Some("Review".into()),
        start: Some(friday_at(9)),
        end: Some(friday_at(10)),
        attendees: vec![guest.clone()],
        ..Event::default()
    };
    harness
        .store
        .queue_calendar_change(
            1,
            CalendarChange::Create {
                calendar: ACCOUNT.into(),
                event: review.clone(),
            },
            None,
        )
        .await
        .expect("queues");
    assert_eq!(flush(&harness).await, [CalendarOutcome::Sent]);
    assert_eq!(
        harness
            .gmail
            .calendar()
            .event(ACCOUNT, "e0guestclear1")
            .await
            .expect("events.get")
            .attendees
            .len(),
        1
    );

    let cleared = Event {
        attendees: Vec::new(),
        ..review
    };
    harness
        .store
        .queue_calendar_change(
            2,
            CalendarChange::Edit {
                calendar: ACCOUNT.into(),
                id: "e0guestclear1".into(),
                patch: Event::default(),
                clear_attendees: true,
                notify: false,
                shift: None,
                shown: vec![cleared],
            },
            None,
        )
        .await
        .expect("queues");
    assert_eq!(flush(&harness).await, [CalendarOutcome::Sent]);
    assert!(
        harness
            .gmail
            .calendar()
            .event(ACCOUNT, "e0guestclear1")
            .await
            .expect("events.get")
            .attendees
            .is_empty(),
        "empty attendees array clears guests on Google Calendar"
    );

    // Removing all guests on a series split leaves the remainder without attendees.
    let series = Event {
        id: Some("e0guestseries1".into()),
        summary: Some("Weekly sync".into()),
        start: Some(EventDateTime {
            date_time: Some("2026-11-10T10:00:00-05:00".into()),
            time_zone: Some("America/New_York".into()),
            ..EventDateTime::default()
        }),
        end: Some(EventDateTime {
            date_time: Some("2026-11-10T10:30:00-05:00".into()),
            time_zone: Some("America/New_York".into()),
            ..EventDateTime::default()
        }),
        recurrence: vec!["RRULE:FREQ=WEEKLY;COUNT=4;BYDAY=TU".into()],
        attendees: vec![guest],
        ..Event::default()
    };
    harness
        .gmail
        .calendar()
        .insert_event(ACCOUNT, &series, None)
        .await
        .expect("inserts series");
    let cut_start = EventDateTime {
        date_time: Some("2026-11-17T10:00:00-05:00".into()),
        time_zone: Some("America/New_York".into()),
        ..EventDateTime::default()
    };
    let cut_end = EventDateTime {
        date_time: Some("2026-11-17T10:30:00-05:00".into()),
        time_zone: Some("America/New_York".into()),
        ..EventDateTime::default()
    };
    harness
        .store
        .queue_calendar_change(
            3,
            CalendarChange::Split {
                calendar: ACCOUNT.into(),
                series: "e0guestseries1".into(),
                from: exo_store::Cut::At("2026-11-17T15:00:00Z".parse().expect("time")),
                patch: Event {
                    id: Some("e0guestseries2".into()),
                    ..Event::default()
                },
                clear_attendees: true,
                start: cut_start,
                end: cut_end,
                notify: false,
                shown: Vec::new(),
            },
            None,
        )
        .await
        .expect("queues split");
    assert_eq!(flush(&harness).await, [CalendarOutcome::Sent]);
    assert!(
        harness
            .gmail
            .calendar()
            .event(ACCOUNT, "e0guestseries2")
            .await
            .expect("events.get")
            .attendees
            .is_empty(),
        "split remainder drops cleared attendees"
    );
}

#[tokio::test]
async fn retrying_a_split_after_partial_failure_keeps_the_original_recurrence() {
    let harness = Harness::start().await;
    let series = Event {
        id: Some("e0splitmaster1".into()),
        summary: Some("Standup".into()),
        start: Some(EventDateTime {
            date_time: Some("2026-11-10T10:00:00-05:00".into()),
            time_zone: Some("America/New_York".into()),
            ..EventDateTime::default()
        }),
        end: Some(EventDateTime {
            date_time: Some("2026-11-10T10:30:00-05:00".into()),
            time_zone: Some("America/New_York".into()),
            ..EventDateTime::default()
        }),
        recurrence: vec!["RRULE:FREQ=WEEKLY;COUNT=6;BYDAY=TU".into()],
        ..Event::default()
    };
    harness
        .gmail
        .calendar()
        .insert_event(ACCOUNT, &series, None)
        .await
        .expect("inserts series");

    // Simulate a partial failure where `PATCH` on the master commits on the server (`after: true`)
    // and then fails all 3 HTTP attempts so the outbox postpones and retries later.
    for _ in 0..3 {
        harness
            .fault(serde_json::json!({
                "method": "PATCH",
                "path": "/calendar/v3/calendars/",
                "status": 503,
                "after": true
            }))
            .await;
    }

    harness
        .store
        .queue_calendar_change(
            1,
            CalendarChange::Split {
                calendar: ACCOUNT.into(),
                series: "e0splitmaster1".into(),
                from: exo_store::Cut::At("2026-11-17T15:00:00Z".parse().expect("time")),
                patch: Event {
                    id: Some("e0splitrem001".into()),
                    summary: Some("Standup (moved)".into()),
                    ..Event::default()
                },
                clear_attendees: false,
                start: EventDateTime {
                    date_time: Some("2026-11-17T11:00:00-05:00".into()),
                    time_zone: Some("America/New_York".into()),
                    ..EventDateTime::default()
                },
                end: EventDateTime {
                    date_time: Some("2026-11-17T11:30:00-05:00".into()),
                    time_zone: Some("America/New_York".into()),
                    ..EventDateTime::default()
                },
                notify: false,
                shown: Vec::new(),
            },
            None,
        )
        .await
        .expect("queues split");

    let (due, _) = harness
        .store
        .due_calendar_changes(true)
        .await
        .expect("reads");
    assert_eq!(
        harness
            .store
            .send_calendar_change(&due[0])
            .await
            .expect("first attempt"),
        CalendarOutcome::Postponed
    );

    // Now retry: master already has `UNTIL=20261117T145959Z` on the server, and remainder must NOT
    // inherit that truncated `UNTIL`.
    let (due, _) = harness
        .store
        .due_calendar_changes(true)
        .await
        .expect("reads");
    assert_eq!(
        harness
            .store
            .send_calendar_change(&due[0])
            .await
            .expect("retry"),
        CalendarOutcome::Sent
    );

    let master_after = harness
        .gmail
        .calendar()
        .event(ACCOUNT, "e0splitmaster1")
        .await
        .expect("master");
    let rem_after = harness
        .gmail
        .calendar()
        .event(ACCOUNT, "e0splitrem001")
        .await
        .expect("remainder");
    assert_eq!(
        master_after.recurrence,
        ["RRULE:FREQ=WEEKLY;BYDAY=TU;UNTIL=20261117T145959Z"]
    );
    assert_eq!(
        rem_after.recurrence,
        ["RRULE:FREQ=WEEKLY;BYDAY=TU;WKST=SU;COUNT=5"],
        "remainder keeps the 5 remaining instances rather than the truncated UNTIL"
    );
}

#[tokio::test]
async fn deleting_or_undoing_an_in_flight_create_waits_and_compensates() {
    let config = mock_servers::Config {
        latency: std::time::Duration::from_millis(80),
        ..mock_servers::Config::default()
    };
    let harness = Harness::start_with(config, support::options()).await;

    // 1. Undoing a `Create` while `send_calendar_change` is in flight queues a compensating Delete.
    let first = Event {
        id: Some("e0inflight001".into()),
        summary: Some("Phantom A".into()),
        start: Some(friday_at(9)),
        end: Some(friday_at(10)),
        ..Event::default()
    };
    harness
        .store
        .queue_calendar_change(
            10,
            CalendarChange::Create {
                calendar: ACCOUNT.into(),
                event: first,
            },
            None,
        )
        .await
        .expect("queues");
    let (due, _) = harness
        .store
        .due_calendar_changes(true)
        .await
        .expect("reads");
    let sending = {
        let store = harness.store.clone();
        let item = due[0].clone();
        tokio::spawn(async move { store.send_calendar_change(&item).await })
    };
    tokio::time::sleep(std::time::Duration::from_millis(20)).await;
    assert!(
        harness
            .store
            .unqueue_calendar_change(10)
            .await
            .expect("undoes in-flight create")
    );
    assert_eq!(
        sending.await.expect("joins").expect("sends"),
        CalendarOutcome::Sent
    );
    assert!(
        friday(&harness).await.is_empty(),
        "compensating Delete hides the created event immediately"
    );
    assert_eq!(flush(&harness).await, [CalendarOutcome::Sent]);
    assert_eq!(
        friday_titled(&harness, "Phantom A").await,
        0,
        "deleted from Google Calendar too"
    );

    // 2. Queueing `CalendarChange::Delete` while `Create` is in flight also waits and deletes it.
    let second = Event {
        id: Some("e0inflight002".into()),
        summary: Some("Phantom B".into()),
        start: Some(friday_at(11)),
        end: Some(friday_at(12)),
        ..Event::default()
    };
    harness
        .store
        .queue_calendar_change(
            11,
            CalendarChange::Create {
                calendar: ACCOUNT.into(),
                event: second,
            },
            None,
        )
        .await
        .expect("queues");
    let (due, _) = harness
        .store
        .due_calendar_changes(true)
        .await
        .expect("reads");
    let sending = {
        let store = harness.store.clone();
        let item = due[0].clone();
        tokio::spawn(async move { store.send_calendar_change(&item).await })
    };
    tokio::time::sleep(std::time::Duration::from_millis(20)).await;
    harness
        .store
        .queue_calendar_change(
            12,
            CalendarChange::Delete {
                calendar: ACCOUNT.into(),
                id: "e0inflight002".into(),
                notify: false,
            },
            None,
        )
        .await
        .expect("queues delete while create is in flight");
    assert_eq!(
        sending.await.expect("joins").expect("sends"),
        CalendarOutcome::Sent
    );
    assert!(friday(&harness).await.is_empty());
    assert_eq!(flush(&harness).await, [CalendarOutcome::Sent]);
    assert_eq!(
        friday_titled(&harness, "Phantom B").await,
        0,
        "no phantom live event remains on Google Calendar"
    );
}

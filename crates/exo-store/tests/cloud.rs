//! Handing reminders and scheduled sends to Examail Cloud. Exo waits out the grace past a
//! handed-off job's time, then does it itself only if Gmail says the service didn't. Here the
//! tests play the service, acting on the mock's Gmail directly.

mod support;

use std::time::Duration;

use exo_store::{
    Address, Draft, DraftAction, DraftRecord, ListId, Mutation, Options, SendLater, ThreadId,
    remind_key, send_key,
};
use jiff::{SignedDuration, Timestamp, ToSpan};
use support::Harness;

/// A second's grace, so a test can wait it out.
fn options() -> Options {
    Options {
        cloud_grace: SignedDuration::from_secs(1),
        ..support::options()
    }
}

/// A new message to Ann, sent later at `at`.
fn draft(id: &str, at: Timestamp) -> Draft {
    let email = gmail::mime::Email {
        from: Some(gmail::mime::Mailbox::new("Maya Okafor", support::ACCOUNT)),
        to: vec![gmail::mime::Mailbox::new("Ann", "ann@example.com")],
        subject: "Quarterly plan".into(),
        html: Some("<div>Here it is</div>".into()),
        text: Some("Here it is".into()),
        message_id: Some(format!("{id}@exo")),
        date: Some(Timestamp::now().to_zoned(jiff::tz::TimeZone::UTC)),
        ..gmail::mime::Email::default()
    };
    let record = DraftRecord {
        action: DraftAction::Compose,
        from: Some(Address {
            name: Some("Maya Okafor".into()),
            email: support::ACCOUNT.into(),
        }),
        to: vec![Address {
            name: Some("Ann".into()),
            email: "ann@example.com".into(),
        }],
        cc: Vec::new(),
        bcc: Vec::new(),
        subject: "Quarterly plan".into(),
        body: "<div>Here it is</div>".into(),
        quoted_content: None,
        quoted_inlined: false,
        in_reply_to: None,
        in_reply_to_rfc822: None,
        references: Vec::new(),
        rfc822_id: format!("{id}@exo"),
        fingerprint: String::new(),
        attachments: Vec::new(),
        send_later: Some(SendLater {
            at,
            abort_on_reply: false,
            current_message_ids: Vec::new(),
        }),
        reminder: None,
    };
    Draft {
        id: id.into(),
        thread: None,
        adopted: None,
        record,
        raw: String::from_utf8(email.to_rfc822()).expect("ASCII"),
        complete: true,
        sending: true,
    }
}

async fn schedule(harness: &Harness, draft: Draft) {
    let id = draft.id.clone();
    harness.store.save_draft(draft).await.expect("saves");
    harness.store.schedule_draft(&id).await.expect("schedules");
    harness.store.push_drafts().await.expect("pushes");
}

/// How many messages with Message-ID `id` are in Sent.
async fn sent(harness: &Harness, id: &str) -> usize {
    let query = gmail::ListQuery {
        q: Some(format!("rfc822msgid:{id}")),
        label_ids: vec!["SENT".into()],
        ..gmail::ListQuery::default()
    };
    harness
        .gmail
        .gmail()
        .messages(&query)
        .await
        .expect("messages.list")
        .messages
        .len()
}

async fn gmail_draft(harness: &Harness, id: &str) -> String {
    harness
        .store
        .draft(id)
        .await
        .expect("reads")
        .and_then(|draft| draft.gmail_id)
        .expect("in Gmail")
}

#[tokio::test]
async fn the_jobs_to_hand_off_are_the_waiting_reminders_and_written_sends() {
    let harness = Harness::synced_with(options()).await;
    let thread = harness.all(&ListId::Inbox).await.remove(0).id;
    let at = Timestamp::now().checked_add(2.hours()).unwrap();
    harness
        .store
        .apply(
            std::slice::from_ref(&thread),
            Mutation::Remind {
                at,
                keep_on_reply: false,
                snooze: true,
            },
        )
        .await
        .unwrap();
    schedule(&harness, draft("draft00000000000101", at)).await;
    let gmail_id = gmail_draft(&harness, "draft00000000000101").await;

    let jobs = harness.store.cloud_jobs().await.unwrap();
    let remind = jobs
        .iter()
        .find(|job| job.key == remind_key(thread.as_str()))
        .expect("the reminder");
    assert_eq!(remind.request["kind"], "remind");
    assert_eq!(remind.request["if_no_reply"], true);
    assert!(
        remind.request["snoozed_label"]
            .as_str()
            .is_some_and(|label| !label.is_empty())
    );
    let send = jobs
        .iter()
        .find(|job| job.key == send_key("draft00000000000101"))
        .expect("the send");
    assert_eq!(send.request["kind"], "send");
    assert_eq!(send.request["draft_id"], gmail_id.as_str());
    assert_eq!(send.request["rfc822_id"], "draft00000000000101@exo");
    assert!(
        send.request["message_id"].as_str().is_some(),
        "the message Gmail holds"
    );
}

#[tokio::test]
async fn a_handed_off_send_waits_out_the_grace_then_goes() {
    let harness = Harness::synced_with(options()).await;
    let at = Timestamp::now() + SignedDuration::from_secs(1);
    schedule(&harness, draft("draft00000000000102", at)).await;
    harness
        .store
        .set_handed_off([send_key("draft00000000000102")])
        .await
        .unwrap();
    tokio::time::sleep(Duration::from_millis(1200)).await;
    let report = harness.store.push_drafts().await.unwrap();
    assert!(report.sent.is_empty(), "the service's to send first");
    assert!(
        report
            .retry_at
            .is_some_and(|retry| retry > Timestamp::now()),
        "and Exo looks again after the grace"
    );
    tokio::time::sleep(Duration::from_millis(1200)).await;
    let report = harness.store.push_drafts().await.unwrap();
    assert_eq!(report.sent.len(), 1, "the service didn't, so Exo does");
    assert_eq!(sent(&harness, "draft00000000000102@exo").await, 1);
}

#[tokio::test]
async fn a_send_the_service_made_isnt_made_again() {
    let harness = Harness::synced_with(options()).await;
    let at = Timestamp::now() + SignedDuration::from_secs(1);
    schedule(&harness, draft("draft00000000000103", at)).await;
    harness
        .store
        .set_handed_off([send_key("draft00000000000103")])
        .await
        .unwrap();
    // The service sends it, as drafts.send does.
    let gmail_id = gmail_draft(&harness, "draft00000000000103").await;
    harness
        .gmail
        .gmail()
        .send_draft(&gmail_id)
        .await
        .expect("drafts.send");
    tokio::time::sleep(Duration::from_millis(2200)).await;
    harness.store.push_drafts().await.unwrap();
    assert_eq!(
        sent(&harness, "draft00000000000103@exo").await,
        1,
        "sent once"
    );
    assert!(
        harness
            .store
            .draft("draft00000000000103")
            .await
            .unwrap()
            .is_none(),
        "and Exo knows it went"
    );
}

#[tokio::test]
async fn a_send_the_service_made_but_left_the_draft_of_isnt_made_again() {
    let harness = Harness::synced_with(options()).await;
    let at = Timestamp::now() + SignedDuration::from_secs(1);
    let scheduled = draft("draft00000000000104", at);
    let raw = scheduled.raw.clone().into_bytes();
    schedule(&harness, scheduled).await;
    harness
        .store
        .set_handed_off([send_key("draft00000000000104")])
        .await
        .unwrap();
    // The service's send went, but its draft is still there.
    harness
        .gmail
        .gmail()
        .send(raw, None)
        .await
        .expect("messages.send");
    tokio::time::sleep(Duration::from_millis(2200)).await;
    harness.store.push_drafts().await.unwrap();
    assert_eq!(
        sent(&harness, "draft00000000000104@exo").await,
        1,
        "sent once"
    );
}

/// A thread snoozed until `at` and handed off.
async fn handed_off_reminder(harness: &Harness, at: Timestamp) -> ThreadId {
    let thread = harness.all(&ListId::Inbox).await.remove(0).id;
    harness
        .store
        .apply(
            std::slice::from_ref(&thread),
            Mutation::Remind {
                at,
                keep_on_reply: true,
                snooze: true,
            },
        )
        .await
        .unwrap();
    harness.store.flush().await.unwrap();
    harness
        .store
        .set_handed_off([remind_key(thread.as_str())])
        .await
        .unwrap();
    thread
}

#[tokio::test]
async fn a_handed_off_reminder_waits_out_the_grace_then_comes_back() {
    let harness = Harness::synced_with(options()).await;
    let at = Timestamp::now()
        .checked_add(1.hour())
        .unwrap()
        .round(jiff::Unit::Second)
        .unwrap();
    let thread = handed_off_reminder(&harness, at).await;
    assert_eq!(
        harness.store.next_reminder().await.unwrap(),
        Some(at + SignedDuration::from_secs(1)),
        "Exo's look is after the grace"
    );
    assert!(
        harness.store.return_reminders(at).await.unwrap().is_empty(),
        "the service's to do first"
    );
    let after = at + SignedDuration::from_secs(5);
    assert_eq!(
        harness.store.return_reminders(after).await.unwrap(),
        std::slice::from_ref(&thread),
        "the service didn't, so Exo does"
    );
}

#[tokio::test]
async fn a_reminder_the_service_did_ends_without_coming_back_again() {
    let harness = Harness::synced_with(options()).await;
    let at = Timestamp::now()
        .checked_add(1.hour())
        .unwrap()
        .round(jiff::Unit::Second)
        .unwrap();
    let thread = handed_off_reminder(&harness, at).await;
    let snoozed = harness
        .store
        .labels()
        .await
        .unwrap()
        .into_iter()
        .find(|label| label.name == "[Exo]/Is Snoozed")
        .unwrap()
        .id;
    // The service brought it back, and the user archived it again elsewhere.
    harness
        .gmail
        .gmail()
        .modify_thread(
            thread.as_str(),
            &gmail::ModifyThreadRequest {
                add_label_ids: Vec::new(),
                remove_label_ids: vec![snoozed],
            },
        )
        .await
        .expect("threads.modify");
    let after = at + SignedDuration::from_secs(5);
    assert!(
        harness
            .store
            .return_reminders(after)
            .await
            .unwrap()
            .is_empty(),
        "not brought back a second time"
    );
    assert_eq!(
        harness.store.next_reminder().await.unwrap(),
        None,
        "it's over"
    );
    assert_eq!(
        harness.store.pending().await.unwrap(),
        0,
        "nothing for Gmail"
    );
    assert!(
        !harness.ids(&ListId::Inbox).await.contains(thread.as_str()),
        "and Exo didn't put it in the inbox"
    );
}

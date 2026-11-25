//! Draft overlay marks and editable-draft queries used by thread views and full-text indexing.

use std::collections::{BTreeSet, HashMap, HashSet};

use jiff::Timestamp;

use super::model::{DraftRecord, SendFailure};
use crate::db::{DraftRow, FromRecord, FromSqlValue, query_rows};
use crate::error::Result;

/// How much of a message's text Gmail's snippets hold.
const SNIPPET_CHARS: usize = 200;

/// Metadata of an editable draft in a thread, loaded without its RFC 822 `raw` blob.
pub(crate) struct EditableDraft {
    pub id: String,
    pub message_id: Option<String>,
    pub revision: u64,
    pub body: String,
}

/// What Exo's drafts change in reads.
#[derive(Default)]
pub(crate) struct Marks {
    /// Draft message ids that belong to Exo's own drafts (`DraftRow`), so reads route and show
    /// them rather than treating them as drafts written elsewhere.
    pub exo_drafts: BTreeSet<String>,
    /// Draft messages that reads leave out: discarded drafts', and held sends' (a send on its way
    /// is no longer an unsent draft, so it leaves the Inbox and the `Draft to …` row).
    pub hidden: HashSet<String>,
    /// Failed drafts' messages, with why and when each send failed.
    pub failed: HashMap<String, (SendFailure, Timestamp)>,
    /// Threads with a send held or waiting to go.
    pub sending: HashSet<String>,
    /// Those sends' text, as Gmail would snippet it, which a waiting send's row in Scheduled
    /// shows.
    pub sending_snippets: HashMap<String, String>,
    /// Scheduled sends' messages, with their time. They show in their thread as messages, not
    /// hidden like the sends that undo send holds.
    pub scheduled: HashMap<String, Timestamp>,
    /// Threads with a scheduled send, with the earliest send's time and whether Gmail has the
    /// latest version of every one.
    pub scheduled_threads: HashMap<String, (Timestamp, bool)>,
}

/// Exo's drafts in `thread` being written (not held, failed or discarded), whose text the search
/// index takes from their records, as their messages are stored without bodies. Held and discarded
/// drafts' messages are hidden, and a failed or scheduled draft's message has its body.
pub(crate) async fn editable(
    exec: &mut dyn toasty::Executor,
    thread: &str,
) -> Result<Vec<EditableDraft>> {
    let f = DraftRow::fields();
    let rows = DraftRow::filter(f.thread_id().eq(thread))
        .select((
            f.id(),
            f.message_id(),
            f.record(),
            f.revision(),
            f.discarded(),
            f.send_at(),
            f.failure(),
        ))
        .exec(exec)
        .await?;
    Ok(rows
        .into_iter()
        .filter(|(_, _, _, _, discarded, send_at, failure)| {
            !*discarded && send_at.is_none() && failure.is_none()
        })
        .map(|(id, message_id, record, revision, ..)| EditableDraft {
            id,
            message_id,
            revision,
            body: record.0.body,
        })
        .collect())
}

struct MarkRow {
    thread_id: Option<String>,
    message_id: Option<String>,
    record: toasty::Json<DraftRecord>,
    revision: u64,
    pushed: u64,
    discarded: bool,
    send_at: Option<Timestamp>,
    failure: Option<String>,
    failed_at: Option<Timestamp>,
}

impl FromRecord for MarkRow {
    fn from_record(fields: Vec<toasty::stmt::Value>) -> Option<Self> {
        let [
            thread_id,
            message_id,
            record,
            revision,
            pushed,
            discarded,
            send_at,
            failure,
            failed_at,
        ]: [toasty::stmt::Value; 9] = fields.try_into().ok()?;
        Some(Self {
            thread_id: FromSqlValue::from_sql_value(thread_id)?,
            message_id: FromSqlValue::from_sql_value(message_id)?,
            record: FromSqlValue::from_sql_value(record)?,
            revision: FromSqlValue::from_sql_value(revision)?,
            pushed: FromSqlValue::from_sql_value(pushed)?,
            discarded: FromSqlValue::from_sql_value(discarded)?,
            send_at: FromSqlValue::from_sql_value(send_at)?,
            failure: FromSqlValue::from_sql_value(failure)?,
            failed_at: FromSqlValue::from_sql_value(failed_at)?,
        })
    }
}

pub(crate) async fn marks(exec: &mut dyn toasty::Executor) -> Result<Marks> {
    let mut marks = Marks::default();
    let rows = query_rows::<MarkRow>(
        exec,
        toasty::sql::query(
            "SELECT thread_id, message_id, record, revision, pushed, discarded, send_at, failure, failed_at FROM drafts",
        ),
    )
    .await?;
    for MarkRow {
        thread_id,
        message_id,
        record,
        revision,
        pushed,
        discarded,
        send_at,
        failure,
        failed_at,
    } in rows
    {
        let held = send_at.is_some() && !discarded;
        let scheduled =
            send_at.filter(|_| !discarded && failure.is_none() && record.0.send_later.is_some());
        if let (true, Some(thread)) = (held, &thread_id) {
            marks.sending.insert(thread.clone());
            let text = crate::search::whole_text(Some(&record.0.body), None);
            marks
                .sending_snippets
                .insert(thread.clone(), text.chars().take(SNIPPET_CHARS).collect());
        }
        if let (Some(at), Some(thread)) = (scheduled, &thread_id) {
            let persisted = pushed >= revision;
            let entry = marks
                .scheduled_threads
                .entry(thread.clone())
                .or_insert((at, persisted));
            *entry = (entry.0.min(at), entry.1 && persisted);
        }
        let Some(message) = message_id else {
            continue;
        };
        marks.exo_drafts.insert(message.clone());
        if let Some(at) = scheduled {
            marks.scheduled.insert(message, at);
        } else if discarded || held {
            marks.hidden.insert(message);
        } else if let (Some(reason), Some(at)) = (failure.as_deref(), failed_at) {
            marks
                .failed
                .insert(message, (SendFailure::from_code(reason), at));
        }
    }
    Ok(marks)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::drafts::{Address, Draft, DraftAction, DraftRecord};
    use crate::ids::ThreadId;
    use crate::store::Store;

    #[tokio::test]
    async fn marks_derives_exo_drafts_from_draft_rows_across_clear_cache() {
        let dir =
            std::env::temp_dir().join(format!("exo-store-draft-marks-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let store = Store::open(
            &dir,
            "maya@brightmoor.co",
            gmail::Endpoints::google(),
            gmail::StaticToken("unused".into()),
        )
        .await
        .expect("opens");
        store
            .save_draft(Draft {
                id: "draft00000000000001".into(),
                thread: Some(ThreadId::new("t1")),
                adopted: Some("m_draft_1".into()),
                record: DraftRecord {
                    action: DraftAction::Compose,
                    from: Some(Address::new("Maya", "maya@brightmoor.co")),
                    to: vec![Address::address("ada@example.com")],
                    cc: Vec::new(),
                    bcc: Vec::new(),
                    subject: "Hello".into(),
                    body: "<div>Body</div>".into(),
                    quoted_content: None,
                    quoted_inlined: false,
                    quote_signed: false,
                    in_reply_to: None,
                    in_reply_to_rfc822: None,
                    references: Vec::new(),
                    rfc822_id: "d1@exo".into(),
                    fingerprint: String::new(),
                    attachments: Vec::new(),
                    send_later: None,
                    reminder: None,
                },
                raw: String::new(),
                complete: true,
                sending: false,
            })
            .await
            .expect("saves");
        let mut db = store.inner.db.clone();
        let before = marks(&mut db).await.expect("marks");
        assert_eq!(before.exo_drafts, BTreeSet::from(["m_draft_1".to_owned()]));
        drop(db);
        drop(store);

        Store::clear_cache(&dir, "maya@brightmoor.co")
            .await
            .expect("clears");
        let reopened = Store::open(
            &dir,
            "maya@brightmoor.co",
            gmail::Endpoints::google(),
            gmail::StaticToken("unused".into()),
        )
        .await
        .expect("reopens");
        let mut db = reopened.inner.db.clone();
        let after = marks(&mut db).await.expect("marks");
        assert_eq!(
            after.exo_drafts,
            BTreeSet::from(["m_draft_1".to_owned()]),
            "preserved across clear_cache without kv[exo_drafts]"
        );
        drop(db);
        drop(reopened);
        let _ = std::fs::remove_dir_all(&dir);
    }
}

use std::time::Duration;

use exo_store::{
    AttachmentSource, Draft, DraftAttachment, DraftFailure, Mutation, SentDraft, Store, ThreadId,
};
use tokio::runtime::Runtime;

use super::{
    Event, block_on_runtime,
    ui_tasks::{Repaint, Requests, Wake, Wakes},
    worker::{Request, Worker},
};
use crate::recovery::Restarts;

/// How long quitting waits for held sends and saved drafts to go out, and how long signing out
/// waits for the drafts waiting out their pace.
pub(super) const QUIT_GRACE: Duration = Duration::from_secs(5);
/// How long signing out waits for the account's tasks to stop before its files go.
pub(super) const STOP_GRACE: Duration = Duration::from_secs(2);

/// Concurrent quit-flush handle for one open account (`Send`), run in parallel across accounts
/// when Exo quits.
pub(crate) struct QuitFlush<'a> {
    pub(super) runtime: &'a Runtime,
    pub(super) calendar: &'a crate::calendar::CalendarSync,
    pub(super) requests: &'a Requests,
    pub(super) flush: &'a Wake,
    pub(super) restarts: &'a Restarts,
}

impl QuitFlush<'_> {
    pub(crate) fn run(self) {
        if self.calendar.held() > 0 {
            block_on_runtime(self.runtime, self.calendar.send_now(QUIT_GRACE));
        }
        // Asked even when no snapshot shows a held send. The worker handles requests in turn, so a
        // Send still queued is held, then sent.
        let (requests, flush) = (self.requests.clone(), self.flush.clone());
        let _ = block_on_runtime(self.runtime, async move {
            let (done, wait) = tokio::sync::oneshot::channel();
            requests.post(Request::SendHeldNow(done));
            flush.wake();
            tokio::time::timeout(QUIT_GRACE, wait).await
        });
        block_on_runtime(self.runtime, self.restarts.stop(STOP_GRACE));
    }
}

/// Drains queued local draft saves on `View ▸ Reload` without expiring Undo-Send holds or
/// blocking on Gmail writes, then stops the account's tasks.
pub(super) fn drain_before_reload(runtime: &Runtime, requests: &Requests, restarts: &Restarts) {
    let requests = requests.clone();
    let _ = block_on_runtime(runtime, async move {
        let (done, wait) = tokio::sync::oneshot::channel();
        requests.post(Request::DrainBeforeReload(done));
        tokio::time::timeout(STOP_GRACE, wait).await
    });
    block_on_runtime(runtime, restarts.stop(STOP_GRACE));
}

impl Worker {
    /// Saves `draft` as `email` with the bytes of its attachments added, and returns how many
    /// couldn't be had. A draft missing some is kept on the device, not written to Gmail. It's
    /// saved again once a sync round that reaches Gmail has fetched the missing files
    /// ([`fetch_missing_files`](Self::fetch_missing_files)), if what kept them away passes.
    pub(super) async fn save_draft(
        &mut self,
        mut draft: Draft,
        email: gmail::mime::Email,
    ) -> usize {
        let mut full = email.clone();
        let missing = attach(&self.store, &mut full, &draft.record.attachments).await;
        if missing.files == 0 {
            self.incomplete.remove(&draft.id);
        } else {
            let again = missing.passing.then(|| Box::new((draft.clone(), email)));
            self.incomplete.insert(draft.id.clone(), again);
        }
        draft.raw = String::from_utf8_lossy(&full.to_rfc822()).into_owned();
        draft.complete = missing.files == 0;
        match self.store.save_draft(draft).await {
            Ok(()) => self.flush.wake(),
            Err(error) => log::error!("saving a draft: {error}"),
        }
        missing.files
    }

    /// Fetches the files that incomplete drafts lack into the draft files, in a task of its own (a
    /// fetch can wait out Gmail's rate limiting), then has the drafts saved again
    /// ([`complete_drafts`](Self::complete_drafts)).
    pub(super) fn fetch_missing_files(&mut self) {
        if self.fetching_files || !self.incomplete.values().any(Option::is_some) {
            return;
        }
        let files = self.store.draft_files().to_owned();
        let wanted: Vec<DraftAttachment> = self
            .incomplete
            .values()
            .flatten()
            .flat_map(|retry| shown(&retry.1, &retry.0.record.attachments))
            .filter(|attachment| !files.join(&attachment.uuid).exists())
            .cloned()
            .collect();
        self.fetching_files = true;
        let (store, requests, busy) = (
            self.store.clone(),
            self.requests.clone(),
            self.requests.busy(),
        );
        self.restarts
            .spawn_task(&tokio::runtime::Handle::current(), async move {
                let mut gone = Vec::new();
                for attachment in wanted {
                    if matches!(file_bytes(&store, &attachment).await, Err(false)) {
                        gone.push(attachment.uuid);
                    }
                }
                requests.post(Request::FilesFetched(gone));
                drop(busy);
            });
    }

    /// The first time a sync round reaches Gmail, hands back to the UI the drafts that an earlier
    /// run saved without some of their files, to save again (each draft's record makes its
    /// message), so they don't wait for their next edit.
    pub(super) async fn resave_incomplete(&mut self) {
        if std::mem::replace(&mut self.resaved, true) {
            return;
        }
        let drafts = match self.store.drafts().await {
            Ok(drafts) => drafts,
            Err(error) => return log::error!("reading drafts: {error}"),
        };
        let incomplete: Vec<String> = drafts
            .into_iter()
            .filter(|draft| {
                !draft.complete
                    && draft.failure.is_none()
                    && !self.incomplete.contains_key(&draft.id)
            })
            .map(|draft| draft.id)
            .collect();
        if !incomplete.is_empty() {
            let _ = self.events.send(Event::Incomplete(incomplete));
            self.repaint.request();
        }
    }

    /// Saves again the incomplete drafts whose files are all in the draft files now, so they go to
    /// Gmail and an account held off screen for them can close.
    pub(super) async fn complete_drafts(&mut self) {
        let files = self.store.draft_files().to_owned();
        let ready: Vec<(Draft, gmail::mime::Email)> = self
            .incomplete
            .values()
            .flatten()
            .filter(|retry| {
                shown(&retry.1, &retry.0.record.attachments)
                    .iter()
                    .all(|attachment| files.join(&attachment.uuid).exists())
            })
            .map(|retry| (**retry).clone())
            .collect();
        for (draft, email) in ready {
            log::info!("draft {} has its files now", draft.id);
            self.save_draft(draft, email).await;
        }
    }

    /// Gives `reminder` to a sent draft while still held, or to its thread if already sent.
    pub(super) async fn remind_sent(
        &self,
        draft: String,
        thread: Option<String>,
        reminder: Option<exo_store::DraftReminder>,
    ) {
        let held = self
            .store
            .remind_draft(&draft, reminder)
            .await
            .unwrap_or_else(|error| {
                log::error!("giving draft {draft} a reminder: {error}");
                false
            });
        if held {
            // Its thread's label changed.
            self.flush.wake();
        }
        if let Some(thread) = thread.filter(|_| !held) {
            let mutation = match reminder {
                Some(reminder) => Mutation::Remind {
                    at: reminder.at,
                    keep_on_reply: reminder.keep_on_reply,
                    snooze: false,
                },
                None => Mutation::CancelReminder { to_inbox: false },
            };
            match self.store.apply(&[ThreadId::new(thread)], mutation).await {
                Ok(_) => self.flush.wake(),
                Err(error) => log::error!("reminding a sent thread: {error}"),
            }
        }
    }

    /// Cancels a held send without touching any thread reminder.
    pub(super) async fn cancel_send(&self, id: String) {
        let held = self.store.cancel_send(&id).await.unwrap_or_else(|error| {
            log::error!("taking back draft {id}: {error}");
            false
        });
        let _ = self.events.send(Event::SendTakenBack { id, held });
        self.repaint.request();
    }

    /// Cancels a held send and removes the reminder it added to its thread.
    pub(super) async fn undo_send(&self, id: String) {
        let held = self.store.undo_send(&id).await.unwrap_or_else(|error| {
            log::error!("taking back draft {id}: {error}");
            false
        });
        // Its reminder's label comes off.
        self.flush.wake();
        let _ = self.events.send(Event::SendTakenBack { id, held });
        self.repaint.request();
    }

    /// Spawns an immediate out-of-pace push of all pending drafts.
    pub(super) fn spawn_write_drafts_now(&self) {
        let (store, events, flush, busy) = (
            self.store.clone(),
            self.events.clone(),
            self.flush.clone(),
            self.requests.busy(),
        );
        self.restarts
            .spawn_task(&tokio::runtime::Handle::current(), async move {
                write_drafts_now(&store, &events, &flush).await;
                drop(busy);
            });
    }

    /// Writes pending drafts up to [`QUIT_GRACE`] and reports remaining unsent counts.
    pub(super) fn spawn_unsent(&self) {
        // On signing out, drafts that only wait out their pace go first, so what's counted
        // is what Gmail can't take. Past the grace, the write goes on, and what it hasn't
        // written yet counts.
        let (store, events, flush, repaint, busy) = (
            self.store.clone(),
            self.events.clone(),
            self.flush.clone(),
            self.repaint.clone(),
            self.requests.busy(),
        );
        self.restarts
            .spawn_task(&tokio::runtime::Handle::current(), async move {
                let write = tokio::spawn({
                    let (store, events) = (store.clone(), events.clone());
                    async move { write_drafts_now(&store, &events, &flush).await }
                });
                let _ = tokio::time::timeout(QUIT_GRACE, write).await;
                let unsent = store.unsent().await.unwrap_or_else(|error| {
                    log::error!("counting unsent changes: {error}");
                    exo_store::Unsent::default()
                });
                let _ = events.send(Event::Unsent(unsent));
                repaint.request();
                drop(busy);
            });
    }

    /// Expires Undo-Send holds (leaving scheduled sends untouched) and pushes drafts before quit.
    pub(super) async fn send_held_now(&self, done: tokio::sync::oneshot::Sender<()>) {
        match self.store.drafts().await {
            Ok(drafts) => {
                let now = jiff::Timestamp::now();
                // Only undo send's holds, not scheduled sends, which wait for their time or
                // the next launch.
                for draft in drafts.iter().filter(|draft| {
                    draft.send_at.is_some_and(|at| at > now) && draft.record.send_later.is_none()
                }) {
                    if let Err(error) = self.store.send_draft(&draft.id, now).await {
                        log::error!("sending draft {} before quitting: {error}", draft.id);
                    }
                }
            }
            Err(error) => log::error!("reading drafts before quitting: {error}"),
        }
        // The held sends, and the drafts the app saved on its way out, backoffs or not.
        if let Err(error) = self.store.push_drafts_now().await {
            log::error!("writing drafts before quitting: {error}");
        }
        // Then the reminders and sends go to Examail Cloud, which does them while Examail is
        // closed, the ones just set or edited too.
        crate::cloud::hand_off_now(&self.email, Duration::from_secs(5)).await;
        let _ = done.send(());
    }
}

/// The files [`attach`] couldn't put on a message.
struct Missing {
    files: usize,
    /// Whether each was a Gmail part that Gmail couldn't hand over just then (out of reach, rate
    /// limiting, failing), so a later try may get it, rather than a file gone from disk or a part
    /// gone from Gmail.
    passing: bool,
}

/// Puts the bytes of a draft's `attachments` (the [`shown`] ones) on `email`, fetching Gmail parts
/// into the draft files the first time. Says what couldn't be had.
async fn attach(
    store: &Store,
    email: &mut gmail::mime::Email,
    attachments: &[DraftAttachment],
) -> Missing {
    let mut missing = Missing {
        files: 0,
        passing: true,
    };
    for attachment in shown(email, attachments) {
        match file_bytes(store, attachment).await {
            Ok(data) => email.attachments.push(gmail::mime::Attachment {
                filename: attachment.name.clone(),
                content_type: attachment.mime_type.clone(),
                data,
                content_id: attachment.content_id.clone(),
            }),
            Err(passing) => {
                missing.files += 1;
                missing.passing &= passing;
            }
        }
    }
    missing
}

/// The `attachments` of a draft that go on `email`, which is all but the inline images the email's
/// HTML no longer shows.
fn shown<'a>(
    email: &gmail::mime::Email,
    attachments: &'a [DraftAttachment],
) -> Vec<&'a DraftAttachment> {
    let html = email.html.as_deref().unwrap_or_default();
    attachments
        .iter()
        .filter(|attachment| {
            attachment
                .content_id
                .as_ref()
                .is_none_or(|cid| html.contains(&format!("cid:{cid}")))
        })
        .collect()
}

/// The bytes of `attachment`, from the draft files, or from Gmail into them the first time. On
/// failure, whether a later try may get them ([`passes`]).
async fn file_bytes(store: &Store, attachment: &DraftAttachment) -> Result<Vec<u8>, bool> {
    let path = store.draft_files().join(&attachment.uuid);
    match (std::fs::read(&path), &attachment.source) {
        (Ok(data), _) => Ok(data),
        (
            Err(error),
            AttachmentSource::Gmail {
                message_id,
                attachment_id,
                part_id,
            },
        ) if error.kind() == std::io::ErrorKind::NotFound => {
            match store
                .part_data(message_id, attachment_id.as_deref(), part_id)
                .await
            {
                Ok(data) => {
                    let kept = path
                        .parent()
                        .map_or(Ok(()), std::fs::create_dir_all)
                        .and_then(|()| std::fs::write(&path, &data));
                    if let Err(error) = kept {
                        log::warn!("keeping a draft's attachment: {error}");
                    }
                    Ok(data)
                }
                Err(error) => {
                    log::warn!("fetching an attachment of {message_id}: {error}");
                    Err(passes(&error))
                }
            }
        }
        (Err(error), _) => {
            log::warn!("reading a draft's attachment: {error}");
            Err(false)
        }
    }
}

/// Whether Gmail couldn't hand a part over for a reason that passes: it was out of reach, rate
/// limiting the account or signing it out, or failing (5xx). Not when it said the part is gone.
fn passes(error: &exo_store::Error) -> bool {
    match error {
        exo_store::Error::Offline(_)
        | exo_store::Error::Throttled { .. }
        | exo_store::Error::Unauthorized(_) => true,
        exo_store::Error::Gmail(error) => error.status().is_none_or(|status| status >= 500),
        _ => false,
    }
}

/// Tells the UI what writing drafts to Gmail did.
fn events_for_drafts(
    events: &std::sync::mpsc::Sender<Event>,
    saved: Vec<String>,
    sent: Vec<SentDraft>,
    failed: Vec<DraftFailure>,
    offline: bool,
    retrying: Vec<String>,
) {
    for id in saved {
        let _ = events.send(Event::DraftSaved(id));
    }
    for id in retrying {
        let _ = events.send(Event::SendRetrying(id));
    }
    for sent in sent {
        let _ = events.send(Event::DraftSent(sent));
    }
    for failure in failed {
        log::warn!("Gmail refused a draft: {failure:?}");
        let _ = events.send(Event::DraftFailed(failure));
    }
    if offline {
        let _ = events.send(Event::DraftsOffline);
    }
}

/// Writes every draft Gmail lacks now, its pace and backoff notwithstanding, and tells the UI what
/// that did. The push loop, woken after, takes over the retries. Runs off the worker, which a slow
/// upload would otherwise hold up.
pub(super) async fn write_drafts_now(
    store: &Store,
    events: &std::sync::mpsc::Sender<Event>,
    flush: &Wake,
) {
    match store.push_drafts_now().await {
        Ok(report) => events_for_drafts(
            events,
            report.saved,
            report.sent,
            report.failed,
            report.offline,
            report.retrying,
        ),
        Err(error) => log::error!("writing drafts now: {error}"),
    }
    flush.wake();
}

/// Waits until the clock reads `at`. A sleeping Mac stops a timer's clock (`Instant` doesn't
/// advance), so a send scheduled for the morning would go as late as the Mac slept. To prevent
/// that, the clock is read again at least every minute.
async fn sleep_until(at: jiff::Timestamp) {
    const CHECK: Duration = Duration::from_secs(60);
    while let Ok(left) = Duration::try_from(at.duration_since(jiff::Timestamp::now())) {
        if left.is_zero() {
            return;
        }
        tokio::time::sleep(left.min(CHECK)).await;
    }
}

/// Sends queued mutations and drafts whenever one is applied, undone or saved, and again when
/// a failed one or a held send is due.
pub(super) async fn flush_loop(
    store: Store,
    requests: Requests,
    mut wake: Wakes,
    sync_now: Wake,
    events: std::sync::mpsc::Sender<Event>,
    repaint: Repaint,
    reach: crate::unsubscribe::Reach,
) {
    let mut retry: Option<jiff::Timestamp> = None;
    loop {
        let _busy = tokio::select! {
            woken = wake.next() => match woken {
                Some(busy) => busy,
                None => return,
            },
            () = sleep_until(retry.unwrap_or_default()), if retry.is_some() => requests.busy(),
        };
        retry = None;
        match store.flush().await {
            Ok(report) => {
                if report.completed > 0 {
                    sync_now.wake();
                }
                for failed in report.failed {
                    log::warn!("giving up on {failed:?}");
                    let _ = events.send(Event::Failed {
                        mutation: failed.mutation,
                        undo: failed.undo,
                    });
                }
                retry = report.retry_at;
            }
            Err(error) => log::error!("flushing: {error}"),
        }
        match store.push_drafts().await {
            Ok(report) => {
                let notable = !report.saved.is_empty()
                    || !report.sent.is_empty()
                    || !report.failed.is_empty()
                    || !report.retrying.is_empty()
                    || report.offline;
                // A held send's reminder queued its label, so flush again at once.
                let reminded = report.reminded.then(jiff::Timestamp::now);
                events_for_drafts(
                    &events,
                    report.saved,
                    report.sent,
                    report.failed,
                    report.offline,
                    report.retrying,
                );
                if notable {
                    repaint.request();
                }
                retry = [retry, reminded, report.retry_at]
                    .into_iter()
                    .flatten()
                    .min();
            }
            Err(error) => log::error!("writing drafts: {error}"),
        }
        if let Some(at) = crate::unsubscribe::push(&store, reach).await {
            retry = Some(retry.map_or(at, |retry| retry.min(at)));
        }
        requests.post(Request::Refresh);
    }
}

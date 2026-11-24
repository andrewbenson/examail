//! Triage beyond single threads, done against Gmail directly: bulk actions on every conversation
//! a select-all reaches, loaded or not, bulk relabels of every thread a search finds (blocks,
//! moving a sender to Important or Other), and the sender filters that carry those choices to
//! future mail.
//!
//! A relabel goes straight to Gmail rather than through the outbox, since most of the threads
//! it touches aren't stored. Syncs along the way and afterwards bring the stored ones up to date.
//! Taking labels off stored threads is done on the messages that carry them, a thousand at a
//! time (`messages.batchModify`, 50 quota units), as for a single thread. Everything else is one
//! `threads.modify` (10 units) per thread.

use std::collections::{HashMap, HashSet};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

use gmail::{
    BatchModifyMessagesRequest, FilterAction, FilterCriteria, ListQuery, ModifyThreadRequest,
};
use jiff::Timestamp;
use serde::{Deserialize, Serialize};

use crate::db::ListEntry;
use crate::error::{Error, Result};
use crate::ids::labels::{SPAM, STARRED, TRASH, UNREAD};
use crate::ids::{Filter, ListId, ThreadId};
use crate::mutation::Mutation;
use crate::quota::Lane;
use crate::split::{Me, important_definition};
use crate::store::{Store, load_views};

/// Gmail calls a relabel runs at once.
const CONCURRENCY: usize = 8;
/// Retries per call.
const ATTEMPTS: u32 = 3;
/// A long relabel syncs after every this many threads, so the stored ones leave their lists as
/// it goes.
const SYNC_EVERY: usize = 200;
/// Threads read from the database at once.
const CHUNK: usize = 500;
/// Most message ids `messages.batchModify` takes.
const BATCH_IDS: usize = 1000;

/// The categories a sender filter can send mail to. A new filter for one replaces a filter on the
/// same sender for the other.
const CATEGORIES: [&str; 2] = ["CATEGORY_PERSONAL", "CATEGORY_SOCIAL"];

/// The threads a bulk action is for.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct BulkTarget {
    /// A select-all: every thread of a list at or before a date, loaded or not.
    pub list: Option<ListCutoff>,
    /// Threads it's for whatever their date (the loaded rows selected).
    pub include: Vec<ThreadId>,
    /// Threads it leaves alone (the loaded rows not selected).
    pub exclude: Vec<ThreadId>,
}

/// Every thread of a list or search (narrowed by `filter`) dated at or before `before`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ListCutoff {
    /// The list or search.
    pub scope: Scope,
    /// The filter narrowing it: Unread or Starred, the ones Gmail can search for.
    pub filter: Option<Filter>,
    /// Unread or Starred. Threads these filters pass are left alone (Get Me To Zero's
    /// `Keep unread emails` and `Keep starred emails`).
    pub keep: Vec<Filter>,
    /// The date of the row the select-all started from.
    pub before: Timestamp,
}

/// What a select-all is of.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Scope {
    /// A list.
    List(ListId),
    /// A search's results, by the query as typed. Its Gmail form is what the search itself asks
    /// Gmail.
    Search(String),
}

/// How far a bulk relabel (or its undo) has got, shared with whoever shows it, and a way to
/// stop it between rounds of Gmail calls.
#[derive(Debug, Default)]
pub struct Progress {
    total: AtomicUsize,
    done: AtomicUsize,
    stopped: AtomicBool,
    /// The bulk action's entry in the log ([`Store::bulk_log`]), which keeps what it has changed
    /// as it goes.
    log: Option<u64>,
}

impl Progress {
    /// The progress of running logged bulk action `log` (not of undoing it), whose entry keeps
    /// what it changes as it goes, so a quit midway loses no more than the last
    /// [`SYNC_EVERY`] threads of its undo.
    pub fn logged(log: u64) -> Self {
        Self {
            log: Some(log),
            ..Self::default()
        }
    }

    /// The progress of one that's over, having changed (or put back) `threads`.
    pub fn ended(threads: usize) -> Self {
        Self {
            total: AtomicUsize::new(threads),
            done: AtomicUsize::new(threads),
            ..Self::default()
        }
    }

    /// How many threads it's changing. It's 0 until they're all found.
    pub fn total(&self) -> usize {
        self.total.load(Ordering::Relaxed)
    }

    /// How many it has changed.
    pub fn done(&self) -> usize {
        self.done.load(Ordering::Relaxed)
    }

    /// Stops it after the calls in flight.
    pub fn stop(&self) {
        self.stopped.store(true, Ordering::Relaxed);
    }

    fn is_stopped(&self) -> bool {
        self.stopped.load(Ordering::Relaxed)
    }

    fn start(&self, total: usize) {
        self.total.store(total, Ordering::Relaxed);
        self.done.store(0, Ordering::Relaxed);
    }
}

/// What a bulk relabel changed, which undoing it reverses: threads with the labels each one
/// gained (only those it didn't have) and lost (only those it had).
#[derive(Debug, Default, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Relabelled {
    groups: Vec<Relabel>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct Relabel {
    changed: Changed,
    added: Vec<String>,
    removed: Vec<String>,
}

/// The threads a relabel changed.
#[derive(Debug, Default, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct Changed {
    /// Changed whole (`threads.modify`).
    threads: Vec<ThreadId>,
    /// Changed on these messages only (`messages.batchModify`).
    messages: Vec<(ThreadId, Vec<String>)>,
}

impl Changed {
    fn len(&self) -> usize {
        self.threads.len() + self.messages.len()
    }

    fn into_threads(self) -> Vec<ThreadId> {
        self.threads
            .into_iter()
            .chain(self.messages.into_iter().map(|(thread, _)| thread))
            .collect()
    }
}

impl Relabelled {
    /// How many threads changed.
    pub fn len(&self) -> usize {
        self.groups.iter().map(|group| group.changed.len()).sum()
    }

    /// Nothing changed.
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// A relabel of `added` and `removed` about to start, with nothing changed yet.
    fn starting(added: &[String], removed: &[String]) -> Self {
        let mut relabelled = Self::default();
        relabelled.start(added, removed);
        relabelled
    }

    /// Starts recording a group of threads that gain `added` and lose `removed`.
    fn start(&mut self, added: &[String], removed: &[String]) {
        self.groups.push(Relabel {
            changed: Changed::default(),
            added: added.to_vec(),
            removed: removed.to_vec(),
        });
    }

    /// The group being recorded.
    fn current(&mut self) -> &mut Changed {
        &mut self
            .groups
            .last_mut()
            .expect("a group is started before its threads change")
            .changed
    }
}

/// The Gmail label carried by the threads that a filter passes. Only Unread's and Starred's are
/// labels.
fn label_of(filter: Filter) -> Result<&'static str> {
    match filter {
        Filter::Unread => Ok(UNREAD),
        Filter::Starred => Ok(STARRED),
        Filter::Important => Err(Error::Unsearchable("Important")),
        Filter::NoReply => Err(Error::Unsearchable("No reply")),
    }
}

/// `{query} before:{t}` and the filter's operator, with the list's own query in parentheses. An
/// `OR` in it (a split's query is the user's) then groups inside them and can't reach past the
/// cutoff. `before:` takes seconds and excludes the second it names, so using the second after
/// the cutoff's keeps threads dated within the cutoff's second.
fn before_query(query: Option<&str>, filter: Option<Filter>, before: Timestamp) -> Result<String> {
    let filter = match filter.map(label_of).transpose()? {
        None => None,
        Some(UNREAD) => Some("is:unread"),
        Some(_) => Some("is:starred"),
    };
    let seconds = before.as_second() + 1;
    let mut parts: Vec<String> = query
        .filter(|query| !query.trim().is_empty())
        .map(|query| format!("({query})"))
        .into_iter()
        .collect();
    parts.extend(filter.map(str::to_owned));
    parts.push(format!("before:{seconds}"));
    Ok(parts.join(" "))
}

/// `{query} after:{t}`, which finds what the list has dated at or after the cutoff's second,
/// whatever the filter.
fn after_query(query: Option<&str>, before: Timestamp) -> String {
    let seconds = before.as_second();
    match query.filter(|query| !query.trim().is_empty()) {
        Some(query) => format!("({query}) after:{seconds}"),
        None => format!("after:{seconds}"),
    }
}

impl Store {
    /// Every thread Gmail finds for `query`, newest first.
    ///
    /// # Errors
    /// When Gmail can't be reached or refuses the query.
    pub async fn find_threads(&self, query: &str) -> Result<Vec<ThreadId>> {
        self.find(ListQuery {
            q: Some(query.to_owned()),
            ..ListQuery::default()
        })
        .await
    }

    /// Every thread Gmail lists for `query`, through every page.
    async fn find(&self, mut query: ListQuery) -> Result<Vec<ThreadId>> {
        let mut found = Vec::new();
        query.max_results = Some(500);
        loop {
            let page = self
                .inner
                .api
                .call(Lane::Sync, ATTEMPTS, |gmail| gmail.threads(&query))
                .await?;
            found.extend(
                page.threads
                    .into_iter()
                    .filter_map(|thread| thread.id)
                    .map(ThreadId::new),
            );
            match page.next_page_token {
                Some(token) => query.page_token = Some(token),
                None => return Ok(found),
            }
        }
    }

    /// Every thread of `cutoff`'s list that Gmail finds at or before the cutoff's date, carrying
    /// `label` if one is given.
    async fn find_before(&self, cutoff: &ListCutoff, label: Option<&str>) -> Result<Vec<ThreadId>> {
        let mut query = self.scope_query(&cutoff.scope).await?;
        query.q = Some(before_query(
            query.q.as_deref(),
            cutoff.filter,
            cutoff.before,
        )?);
        if let Some(label) = label {
            query.label_ids.push(label.to_owned());
            query.include_spam_trash |= [SPAM, TRASH].contains(&label);
        }
        self.find(query).await
    }

    /// Every thread of `cutoff`'s list that Gmail finds with mail since the cutoff's date.
    /// Gmail's `after:` has 1-second resolution, so threads whose newest mail falls in the
    /// cutoff's own second are checked by millisecond `internalDate`.
    async fn find_after(&self, cutoff: &ListCutoff) -> Result<Vec<ThreadId>> {
        let base = self.scope_query(&cutoff.scope).await?;
        let sec = cutoff.before.as_second();
        let next_sec = Timestamp::from_second(sec.saturating_add(1)).unwrap_or(Timestamp::MAX);
        let mut strictly_after = base.clone();
        strictly_after.q = Some(after_query(base.q.as_deref(), next_sec));
        let mut newer: HashSet<ThreadId> = self.find(strictly_after).await?.into_iter().collect();

        let before_ms = cutoff.before.as_millisecond();
        let next_ms = next_sec.as_millisecond();
        let required_labels = base.label_ids.clone();
        let include_spam_trash = base.include_spam_trash;
        let mut boundary = base;
        boundary.q = Some(format!(
            "{} before:{}",
            after_query(boundary.q.as_deref(), cutoff.before),
            sec.saturating_add(1)
        ));
        let candidates: Vec<ThreadId> = self
            .find(boundary)
            .await?
            .into_iter()
            .filter(|thread| !newer.contains(thread))
            .collect();
        let drafts_count = matches!(cutoff.scope, Scope::List(ListId::Drafts));
        let sent_scope = matches!(cutoff.scope, Scope::List(ListId::Sent));
        for round in candidates.chunks(CONCURRENCY) {
            let fetched = futures::future::join_all(round.iter().map(|thread| {
                let id = thread.as_str().to_owned();
                async move {
                    let result = self
                        .inner
                        .api
                        .call(Lane::Sync, ATTEMPTS, |gmail| {
                            gmail.thread(&id, gmail::Format::Minimal, &[])
                        })
                        .await;
                    (thread.clone(), result)
                }
            }))
            .await;
            for (thread, result) in fetched {
                match result {
                    Ok(full) => {
                        let has_newer = full.messages.iter().any(|message| {
                            let date = message.internal_date.unwrap_or_default();
                            if date <= before_ms || date >= next_ms {
                                return false;
                            }
                            let has = |label: &str| message.label_ids.iter().any(|l| l == label);
                            let draft = !drafts_count && has(crate::ids::labels::DRAFT);
                            let chat = has(crate::ids::labels::CHAT);
                            let spam_or_trash = !include_spam_trash
                                && (has(crate::ids::labels::SPAM)
                                    || has(crate::ids::labels::TRASH));
                            let own_reply = !sent_scope
                                && has(crate::ids::labels::SENT)
                                && !has(crate::ids::labels::INBOX);
                            !draft
                                && !chat
                                && !spam_or_trash
                                && !own_reply
                                && required_labels.iter().all(|label| has(label))
                        });
                        if has_newer {
                            newer.insert(thread);
                        }
                    }
                    Err(failure) if failure.is_not_found() => {}
                    Err(failure) => return Err(failure.into()),
                }
            }
        }
        Ok(newer.into_iter().collect())
    }

    /// How Gmail is asked for `scope`'s threads: the list's own query (the one backfill uses),
    /// or the search's Gmail form.
    async fn scope_query(&self, scope: &Scope) -> Result<ListQuery> {
        let router = self.inner.router();
        Ok(match scope {
            Scope::List(list) => crate::sync::selection(list, &router)?,
            Scope::Search(raw) => {
                let labels = crate::search::label_index(&mut self.inner.db.clone()).await?;
                let me = Me {
                    addresses: &router.me.addresses,
                    name: router.me.name.as_deref(),
                };
                let parsed = crate::query::SearchQuery::parse(raw);
                let q = parsed.to_gmail(&labels, &router.me.addresses, &important_definition(&me));
                ListQuery {
                    q: Some(q),
                    include_spam_trash: parsed.includes_spam_trash(),
                    ..ListQuery::default()
                }
            }
        })
    }

    /// Adds `add` and removes `remove` on every message of `threads`, then syncs so stored
    /// threads show it. Returns the threads Gmail changed. A thread that's gone is skipped.
    ///
    /// # Errors
    /// When Gmail can't be reached. Threads already changed stay changed.
    pub async fn relabel(
        &self,
        threads: &[ThreadId],
        add: &[String],
        remove: &[String],
    ) -> Result<Vec<ThreadId>> {
        let mut changed = Relabelled::starting(add, remove);
        self.modify_threads(threads, add, remove, &Progress::default(), &mut changed)
            .await?;
        self.sync().await?;
        Ok(changed
            .groups
            .into_iter()
            .flat_map(|group| group.changed.into_threads())
            .collect())
    }

    /// Applies `mutation` to every thread of `target`, to whole threads, recording in `done`
    /// what changed as it goes, so even a failed or stopped bulk action can be undone
    /// ([`Self::undo_bulk`]). A thread is changed only where it differs (a label added where it's
    /// missing, removed where it's there), which is what makes the undo exact. Stored threads are
    /// read as the user sees them, and for the others Gmail is asked which carry each label. A
    /// stored thread that the list's query finds is only taken when the list on screen has it at
    /// or before the cutoff, because the query only approximates the list.
    ///
    /// # Errors
    /// When Gmail can't be reached or refuses a query, when the list's filter can't be searched
    /// for ([`Error::Unsearchable`]), or for muting before the first sync.
    pub async fn bulk(
        &self,
        target: &BulkTarget,
        mutation: &Mutation,
        progress: &Progress,
        done: &mut Relabelled,
    ) -> Result<()> {
        let muted = match mutation {
            Mutation::Mute | Mutation::Unmute => self
                .inner
                .router()
                .me
                .muted_label
                .ok_or(Error::NotSynced("the mute label"))?,
            _ => String::new(),
        };
        let Some((add, remove)) = mutation.label_change(&muted) else {
            return Ok(());
        };
        let keep = target
            .list
            .iter()
            .flat_map(|cutoff| &cutoff.keep)
            .map(|filter| label_of(*filter).map(str::to_owned))
            .collect::<Result<Vec<String>>>()?;
        let labels: Vec<String> = add.iter().chain(&remove).chain(&keep).cloned().collect();
        let (threads, carried) = self.bulk_threads(target, &labels).await?;
        let has = |thread: &ThreadId, label: &str| {
            carried
                .get(thread.as_str())
                .is_some_and(|labels| labels.contains(label))
        };
        let mut groups: Vec<(Vec<ThreadId>, Vec<String>, Vec<String>)> = Vec::new();
        for thread in threads
            .into_iter()
            .filter(|thread| !keep.iter().any(|label| has(thread, label)))
        {
            let added: Vec<String> = add
                .iter()
                .filter(|label| !has(&thread, label))
                .cloned()
                .collect();
            let removed: Vec<String> = remove
                .iter()
                .filter(|label| has(&thread, label))
                .cloned()
                .collect();
            if added.is_empty() && removed.is_empty() {
                continue;
            }
            match groups.iter_mut().find(|(_, group_added, group_removed)| {
                *group_added == added && *group_removed == removed
            }) {
                Some((threads, _, _)) => threads.push(thread),
                None => groups.push((vec![thread], added, removed)),
            }
        }
        progress.start(groups.iter().map(|(threads, _, _)| threads.len()).sum());
        for (threads, added, removed) in groups {
            done.start(&added, &removed);
            self.modify_threads(&threads, &added, &removed, progress, done)
                .await?;
        }
        self.sync().await?;
        Ok(())
    }

    /// Undoes [`Self::bulk`]: every thread gets back the labels it lost and loses the ones it
    /// gained.
    ///
    /// # Errors
    /// When Gmail can't be reached. What was put back by then stays put back.
    pub async fn undo_bulk(&self, done: &Relabelled, progress: &Progress) -> Result<()> {
        progress.start(done.len());
        for group in done.groups.iter().rev() {
            let mut changed = Relabelled::starting(&group.removed, &group.added);
            let whole = self
                .batch_modify(
                    &group.changed.messages,
                    &group.removed,
                    &group.added,
                    progress,
                    &mut changed,
                )
                .await?;
            let threads: Vec<ThreadId> =
                group.changed.threads.iter().cloned().chain(whole).collect();
            self.modify_whole(
                &threads,
                &group.removed,
                &group.added,
                progress,
                &mut changed,
            )
            .await?;
        }
        self.sync().await?;
        Ok(())
    }

    /// The threads `target` names, the included first, and which of `labels` (the ones the
    /// action changes) each carries, as the user sees it.
    async fn bulk_threads(
        &self,
        target: &BulkTarget,
        labels: &[String],
    ) -> Result<(Vec<ThreadId>, HashMap<String, HashSet<String>>)> {
        let excluded: HashSet<&ThreadId> = target.exclude.iter().collect();
        let mut seen: HashSet<ThreadId> = HashSet::new();
        let mut threads: Vec<ThreadId> = target
            .include
            .iter()
            .filter(|thread| !excluded.contains(thread) && seen.insert((*thread).clone()))
            .cloned()
            .collect();
        let found: Vec<ThreadId> = match &target.list {
            Some(cutoff) => {
                // `before:` finds a thread by any message before the cutoff. One with mail since
                // is dated after it, above the select-all, whatever the store has of it.
                let newer: HashSet<ThreadId> = self.find_after(cutoff).await?.into_iter().collect();
                self.find_before(cutoff, None)
                    .await?
                    .into_iter()
                    .filter(|thread| {
                        !excluded.contains(thread)
                            && !seen.contains(thread)
                            && !newer.contains(thread)
                    })
                    .collect()
            }
            None => Vec::new(),
        };
        let candidates: Vec<String> = threads
            .iter()
            .chain(&found)
            .map(|thread| thread.as_str().to_owned())
            .collect();
        let mut db = self.inner.db.clone();
        let mut carried: HashMap<String, HashSet<String>> = HashMap::new();
        for chunk in candidates.chunks(CHUNK) {
            for (id, view) in load_views(&mut db, chunk).await? {
                let has: HashSet<String> = labels
                    .iter()
                    .filter(|label| view.has(label))
                    .cloned()
                    .collect();
                carried.insert(id, has);
            }
        }
        if let Some(cutoff) = &target.list {
            let (stored, unstored): (Vec<ThreadId>, Vec<ThreadId>) = found
                .into_iter()
                .partition(|thread| carried.contains_key(thread.as_str()));
            let stored: Vec<String> = stored
                .iter()
                .map(|thread| thread.as_str().to_owned())
                .collect();
            let listed = self.listed_by(cutoff, &stored, &mut db).await?;
            if !unstored.is_empty() {
                let unstored: HashSet<&ThreadId> = unstored.iter().collect();
                for label in labels {
                    for thread in self.find_before(cutoff, Some(label)).await? {
                        if unstored.contains(&thread) {
                            carried
                                .entry(thread.as_str().to_owned())
                                .or_default()
                                .insert(label.clone());
                        }
                    }
                }
            }
            threads.extend(
                stored
                    .into_iter()
                    .filter(|thread| listed.contains(thread))
                    .map(ThreadId::new),
            );
            threads.extend(unstored);
        }
        Ok((threads, carried))
    }

    /// Of the stored `threads`, those that `cutoff`'s list or search has at or before the
    /// cutoff's date and that its filter passes.
    async fn listed_by(
        &self,
        cutoff: &ListCutoff,
        threads: &[String],
        db: &mut toasty::Db,
    ) -> Result<HashSet<String>> {
        let fields = ListEntry::fields();
        let mut listed = HashSet::new();
        let before = cutoff.before.as_millisecond();
        for chunk in threads.chunks(CHUNK) {
            let mut passing: HashSet<String> = match &cutoff.scope {
                Scope::List(list) => ListEntry::filter(
                    fields
                        .thread_id()
                        .in_list(chunk)
                        .and(fields.list_id().in_list(list.members()))
                        .and(fields.sort().le(before)),
                )
                .exec(db)
                .await?
                .into_iter()
                .map(|entry| entry.thread_id)
                .collect(),
                Scope::Search(_) => load_views(db, chunk)
                    .await?
                    .into_iter()
                    .filter(|(_, view)| view.gmail_sort() <= before)
                    .map(|(id, _)| id)
                    .collect(),
            };
            if let Some(filter) = cutoff.filter {
                let filtered: HashSet<String> = ListEntry::filter(
                    fields
                        .thread_id()
                        .in_list(chunk)
                        .and(fields.list_id().eq(filter.key())),
                )
                .exec(db)
                .await?
                .into_iter()
                .map(|entry| entry.thread_id)
                .collect();
                passing.retain(|thread| filtered.contains(thread));
            }
            listed.extend(passing);
        }
        Ok(listed)
    }

    /// Adds `add` and removes `remove` on every message of `threads` until done or `progress`
    /// is stopped, counting them in `progress` and recording in `changed`'s current group what
    /// Gmail changed. A relabel that only removes takes the labels off the stored threads'
    /// messages that carry them in batches. The rest are changed whole.
    async fn modify_threads(
        &self,
        threads: &[ThreadId],
        add: &[String],
        remove: &[String],
        progress: &Progress,
        changed: &mut Relabelled,
    ) -> Result<()> {
        let (carrying, mut whole) = if add.is_empty() {
            self.carrying(threads, remove).await?
        } else {
            (Vec::new(), threads.to_vec())
        };
        whole.extend(
            self.batch_modify(&carrying, add, remove, progress, changed)
                .await?,
        );
        self.modify_whole(&whole, add, remove, progress, changed)
            .await
    }

    /// Of `threads`, the stored ones with the messages carrying any of `labels` as the user
    /// sees them, and the others.
    async fn carrying(
        &self,
        threads: &[ThreadId],
        labels: &[String],
    ) -> Result<(Vec<(ThreadId, Vec<String>)>, Vec<ThreadId>)> {
        let mut db = self.inner.db.clone();
        let (mut carrying, mut others) = (Vec::new(), Vec::new());
        for chunk in threads.chunks(CHUNK) {
            let ids: Vec<String> = chunk
                .iter()
                .map(|thread| thread.as_str().to_owned())
                .collect();
            let views = load_views(&mut db, &ids).await?;
            for thread in chunk {
                match views.get(thread.as_str()) {
                    Some(view) => carrying.push((thread.clone(), view.carriers(labels))),
                    None => others.push(thread.clone()),
                }
            }
        }
        Ok((carrying, others))
    }

    /// `messages.batchModify` on the messages of `threads`, up to [`BATCH_IDS`] a call, until
    /// done or `progress` is stopped. Returns the threads of a batch Gmail refused (a message
    /// deleted since it was stored), to be changed whole instead.
    async fn batch_modify(
        &self,
        threads: &[(ThreadId, Vec<String>)],
        add: &[String],
        remove: &[String],
        progress: &Progress,
        changed: &mut Relabelled,
    ) -> Result<Vec<ThreadId>> {
        let mut refused = Vec::new();
        let mut rest = threads;
        while !rest.is_empty() && !progress.is_stopped() {
            // Whole threads to a batch, as many as fit (a thread with more than fits goes alone).
            let mut ids = 0;
            let taken = rest
                .iter()
                .take_while(|(_, messages)| {
                    ids += messages.len();
                    ids <= BATCH_IDS
                })
                .count()
                .max(1);
            let (batch, after) = rest.split_at(taken);
            rest = after;
            let request = BatchModifyMessagesRequest {
                ids: batch
                    .iter()
                    .flat_map(|(_, messages)| messages.iter().cloned())
                    .collect(),
                add_label_ids: add.to_vec(),
                remove_label_ids: remove.to_vec(),
                ..BatchModifyMessagesRequest::default()
            };
            let sent = match request.ids.len() {
                0 => Ok(()),
                1..=BATCH_IDS => {
                    self.inner
                        .api
                        .call(Lane::Sync, ATTEMPTS, |gmail| {
                            gmail.batch_modify_messages(&request)
                        })
                        .await
                }
                _ => {
                    refused.push(batch[0].0.clone());
                    continue;
                }
            };
            match sent {
                Ok(()) => changed.current().messages.extend_from_slice(batch),
                Err(failure) if failure.is_permanent() => {
                    log::info!(
                        "Gmail refused a batch of {} messages ({}); changing their threads whole",
                        request.ids.len(),
                        failure.message()
                    );
                    refused.extend(batch.iter().map(|(thread, _)| thread.clone()));
                    continue;
                }
                Err(failure) => return Err(failure.into()),
            }
            self.progressed(progress, batch.len(), changed).await?;
        }
        Ok(refused)
    }

    /// `threads.modify` on `threads`, a round of [`CONCURRENCY`] at a time until done or
    /// `progress` is stopped. A thread that's gone is skipped.
    async fn modify_whole(
        &self,
        threads: &[ThreadId],
        add: &[String],
        remove: &[String],
        progress: &Progress,
        changed: &mut Relabelled,
    ) -> Result<()> {
        let request = ModifyThreadRequest {
            add_label_ids: add.to_vec(),
            remove_label_ids: remove.to_vec(),
        };
        // In rounds rather than a stream, because a stream's closure makes the future unsendable
        // (rust-lang/rust#100013), and tasks spawn this.
        for round in threads.chunks(CONCURRENCY) {
            if progress.is_stopped() {
                break;
            }
            for (thread, result) in futures::future::join_all(
                round
                    .iter()
                    .map(|thread| self.modify_thread(thread, &request)),
            )
            .await
            {
                match result {
                    Ok(()) => changed.current().threads.push(thread),
                    Err(failure) if failure.is_not_found() => {}
                    Err(failure) => return Err(failure.into()),
                }
            }
            self.progressed(progress, round.len(), changed).await?;
        }
        Ok(())
    }

    /// Counts `threads` more done, syncing every [`SYNC_EVERY`] threads, so the stored ones
    /// leave their lists as the relabel goes. A logged relabel's entry then gets what it has
    /// `changed` so far.
    async fn progressed(
        &self,
        progress: &Progress,
        threads: usize,
        changed: &Relabelled,
    ) -> Result<()> {
        let done = progress.done.fetch_add(threads, Ordering::Relaxed) + threads;
        if done / SYNC_EVERY > (done - threads) / SYNC_EVERY {
            self.sync().await?;
            if let Some(log) = progress.log {
                self.checkpoint_bulk(log, changed).await?;
            }
        }
        Ok(())
    }

    /// `threads.modify` on `thread`, paired with it.
    async fn modify_thread(
        &self,
        thread: &ThreadId,
        request: &ModifyThreadRequest,
    ) -> (ThreadId, Result<(), crate::api::Failure>) {
        let result = self
            .inner
            .api
            .call(Lane::Sync, ATTEMPTS, |gmail| {
                gmail.modify_thread(thread.as_str(), request)
            })
            .await;
        (thread.clone(), result.map(drop))
    }

    /// Creates the filter that gives mail from `from` the label `label`, shaped as Exo's own
    /// filters are (`from` alone, one label added). `TRASH` blocks a sender or domain, and a
    /// category sends them to Important or Other. A category filter replaces the sender's filter
    /// for the other category. Returns the new filter's id, or none when Gmail already had the
    /// filter (made in Gmail on the web, say, in which case undo leaves it), and the replaced
    /// filter.
    ///
    /// # Errors
    /// When Gmail can't be reached or refuses the filter.
    pub async fn create_sender_filter(
        &self,
        from: &str,
        label: &str,
    ) -> Result<(Option<String>, Option<gmail::Filter>)> {
        let criteria = FilterCriteria {
            from: Some(from.to_owned()),
            ..FilterCriteria::default()
        };
        let action = FilterAction {
            add_label_ids: vec![label.to_owned()],
            ..FilterAction::default()
        };
        let same = |filter: &gmail::Filter| {
            filter.criteria.as_ref() == Some(&criteria) && filter.action.as_ref() == Some(&action)
        };
        let filters = self
            .inner
            .api
            .call(Lane::Act, ATTEMPTS, |gmail| gmail.filters())
            .await?
            .filter;
        let replaced = filters
            .iter()
            .find(|filter| {
                let adds = filter
                    .action
                    .as_ref()
                    .map(|action| action.add_label_ids.clone())
                    .unwrap_or_default();
                CATEGORIES.contains(&label)
                    && filter.criteria.as_ref() == Some(&criteria)
                    && adds
                        .iter()
                        .any(|added| CATEGORIES.contains(&added.as_str()) && added != label)
            })
            .cloned();
        let filter = gmail::Filter {
            id: None,
            criteria: Some(criteria.clone()),
            action: Some(action.clone()),
        };
        // The new filter first: if Gmail won't make it, the old one stays. If the old one then
        // can't go, the new one goes again, since a failed job leaves undo nothing to undo.
        let id = if filters.iter().any(same) {
            None
        } else {
            Some(
                match self
                    .inner
                    .api
                    .call(Lane::Act, ATTEMPTS, |gmail| gmail.create_filter(&filter))
                    .await
                {
                    Ok(created) => created.id.unwrap_or_default(),
                    // An attempt whose answer was lost made it, so the retry was refused as a
                    // duplicate. The filter there now is this one, since it wasn't there before.
                    Err(failure) => {
                        let made = self
                            .inner
                            .api
                            .call(Lane::Act, ATTEMPTS, |gmail| gmail.filters())
                            .await
                            .ok()
                            .and_then(|list| {
                                list.filter
                                    .into_iter()
                                    .find(|filter| same(filter))
                                    .and_then(|filter| filter.id)
                            });
                        made.ok_or(failure)?
                    }
                },
            )
        };
        if let Some(old) = replaced.as_ref().and_then(|old| old.id.clone())
            && let Err(failure) = self
                .inner
                .api
                .call(Lane::Act, ATTEMPTS, |gmail| gmail.delete_filter(&old))
                .await
        {
            if let Some(id) = &id
                && let Err(error) = self
                    .inner
                    .api
                    .call(Lane::Act, ATTEMPTS, |gmail| gmail.delete_filter(id))
                    .await
            {
                log::warn!("taking back filter {id}: {}", error.message());
            }
            return Err(failure.into());
        }
        Ok((id, replaced))
    }

    /// Deletes filter `id`, if it was made, and puts back `replaced` (undoing
    /// [`Self::create_sender_filter`]).
    ///
    /// # Errors
    /// When Gmail can't be reached.
    pub async fn delete_sender_filter(
        &self,
        id: Option<&str>,
        replaced: Option<&gmail::Filter>,
    ) -> Result<()> {
        if let Some(id) = id {
            match self
                .inner
                .api
                .call(Lane::Act, ATTEMPTS, |gmail| gmail.delete_filter(id))
                .await
            {
                Err(failure) if failure.is_not_found() => {}
                result => result?,
            }
        }
        if let Some(filter) = replaced {
            let filter = gmail::Filter {
                id: None,
                ..filter.clone()
            };
            self.inner
                .api
                .call(Lane::Act, ATTEMPTS, |gmail| gmail.create_filter(&filter))
                .await?;
        }
        Ok(())
    }

    /// Unblocks `from`, an address or domain. It deletes the filters that block it, which are
    /// those shaped exactly as a block makes them (`from` alone, Trash added and nothing else),
    /// and leaves what they trashed in Trash. Returns how many it deleted.
    ///
    /// # Errors
    /// When Gmail can't be reached.
    pub async fn delete_block_filters(&self, from: &str) -> Result<usize> {
        let block = (
            FilterCriteria {
                from: Some(from.to_owned()),
                ..FilterCriteria::default()
            },
            FilterAction {
                add_label_ids: vec![TRASH.to_owned()],
                ..FilterAction::default()
            },
        );
        let filters = self
            .inner
            .api
            .call(Lane::Act, ATTEMPTS, |gmail| gmail.filters())
            .await?
            .filter;
        let mut deleted = 0;
        for filter in filters {
            let shape = (
                filter.criteria.clone().unwrap_or_default(),
                filter.action.clone().unwrap_or_default(),
            );
            let Some(id) = filter.id.filter(|_| shape == block) else {
                continue;
            };
            match self
                .inner
                .api
                .call(Lane::Act, ATTEMPTS, |gmail| gmail.delete_filter(&id))
                .await
            {
                Err(failure) if failure.is_not_found() => {}
                result => result?,
            }
            deleted += 1;
        }
        Ok(deleted)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_cutoff_and_filter_go_outside_the_lists_query() {
        let at: Timestamp = "2026-11-10T14:00:00.250Z".parse().unwrap();
        let seconds = at.as_second() + 1;
        // A split's own `OR` stays inside its parentheses, so it can't reach past the cutoff.
        assert_eq!(
            before_query(
                Some("-in:DRAFT in:inbox {from:\"a.com\"} OR subject:x"),
                None,
                at
            )
            .unwrap(),
            format!("(-in:DRAFT in:inbox {{from:\"a.com\"}} OR subject:x) before:{seconds}")
        );
        assert_eq!(
            before_query(Some("-in:INBOX"), Some(Filter::Unread), at).unwrap(),
            format!("(-in:INBOX) is:unread before:{seconds}")
        );
        // A label list is asked for by id, with nothing in `q`.
        assert_eq!(
            before_query(None, Some(Filter::Starred), at).unwrap(),
            format!("is:starred before:{seconds}")
        );
        assert!(matches!(
            before_query(Some("in:inbox"), Some(Filter::Important), at),
            Err(Error::Unsearchable(_))
        ));
        assert!(matches!(
            before_query(Some("in:inbox"), Some(Filter::NoReply), at),
            Err(Error::Unsearchable(_))
        ));
        let after_sec = at.as_second();
        assert_eq!(
            after_query(Some("-in:INBOX OR x"), at),
            format!("(-in:INBOX OR x) after:{after_sec}")
        );
        assert_eq!(after_query(None, at), format!("after:{after_sec}"));
    }

    #[tokio::test]
    async fn search_listed_by_filters_out_stored_threads_newer_than_cutoff() {
        let dir =
            std::env::temp_dir().join(format!("exo-store-bulk-listed-by-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let store = Store::open(
            &dir,
            "maya@brightmoor.co",
            gmail::Endpoints::google(),
            gmail::StaticToken("unused".into()),
        )
        .await
        .expect("opens");
        let mut db = store.inner.db.clone();
        let insert = |id: &str, thread_id: &str, ms: i64| gmail::Message {
            id: Some(id.into()),
            thread_id: Some(thread_id.into()),
            internal_date: Some(ms),
            label_ids: vec!["INBOX".into()],
            payload: Some(gmail::MessagePart {
                mime_type: Some("text/plain".into()),
                headers: vec![gmail::MessagePartHeader {
                    name: "From".into(),
                    value: "Tom <tom@brightmoor.co>".into(),
                }],
                ..Default::default()
            }),
            ..Default::default()
        };
        {
            let mut write = store.inner.begin(&mut db).await.expect("tx");
            for (m, t, ms) in [
                ("m1", "t_before", 1_794_319_200_250),
                ("m2", "t_subsec_after", 1_794_319_200_750),
            ] {
                let parsed = crate::parse::parse(&insert(m, t, ms), false).expect("parses");
                crate::provider::save_message(write.exec(), parsed)
                    .await
                    .expect("saves");
                crate::store::reindex(write.exec(), t, &store.inner.router())
                    .await
                    .expect("indexes");
            }
            write.commit().await.expect("commits");
        }

        let cutoff = ListCutoff {
            scope: Scope::Search("from:tom@brightmoor.co".into()),
            filter: None,
            keep: Vec::new(),
            before: Timestamp::from_millisecond(1_794_319_200_250).unwrap(),
        };
        let listed = store
            .listed_by(
                &cutoff,
                &["t_before".to_owned(), "t_subsec_after".to_owned()],
                &mut db,
            )
            .await
            .expect("listed");
        assert_eq!(listed, HashSet::from(["t_before".to_owned()]));

        drop(store);
        let _ = std::fs::remove_dir_all(&dir);
    }
}

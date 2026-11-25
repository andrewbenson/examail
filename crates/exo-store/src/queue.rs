//! Shared helpers for the store's persistent retry queues: flush serialization, per-queue FIFO
//! head selection with wake-time calculation, and standard [`Api`] failure disposition.

use std::collections::HashSet;
use std::hash::Hash;

use jiff::Timestamp;
use tokio::sync::{Mutex, MutexGuard};

use crate::api::{Api, Failure};
use crate::error::{Error, Result};

/// Serializes flushes of a persistent queue so concurrent callers wait their turn rather than
/// sending the same item twice or racing read-modify-writes.
#[derive(Default)]
pub(crate) struct FlushGate {
    lock: Mutex<()>,
}

impl FlushGate {
    pub async fn lock(&self) -> MutexGuard<'_, ()> {
        self.lock.lock().await
    }
}

/// The earlier of `current` and `at`.
#[must_use]
pub(crate) fn earliest(current: Option<Timestamp>, at: Timestamp) -> Option<Timestamp> {
    Some(current.map_or(at, |current| current.min(at)))
}

/// Result of selecting FIFO queue heads from a persistent queue snapshot.
#[derive(Debug, PartialEq, Eq)]
pub(crate) struct DueBatch<T> {
    /// Each queue's first item that isn't waiting out a future `not_before`.
    pub due: Vec<T>,
    /// Earliest future `not_before` among queue heads still waiting.
    pub retry_at: Option<Timestamp>,
}

/// Takes the first item of each queue from `rows` (in insertion order), partitioning those heads
/// into items due at `now` and the earliest wake timestamp among heads still waiting. When
/// `ignore_holds` is set, every queue's head is due immediately.
pub(crate) fn select_due<T, K>(
    rows: impl IntoIterator<Item = T>,
    now: Timestamp,
    ignore_holds: bool,
    mut queue_key: impl FnMut(&T) -> K,
    mut not_before: impl FnMut(&T) -> Option<Timestamp>,
) -> DueBatch<T>
where
    K: Eq + Hash,
{
    let mut seen = HashSet::new();
    let mut due = Vec::new();
    let mut retry_at = None;
    for row in rows {
        if !seen.insert(queue_key(&row)) {
            continue;
        }
        match not_before(&row).filter(|at| *at > now && !ignore_holds) {
            Some(at) => retry_at = earliest(retry_at, at),
            None => due.push(row),
        }
    }
    DueBatch { due, retry_at }
}

/// How a persistent queue disposes of an API failure.
#[derive(Debug)]
pub(crate) enum Disposition<E> {
    /// Retry at this timestamp (from a server throttle hint or `api.postpone(failures)`).
    Postponed(Timestamp),
    /// Permanent refusal (4xx); retrying won't help.
    Refused(E),
}

/// Classifies an [`Api`] [`Failure`]: rate-limit hints use their `until` timestamp, offline, auth
/// and transient failures back off via `api.postpone(failures)`, and permanent 4xx failures are
/// refused.
pub(crate) fn dispose_failure(
    api: &Api,
    failure: Failure,
    failures: u32,
) -> Disposition<gmail::Error> {
    match failure {
        Failure::Throttled(until) | Failure::SendingLimited(until) => Disposition::Postponed(until),
        Failure::Offline(_) | Failure::Unauthorized(_) | Failure::Transient(_) => {
            Disposition::Postponed(api.postpone(failures))
        }
        Failure::Permanent(error) => Disposition::Refused(error),
    }
}

/// Classifies a store [`Error`] produced by an [`Api`] call: `Throttled { until }`, `Offline`,
/// `Unauthorized` and transient 5xx responses postpone; permanent `Error::Gmail` responses refuse;
/// and local/domain errors are returned in `Err` for the caller to handle.
pub(crate) fn dispose_error(
    api: &Api,
    error: Error,
    failures: u32,
) -> Result<Disposition<gmail::Error>> {
    match error {
        Error::Throttled { until } => Ok(Disposition::Postponed(until)),
        Error::Offline(_) | Error::Unauthorized(_) => {
            Ok(Disposition::Postponed(api.postpone(failures)))
        }
        Error::Gmail(error) if matches!(error.status(), Some(500..=599)) => {
            Ok(Disposition::Postponed(api.postpone(failures)))
        }
        Error::Gmail(error) => Ok(Disposition::Refused(error)),
        other => Err(other),
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::time::Duration;

    use jiff::SignedDuration;

    use super::*;
    use crate::connection::Debounce;

    #[derive(Debug, Clone, PartialEq, Eq)]
    struct Item {
        id: u64,
        queue: &'static str,
        not_before: Option<Timestamp>,
    }

    fn api_for_test(base: Duration) -> (Api, std::path::PathBuf) {
        let dir = std::env::temp_dir().join(format!("exo-store-queue-{}", std::process::id()));
        let _ = std::fs::create_dir_all(&dir);
        let client = gmail::Client::new(
            gmail::Endpoints::google(),
            gmail::StaticToken("unused".into()),
        );
        (Api::new(client, base, None, Debounce::default(), &dir), dir)
    }

    fn api_error(code: u16, message: &str) -> gmail::Error {
        gmail::Error::Api(gmail::ApiError {
            code,
            message: message.into(),
            errors: Vec::new(),
            status: None,
            details: Vec::new(),
        })
    }

    #[test]
    fn select_due_keeps_fifo_heads_and_earliest_wake_time() {
        let now = Timestamp::from_second(1_000).expect("valid timestamp");
        let past = now - SignedDuration::from_secs(5);
        let soon = now + SignedDuration::from_secs(10);
        let later = now + SignedDuration::from_secs(30);

        let items = vec![
            // Queue A: head is due now; tail has an earlier future time that must NOT be picked.
            Item {
                id: 1,
                queue: "a",
                not_before: Some(past),
            },
            Item {
                id: 2,
                queue: "a",
                not_before: Some(now + SignedDuration::from_secs(2)),
            },
            // Queue B: head is postponed until `later`; tail is ready now but blocked behind head.
            Item {
                id: 3,
                queue: "b",
                not_before: Some(later),
            },
            Item {
                id: 4,
                queue: "b",
                not_before: None,
            },
            // Queue C: head is postponed until `soon` (becomes `retry_at`).
            Item {
                id: 5,
                queue: "c",
                not_before: Some(soon),
            },
            // Queue D: head has no hold.
            Item {
                id: 6,
                queue: "d",
                not_before: None,
            },
        ];

        let normal = select_due(
            items.clone(),
            now,
            false,
            |item| item.queue,
            |item| item.not_before,
        );
        let due_ids: Vec<u64> = normal.due.iter().map(|item| item.id).collect();
        assert_eq!(due_ids, vec![1, 6]);
        assert_eq!(normal.retry_at, Some(soon));

        let forced = select_due(items, now, true, |item| item.queue, |item| item.not_before);
        let forced_ids: Vec<u64> = forced.due.iter().map(|item| item.id).collect();
        assert_eq!(forced_ids, vec![1, 3, 5, 6]);
        assert_eq!(forced.retry_at, None);
    }

    #[test]
    fn dispose_honors_throttle_hints_backs_off_transients_and_refuses_4xx() {
        let (api, dir) = api_for_test(Duration::from_secs(2));
        let throttle_until = Timestamp::now() + SignedDuration::from_secs(90);

        match dispose_failure(&api, Failure::Throttled(throttle_until), 1) {
            Disposition::Postponed(until) => assert_eq!(until, throttle_until),
            Disposition::Refused(err) => panic!("unexpected refusal: {err}"),
        }

        let before = Timestamp::now();
        match dispose_failure(&api, Failure::Transient(api_error(503, "unavailable")), 2) {
            Disposition::Postponed(until) => {
                let wait = until.duration_since(before);
                assert!(
                    wait >= SignedDuration::from_secs(7) && wait <= SignedDuration::from_secs(10),
                    "expected ~8s backoff for base=2s, failures=2, got {wait:?}"
                );
            }
            Disposition::Refused(err) => panic!("unexpected refusal: {err}"),
        }

        match dispose_failure(&api, Failure::Permanent(api_error(400, "bad request")), 1) {
            Disposition::Refused(err) => assert_eq!(err.status(), Some(400)),
            Disposition::Postponed(until) => panic!("unexpected postpone until {until}"),
        }

        match dispose_error(
            &api,
            Error::Throttled {
                until: throttle_until,
            },
            1,
        )
        .expect("API error")
        {
            Disposition::Postponed(until) => assert_eq!(until, throttle_until),
            Disposition::Refused(err) => panic!("unexpected refusal: {err}"),
        }

        match dispose_error(&api, Error::Offline(api_error(503, "down")), 1).expect("API error") {
            Disposition::Postponed(until) => assert!(until > before),
            Disposition::Refused(err) => panic!("unexpected refusal: {err}"),
        }

        match dispose_error(&api, Error::Gmail(api_error(502, "bad gateway")), 1)
            .expect("API error")
        {
            Disposition::Postponed(until) => assert!(until > before),
            Disposition::Refused(err) => panic!("unexpected refusal: {err}"),
        }

        match dispose_error(&api, Error::Gmail(api_error(404, "gone")), 1).expect("API error") {
            Disposition::Refused(err) => assert_eq!(err.status(), Some(404)),
            Disposition::Postponed(until) => panic!("unexpected postpone until {until}"),
        }

        let domain = dispose_error(&api, Error::EventCancelled, 1).expect_err("domain error");
        assert!(matches!(domain, Error::EventCancelled));

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn flush_gate_serializes_concurrent_callers() {
        let gate = Arc::new(FlushGate::default());
        let active = Arc::new(AtomicUsize::new(0));
        let max_active = Arc::new(AtomicUsize::new(0));

        let mut tasks = Vec::new();
        for _ in 0..4 {
            let gate = Arc::clone(&gate);
            let active = Arc::clone(&active);
            let max_active = Arc::clone(&max_active);
            tasks.push(tokio::spawn(async move {
                let _guard = gate.lock().await;
                let now_active = active.fetch_add(1, Ordering::SeqCst) + 1;
                max_active.fetch_max(now_active, Ordering::SeqCst);
                tokio::time::sleep(Duration::from_millis(10)).await;
                active.fetch_sub(1, Ordering::SeqCst);
            }));
        }
        for task in tasks {
            task.await.expect("joins");
        }
        assert_eq!(max_active.load(Ordering::SeqCst), 1);
    }
}

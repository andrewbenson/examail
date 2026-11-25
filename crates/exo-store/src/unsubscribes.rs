//! One-click unsubscribes (RFC 8058) waiting to be posted. The app posts them itself and keeps
//! them here until it has, so an unsubscribe asked for offline, or just before a quit, still
//! goes. The queue is a JSON list in `kv`, so it needs no schema change.

use std::future::Future;

use jiff::Timestamp;
use serde::{Deserialize, Serialize};

use crate::db;
use crate::error::Result;
use crate::queue;
use crate::store::Store;

/// Where the queue is kept in `kv`.
pub(crate) const KEY: &str = "unsubscribes";

/// An unsubscribe waiting to be posted.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PendingUnsubscribe {
    /// The link to post to.
    pub url: String,
    /// When it was asked for.
    pub queued: Timestamp,
    /// Attempts that failed for a reason trying again could fix.
    pub failures: u32,
    /// When to try again, after a failure.
    pub not_before: Option<Timestamp>,
}

impl Store {
    /// Queues a one-click unsubscribe from `url`, unless it's already waiting.
    ///
    /// # Errors
    /// On database failures.
    pub async fn queue_unsubscribe(&self, url: &str) -> Result<()> {
        self.change_unsubscribes(|queue| {
            if !queue.iter().any(|pending| pending.url == url) {
                queue.push(PendingUnsubscribe {
                    url: url.to_owned(),
                    queued: Timestamp::now(),
                    failures: 0,
                    not_before: None,
                });
            }
        })
        .await
    }

    /// The unsubscribes waiting, oldest first.
    ///
    /// # Errors
    /// On database failures.
    pub async fn pending_unsubscribes(&self) -> Result<Vec<PendingUnsubscribe>> {
        let mut db = self.inner.db.clone();
        Ok(db::kv_get(&mut db, KEY).await?.unwrap_or_default())
    }

    /// Waiting unsubscribes that are due now (one head per URL), along with the earliest wake
    /// timestamp among those still backing off.
    ///
    /// # Errors
    /// On database failures.
    pub async fn due_unsubscribes(&self) -> Result<(Vec<PendingUnsubscribe>, Option<Timestamp>)> {
        let batch = queue::select_due(
            self.pending_unsubscribes().await?,
            Timestamp::now(),
            false,
            |pending| pending.url.clone(),
            |pending| pending.not_before,
        );
        Ok((batch.due, batch.retry_at))
    }

    /// Runs `send` on every due unsubscribe while holding the store's unsubscribe flush lock, so
    /// concurrent flushes cannot double-post or perform racy read-modify-writes on `kv`.
    /// Returning `None` from `send` finishes and removes the item; returning `Some(until)`
    /// increments `failures` and postpones it until `until`. Returns the earliest next wake time.
    ///
    /// # Errors
    /// On database failures.
    pub async fn flush_unsubscribes<F, Fut>(&self, mut send: F) -> Result<Option<Timestamp>>
    where
        F: FnMut(PendingUnsubscribe) -> Fut,
        Fut: Future<Output = Option<Timestamp>>,
    {
        let _flush = self.inner.unsubscribe_flush.lock().await;
        let (due, mut retry_at) = self.due_unsubscribes().await?;
        for pending in due {
            let url = pending.url.clone();
            match send(pending).await {
                None => self.finish_unsubscribe(&url).await?,
                Some(until) => {
                    retry_at = queue::earliest(retry_at, until);
                    self.postpone_unsubscribe(&url, until).await?;
                }
            }
        }
        Ok(retry_at)
    }

    /// Takes `url` off the queue once it's posted or given up on.
    ///
    /// # Errors
    /// On database failures.
    pub async fn finish_unsubscribe(&self, url: &str) -> Result<()> {
        self.change_unsubscribes(|queue| queue.retain(|pending| pending.url != url))
            .await
    }

    /// Counts a failed attempt at `url`, to be tried again at `until`.
    ///
    /// # Errors
    /// On database failures.
    pub async fn postpone_unsubscribe(&self, url: &str, until: Timestamp) -> Result<()> {
        self.change_unsubscribes(|queue| {
            if let Some(pending) = queue.iter_mut().find(|pending| pending.url == url) {
                pending.failures += 1;
                pending.not_before = Some(until);
            }
        })
        .await
    }

    /// Reads, changes and writes the queue in one transaction, so a post finishing and a new
    /// unsubscribe can't lose each other's change.
    async fn change_unsubscribes(
        &self,
        change: impl FnOnce(&mut Vec<PendingUnsubscribe>),
    ) -> Result<()> {
        let mut db = self.inner.db.clone();
        let mut write = self.inner.begin(&mut db).await?;
        let mut queue: Vec<PendingUnsubscribe> =
            db::kv_get(write.exec(), KEY).await?.unwrap_or_default();
        change(&mut queue);
        db::kv_set(write.exec(), KEY, &queue).await?;
        write.commit().await
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::time::Duration;

    use jiff::SignedDuration;

    use super::*;

    #[tokio::test]
    async fn concurrent_flushes_serialize_on_kv_and_preserve_new_items() {
        let dir = std::env::temp_dir().join(format!(
            "exo-store-unsubscribes-flush-{}",
            std::process::id()
        ));
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
            .queue_unsubscribe("https://news.example/one")
            .await
            .expect("queues one");
        store
            .queue_unsubscribe("https://news.example/two")
            .await
            .expect("queues two");

        let attempts = Arc::new(AtomicUsize::new(0));
        let retry_two = Timestamp::now() + SignedDuration::from_secs(60);

        let s1 = store.clone();
        let a1 = Arc::clone(&attempts);
        let first = tokio::spawn(async move {
            s1.flush_unsubscribes(|pending| {
                let a1 = Arc::clone(&a1);
                async move {
                    a1.fetch_add(1, Ordering::SeqCst);
                    tokio::time::sleep(Duration::from_millis(20)).await;
                    if pending.url.ends_with("/one") {
                        None
                    } else {
                        Some(retry_two)
                    }
                }
            })
            .await
            .expect("first flush")
        });

        let s2 = store.clone();
        let a2 = Arc::clone(&attempts);
        let second = tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(5)).await;
            s2.flush_unsubscribes(|_| {
                let a2 = Arc::clone(&a2);
                async move {
                    a2.fetch_add(1, Ordering::SeqCst);
                    None
                }
            })
            .await
            .expect("second flush")
        });

        // Queuing a fresh URL while the first flush is in flight keeps it intact.
        tokio::time::sleep(Duration::from_millis(5)).await;
        store
            .queue_unsubscribe("https://news.example/three")
            .await
            .expect("queues three");

        let r1 = first.await.expect("joins first");
        let r2 = second.await.expect("joins second");
        assert_eq!(r1, Some(retry_two));
        assert_eq!(r2, Some(retry_two));

        // `/one` ran once and finished, `/two` ran once and backed off (second flush saw its
        // `not_before > now` and did not re-send or double-increment `failures`), and `/three`
        // arrived before the second flush started so the second flush finished it.
        assert_eq!(attempts.load(Ordering::SeqCst), 3);
        let remaining = store.pending_unsubscribes().await.expect("reads");
        assert_eq!(remaining.len(), 1);
        assert_eq!(remaining[0].url, "https://news.example/two");
        assert_eq!(remaining[0].failures, 1);
        assert_eq!(remaining[0].not_before, Some(retry_two));

        drop(store);
        let _ = std::fs::remove_dir_all(&dir);
    }
}

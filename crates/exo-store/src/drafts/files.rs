//! Draft attachment storage, orphan file pruning, and upload progress tracking.

use std::collections::HashSet;
use std::path::Path;
use std::sync::Arc;
use std::time::{Duration, SystemTime};

use super::model::{AttachmentSource, DraftRecord, Upload, Uploads};
use crate::db::DraftRow;
use crate::error::Result;
use crate::store::{Inner, Store};

/// The ids of `record`'s files that go up to Gmail ([`AttachmentSource::Local`]).
pub(super) fn local_files(record: &DraftRecord) -> HashSet<String> {
    record
        .attachments
        .iter()
        .filter(|attachment| attachment.source == AttachmentSource::Local)
        .map(|attachment| attachment.uuid.clone())
        .collect()
}

impl Store {
    /// The folder draft attachments' bytes are kept in, each named by its `uuid`.
    pub fn draft_files(&self) -> &Path {
        &self.inner.files
    }

    /// How each draft's files stand with Gmail, as it changes: which of them Gmail's copy has,
    /// and how far a write carrying the others has come. The first time the store sees a draft
    /// this session, Gmail's copy has all its files if Gmail has the draft's latest version.
    /// After that, each write says which files it has. A draft the store doesn't know of yet has
    /// none up.
    #[must_use]
    pub fn uploads(&self) -> tokio::sync::watch::Receiver<Uploads> {
        self.inner.uploads.subscribe()
    }

    /// Deletes kept attachment bytes no draft or snippet refers to any more.
    ///
    /// # Errors
    /// On database failures. Files that can't be listed or removed are skipped.
    pub async fn prune_draft_files(&self) -> Result<()> {
        self.prune_draft_files_older_than(Duration::ZERO).await
    }

    /// Deletes kept attachment bytes no draft or snippet refers to any more, leaving files
    /// modified within `grace` so newly attached or pasted files in an unsaved draft or snippet
    /// editor stay intact during background startup pruning.
    ///
    /// # Errors
    /// On database failures. Files that can't be listed or removed are skipped.
    pub async fn prune_draft_files_older_than(&self, grace: Duration) -> Result<()> {
        let mut db = self.inner.db.clone();
        let mut kept: HashSet<String> = DraftRow::all()
            .select(DraftRow::fields().record())
            .exec(&mut db)
            .await?
            .into_iter()
            .flat_map(|record| {
                record
                    .0
                    .attachments
                    .into_iter()
                    .map(|attachment| attachment.uuid)
            })
            .collect();
        let settings: crate::Settings = self.settings().await?;
        kept.extend(
            settings
                .snippets
                .into_iter()
                .flat_map(|snippet| snippet.attachments)
                .map(|attachment| attachment.uuid),
        );
        let Ok(entries) = std::fs::read_dir(&self.inner.files) else {
            return Ok(());
        };
        let cutoff = (!grace.is_zero())
            .then(|| SystemTime::now().checked_sub(grace))
            .flatten();
        for entry in entries.flatten() {
            if entry
                .file_name()
                .to_str()
                .is_some_and(|name| kept.contains(name))
            {
                continue;
            }
            if cutoff.is_some_and(|cutoff| {
                entry
                    .metadata()
                    .and_then(|meta| meta.modified())
                    .is_ok_and(|modified| modified > cutoff)
            }) {
                continue;
            }
            if let Err(error) = std::fs::remove_file(entry.path()) {
                log::warn!("removing a draft's file: {error}");
            }
        }
        Ok(())
    }
}

impl Inner {
    /// Notes which of `row`'s files Gmail's copy has, the first time the store sees the draft
    /// this session. Gmail's copy has all of them once Gmail has the latest revision, or else none
    /// that the store knows of. After that, only writes say which files it has.
    pub(super) fn note_draft(&self, row: &DraftRow) {
        self.note_draft_meta(&row.id, row.pushed, row.revision, &row.record.0);
    }

    /// [`note_draft`](Self::note_draft) from metadata columns alone, without loading `raw`.
    pub(super) fn note_draft_meta(
        &self,
        id: &str,
        pushed: u64,
        revision: u64,
        record: &DraftRecord,
    ) {
        self.uploads.send_if_modified(|uploads| {
            if uploads.contains_key(id) {
                return false;
            }
            let gmail_has = if pushed >= revision {
                local_files(record)
            } else {
                HashSet::new()
            };
            uploads.insert(
                id.to_owned(),
                Upload {
                    gmail_has,
                    sending: None,
                },
            );
            true
        });
    }

    /// Gmail's copy of draft `id` now has `files`, and no write of it is under way.
    pub(super) fn set_gmail_files(&self, id: &str, files: HashSet<String>) {
        self.uploads.send_if_modified(|uploads| {
            let upload = Upload {
                gmail_has: files,
                sending: None,
            };
            let changed = uploads.get(id) != Some(&upload);
            uploads.insert(id.to_owned(), upload);
            changed
        });
    }

    /// Whether draft `id` has a file that Gmail's copy lacks. Such a draft goes up at once, so a
    /// file's upload starts as it's added.
    pub(super) fn uploads_files_meta(&self, id: &str, record: &DraftRecord) -> bool {
        let uploads = self.uploads.borrow();
        let gmail_has = uploads.get(id).map(|upload| &upload.gmail_has);
        local_files(record)
            .iter()
            .any(|file| !gmail_has.is_some_and(|has| has.contains(file)))
    }

    /// Reports to `row`'s [`Upload`] how much of a write's message has been sent so far, for the
    /// files the write carries that Gmail lacks. `None` when it carries none.
    pub(super) fn upload_progress(&self, row: &DraftRow) -> Option<gmail::Progress> {
        let id = row.id.clone();
        let mut carried = local_files(&row.record.0);
        let uploads = self.uploads.clone();
        let started = uploads.send_if_modified(|uploads| {
            let upload = uploads.entry(id.clone()).or_default();
            carried.retain(|file| !upload.gmail_has.contains(file));
            let starts = !carried.is_empty();
            if starts {
                upload.sending = Some((carried, 0.0));
            }
            starts
        });
        started.then(|| -> gmail::Progress {
            Arc::new(move |transfer: gmail::Transfer| {
                let percent = transfer
                    .total
                    .filter(|total| *total > 0)
                    .map_or(0.0, |total| {
                        (transfer.done as f64 / total as f64 * 100.0) as f32
                    });
                uploads.send_if_modified(|uploads| {
                    match uploads
                        .get_mut(&id)
                        .and_then(|upload| upload.sending.as_mut())
                    {
                        // A retried call starts again from 0, but the bar doesn't go back.
                        Some((_, shown)) if percent > *shown => {
                            *shown = percent;
                            true
                        }
                        _ => false,
                    }
                });
            })
        })
    }

    /// A write of draft `id` ended without Gmail taking it, so no upload is under way.
    pub(super) fn upload_ended(&self, id: &str) {
        self.uploads.send_if_modified(|uploads| {
            uploads
                .get_mut(id)
                .and_then(|upload| upload.sending.take())
                .is_some()
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::drafts::{Address, Draft, DraftAction, DraftAttachment};

    #[tokio::test]
    async fn prune_draft_files_spares_files_modified_within_the_last_hour() {
        let dir = std::env::temp_dir().join(format!(
            "exo-store-prune-draft-files-{}",
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
        std::fs::create_dir_all(&store.inner.files).expect("creates files dir");
        let fresh = store.inner.files.join("fresh-uuid");
        let stale = store.inner.files.join("stale-uuid");
        let kept_stale = store.inner.files.join("kept-stale-uuid");
        std::fs::write(&fresh, b"new").expect("writes fresh");
        std::fs::write(&stale, b"old").expect("writes stale");
        std::fs::write(&kept_stale, b"kept").expect("writes kept");

        let two_hours_ago = SystemTime::now()
            .checked_sub(Duration::from_secs(7200))
            .expect("valid time");
        for path in [&stale, &kept_stale] {
            std::fs::OpenOptions::new()
                .write(true)
                .open(path)
                .expect("opens file")
                .set_modified(two_hours_ago)
                .expect("sets mtime");
        }

        store
            .save_draft(Draft {
                id: "draft001".into(),
                thread: None,
                adopted: None,
                record: DraftRecord {
                    action: DraftAction::Compose,
                    from: Some(Address::new("Maya", "maya@brightmoor.co")),
                    to: Vec::new(),
                    cc: Vec::new(),
                    bcc: Vec::new(),
                    subject: String::new(),
                    body: String::new(),
                    quoted_content: None,
                    quoted_inlined: false,
                    in_reply_to: None,
                    in_reply_to_rfc822: None,
                    references: Vec::new(),
                    rfc822_id: "d1@exo".into(),
                    fingerprint: String::new(),
                    attachments: vec![DraftAttachment {
                        uuid: "kept-stale-uuid".into(),
                        name: "report.pdf".into(),
                        mime_type: "application/pdf".into(),
                        size: 4,
                        content_id: None,
                        source: AttachmentSource::Local,
                    }],
                    send_later: None,
                    reminder: None,
                },
                raw: String::new(),
                complete: true,
                sending: false,
            })
            .await
            .expect("saves draft");

        store
            .prune_draft_files_older_than(Duration::from_secs(3600))
            .await
            .expect("prunes");
        assert!(
            fresh.exists(),
            "recent unreferenced file stays within grace window"
        );
        assert!(!stale.exists(), "old unreferenced file is pruned");
        assert!(kept_stale.exists(), "referenced old file stays");

        drop(store);
        let _ = std::fs::remove_dir_all(&dir);
    }
}

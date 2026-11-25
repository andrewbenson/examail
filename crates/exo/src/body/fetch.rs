//! Loads what message bodies ask for. Remote images come straight from the sender's server, with
//! no proxy, and only from the public internet (`crate::remote_content`). `cid:` images come from
//! the message's inline attachments. Both go through the account's [`Loader`].

use std::{
    collections::HashMap,
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
        mpsc::Sender,
    },
};

use exo_store::Attachment;

use super::Job;
use crate::account::Loader;

/// The [`exo_mail::Fetch`] for one thread, which knows the thread's inline attachments by content
/// id.
pub(super) struct Fetcher {
    loader: Loader,
    /// Inline attachments by lower-cased `Content-ID`.
    inline: HashMap<String, Attachment>,
    /// Wakes the body worker when bytes arrive. Each wake adds one to `pending`.
    wake: Sender<Job>,
    pending: Arc<AtomicUsize>,
}

impl Fetcher {
    pub(super) fn new(
        loader: Loader,
        attachments: Vec<Attachment>,
        wake: Sender<Job>,
        pending: Arc<AtomicUsize>,
    ) -> Self {
        Self {
            loader,
            inline: by_content_id(attachments),
            wake,
            pending,
        }
    }
}

/// The `attachments` that have a `Content-ID`, keyed by that id in lower case.
pub(crate) fn by_content_id(attachments: Vec<Attachment>) -> HashMap<String, Attachment> {
    attachments
        .into_iter()
        .filter_map(|attachment| Some((attachment.content_id.clone()?.to_lowercase(), attachment)))
        .collect()
}

/// The attachment in `inline` (from [`by_content_id`]) that the `cid` of a `cid:` URL names. The
/// `cid` may be percent-encoded.
pub(crate) fn named<'a>(
    inline: &'a HashMap<String, Attachment>,
    cid: &str,
) -> Option<&'a Attachment> {
    let cid = cid.to_lowercase();
    inline
        .get(&cid)
        .or_else(|| inline.get(&exo_mail::percent_decode_lossy(&cid)))
}

impl exo_mail::Fetch for Fetcher {
    fn fetch(&self, url: &str, done: exo_mail::Done) {
        let (wake, pending) = (self.wake.clone(), self.pending.clone());
        let finish = move |bytes: Option<Vec<u8>>| {
            if let Some(bytes) = bytes {
                done.finish(bytes);
                pending.fetch_add(1, Ordering::SeqCst);
                if wake.send(Job::Wake).is_err() {
                    pending.fetch_sub(1, Ordering::SeqCst);
                }
            }
        };
        if let Some(cid) = url.strip_prefix("cid:") {
            match named(&self.inline, cid) {
                Some(attachment) => self
                    .loader
                    .inline_image(attachment, |bytes| finish(bytes.ok())),
                None => log::debug!("no inline attachment for a `cid:` link"),
            }
        } else if url.starts_with("https://") || url.starts_with("http://") {
            self.loader.url(url, finish);
        }
    }
}

#[cfg(test)]
mod tests {
    use exo_mail::percent_decode_lossy;

    #[test]
    fn decodes_percent_escapes_in_content_ids() {
        assert_eq!(
            percent_decode_lossy("image001.png%4001d9"),
            "image001.png@01d9"
        );
        assert_eq!(percent_decode_lossy("plain@cid"), "plain@cid");
    }
}

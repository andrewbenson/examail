//! The Snippets folder's numbers, counted from the messages Exo sends with a snippet. Each
//! message counts a send and records when the last one went, and it counts a reply once someone
//! else writes in its conversation after it. Opens need read statuses, which Exo hasn't got.

use exo_store::{Snippet, SnippetAnalytics, SnippetSend, Store, ThreadDetail, ThreadId};

/// Maximum unreplied sends tracked per snippet so settings and reply checks stay bounded.
const MAX_UNREPLIED: usize = 200;

/// Sends by `(snippet_id, send)`.
type Found = Vec<(String, SnippetSend)>;

/// What a store check found for the sends still waiting on a reply.
#[derive(Debug, Default, PartialEq, Eq)]
struct CheckOutcome {
    /// Sends that now have a reply from someone else.
    replied: Found,
    /// Sends whose conversation is no longer stored (deleted or evicted).
    pruned: Found,
}

impl CheckOutcome {
    fn is_empty(&self) -> bool {
        self.replied.is_empty() && self.pruned.is_empty()
    }
}

/// Tracks whether a snippet reply check is already running.
#[derive(Default)]
pub(crate) struct Replies {
    checking: bool,
}

/// Whether `thread` has a reply to its message `message`: a later message, not a draft, from
/// someone else.
fn replied(thread: &ThreadDetail, message: &str) -> bool {
    let Some(at) = thread.messages.iter().position(|kept| kept.id == message) else {
        return false;
    };
    thread.messages[at + 1..]
        .iter()
        .any(|later| !later.is_draft() && later.from.as_ref().is_some_and(|from| !from.is_me))
}

/// `round(part / sends * 100)%`, `0%` with no sends.
pub(super) fn percentage(part: u32, sends: u32) -> String {
    let percent = if sends == 0 {
        0.0
    } else {
        (f64::from(part) / f64::from(sends) * 100.0).round()
    };
    format!("{percent}%")
}

/// Records one outgoing send in `analytics`, keeping at most [`MAX_UNREPLIED`] newest unreplied
/// sends.
fn record_send(analytics: &mut SnippetAnalytics, at: &str, send: SnippetSend) {
    analytics.sends += 1;
    analytics.last_sent = at.to_owned();
    analytics.unreplied.push(send);
    let excess = analytics.unreplied.len().saturating_sub(MAX_UNREPLIED);
    if excess > 0 {
        analytics.unreplied.drain(..excess);
    }
}

/// Checks `waiting` against `store`, finding replied sends and pruning threads no longer stored.
async fn check_waiting(store: &Store, waiting: Found) -> CheckOutcome {
    let mut outcome = CheckOutcome::default();
    let mut threads: Vec<&str> = waiting
        .iter()
        .map(|(_, send)| send.thread.as_str())
        .collect();
    threads.sort_unstable();
    threads.dedup();
    for thread in threads {
        let detail = match store.thread(&ThreadId::new(thread.to_owned())).await {
            Ok(Some(detail)) => detail,
            Ok(None) => {
                outcome.pruned.extend(
                    waiting
                        .iter()
                        .filter(|(_, send)| send.thread == thread)
                        .cloned(),
                );
                continue;
            }
            Err(error) => {
                log::info!("checking snippet replies in {thread}: {error}");
                continue;
            }
        };
        outcome.replied.extend(
            waiting
                .iter()
                .filter(|(_, send)| send.thread == thread && replied(&detail, &send.message))
                .cloned(),
        );
    }
    outcome
}

/// Applies `outcome` to `snippets`: replied sends increment `replies`, and evicted threads are
/// dropped without counting a reply.
fn apply_outcome(snippets: &mut [Snippet], outcome: &CheckOutcome) {
    for (id, send) in &outcome.replied {
        let Some(snippet) = snippets.iter_mut().find(|snippet| snippet.id == *id) else {
            continue;
        };
        let before = snippet.analytics.unreplied.len();
        snippet
            .analytics
            .unreplied
            .retain(|waiting| waiting != send);
        if snippet.analytics.unreplied.len() < before {
            snippet.analytics.replies += 1;
        }
    }
    for (id, send) in &outcome.pruned {
        if let Some(snippet) = snippets.iter_mut().find(|snippet| snippet.id == *id) {
            snippet
                .analytics
                .unreplied
                .retain(|waiting| waiting != send);
        }
    }
}

impl crate::Mail {
    /// A message that used the snippets `used` went out as `message` in `thread`. Each snippet
    /// counts a send, sent now, and the folder looks for replies to it.
    pub(crate) fn snippets_sent(&mut self, used: &[String], thread: &ThreadId, message: &str) {
        if used.is_empty() {
            return;
        }
        let at = jiff::Timestamp::now().to_string();
        self.account.update_settings(|settings| {
            for snippet in settings
                .snippets
                .iter_mut()
                .filter(|snippet| used.contains(&snippet.id))
            {
                record_send(
                    &mut snippet.analytics,
                    &at,
                    SnippetSend {
                        thread: thread.as_str().to_owned(),
                        message: message.to_owned(),
                    },
                );
            }
        });
    }

    /// On opening the folder, asks the store which sends still waiting for a reply have one now,
    /// and prunes sends whose conversation was deleted or evicted.
    pub(crate) fn check_snippet_replies(&mut self) {
        let waiting: Found = self
            .account
            .settings()
            .snippets
            .iter()
            .flat_map(|snippet| {
                snippet
                    .analytics
                    .unreplied
                    .iter()
                    .map(|send| (snippet.id.clone(), send.clone()))
            })
            .collect();
        if waiting.is_empty() || self.snippets.replies.checking {
            return;
        }
        self.snippets.replies.checking = true;
        let ui = self.ui_sender();
        self.account.loader().run(
            move |store| async move { check_waiting(&store, waiting).await },
            move |outcome| {
                ui.send(move |mail| {
                    mail.snippets.replies.checking = false;
                    if !outcome.is_empty() {
                        mail.account.update_settings(|settings| {
                            apply_outcome(&mut settings.snippets, &outcome);
                        });
                    }
                });
            },
        );
    }

    /// Drains any pending UI tasks so snippet reply updates apply before the folder draws.
    pub(super) fn snippet_replies_arrived(&mut self) {
        self.drain_ui_tasks();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use exo_store::{Contact, MessageDetail};

    fn message(id: &str, from_me: bool, draft: bool) -> MessageDetail {
        MessageDetail {
            id: id.into(),
            labels: if draft {
                vec!["DRAFT".into()]
            } else {
                Vec::new()
            },
            date: jiff::Timestamp::UNIX_EPOCH,
            from: Some(Contact {
                name: None,
                email: if from_me {
                    "maya@brightmoor.co"
                } else {
                    "tom@brightmoor.co"
                }
                .into(),
                is_me: from_me,
            }),
            to: Vec::new(),
            cc: Vec::new(),
            bcc: Vec::new(),
            reply_to: Vec::new(),
            subject: String::new(),
            snippet: String::new(),
            rfc822_id: None,
            in_reply_to: None,
            references: Vec::new(),
            list_id: None,
            list_unsubscribe: None,
            list_unsubscribe_post: None,
            headers: Vec::new(),
            body: exo_store::Body::default(),
            attachments: Vec::new(),
            failure: None,
            scheduled: None,
            sending: false,
        }
    }

    fn thread(messages: Vec<MessageDetail>) -> ThreadDetail {
        ThreadDetail {
            id: ThreadId::new("t".to_owned()),
            subject: String::new(),
            labels: Vec::new(),
            muted: false,
            reminder: None,
            messages,
        }
    }

    #[test]
    fn a_reply_is_someone_elses_later_message() {
        let earlier = thread(vec![
            message("theirs", false, false),
            message("sent", true, false),
        ]);
        assert!(!replied(&earlier, "sent"), "what came before doesn't count");
        let mine = thread(vec![
            message("sent", true, false),
            message("follow-up", true, false),
            message("draft", false, true),
        ]);
        assert!(!replied(&mine, "sent"), "nor my own follow-up, nor a draft");
        let answered = thread(vec![
            message("sent", true, false),
            message("reply", false, false),
        ]);
        assert!(replied(&answered, "sent"));
        assert!(
            !replied(&answered, "gone"),
            "a message the store doesn't have"
        );
    }

    #[test]
    fn percentages_round_to_whole_numbers() {
        assert_eq!(percentage(3, 12), "25%");
        assert_eq!(percentage(450, 1500), "30%");
        assert_eq!(percentage(1, 3), "33%");
        assert_eq!(percentage(0, 0), "0%", "no sends yet");
    }

    #[test]
    fn unreplied_tracking_caps_newest_entries_and_prunes_evicted_threads() {
        let mut analytics = SnippetAnalytics::default();
        for index in 0..250 {
            record_send(
                &mut analytics,
                "2026-11-23T12:00:00Z",
                SnippetSend {
                    thread: format!("t-{index}"),
                    message: format!("m-{index}"),
                },
            );
        }
        assert_eq!(analytics.sends, 250);
        assert_eq!(analytics.unreplied.len(), MAX_UNREPLIED);
        assert_eq!(
            analytics.unreplied.first().map(|send| send.thread.as_str()),
            Some("t-50"),
            "oldest entries drop once full"
        );
        assert_eq!(
            analytics.unreplied.last().map(|send| send.thread.as_str()),
            Some("t-249")
        );

        let dir = std::env::temp_dir().join(format!(
            "exo-snippet-analytics-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("runtime");
        let evicted = SnippetSend {
            thread: "t-evicted".into(),
            message: "m-evicted".into(),
        };
        let outcome = runtime.block_on(async {
            let store = Store::open(
                &dir,
                "maya@brightmoor.co",
                gmail::Endpoints::mock(&"http://127.0.0.1:9/".parse().expect("origin")),
                gmail::StaticToken("dev".into()),
            )
            .await
            .expect("store opens");
            check_waiting(&store, vec![("s1".into(), evicted.clone())]).await
        });
        let _ = std::fs::remove_dir_all(&dir);

        assert!(outcome.replied.is_empty());
        assert_eq!(outcome.pruned, vec![("s1".into(), evicted.clone())]);

        let mut snippets = vec![Snippet {
            id: "s1".into(),
            analytics: SnippetAnalytics {
                sends: 1,
                unreplied: vec![evicted],
                ..SnippetAnalytics::default()
            },
            ..Snippet::default()
        }];
        apply_outcome(&mut snippets, &outcome);
        assert!(snippets[0].analytics.unreplied.is_empty());
        assert_eq!(
            snippets[0].analytics.replies, 0,
            "evicted threads are pruned without counting a reply"
        );
    }
}

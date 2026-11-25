//! A thread's source as the raw source viewer shows it. This is Gmail's `threads.get` response
//! with the HTML entities in its snippets decoded, written the way
//! `JSON.stringify(thread, null, 4)` writes it.

use futures::future;
use serde::Serialize as _;
use serde_json::{Serializer, Value, ser::PrettyFormatter};

use crate::{
    error::Result, parse::unescape_html, quota::Lane, store::Store, sync::MESSAGES_AT_ONCE,
};

impl Store {
    /// Thread `id`'s source for the raw source viewer, from `threads.get` (format `full`, 10
    /// units). A thread too large for one answer is its `format=minimal` answer with each message
    /// in full from `messages.get`, as sync fetches it.
    ///
    /// # Errors
    /// [`crate::Error::Offline`] without a network; [`crate::Error::Gmail`] when Gmail refuses.
    pub async fn thread_source(&self, id: &str) -> Result<String> {
        let api = &self.inner.api;
        let get = |format| {
            api.call(Lane::Act, ATTEMPTS, move |gmail| {
                gmail.thread(id, format, &[]).returning::<Value>()
            })
        };
        let mut thread = match get(gmail::Format::Full).await {
            Err(failure) if failure.is_too_large() => {
                let mut thread = get(gmail::Format::Minimal).await?;
                let mut messages = Vec::new();
                for chunk in message_ids(&thread).chunks(MESSAGES_AT_ONCE) {
                    messages.extend(
                        future::try_join_all(chunk.iter().map(|message| {
                            api.call(Lane::Act, ATTEMPTS, |gmail| {
                                gmail
                                    .message(message, gmail::Format::Full, &[])
                                    .returning::<Value>()
                            })
                        }))
                        .await?,
                    );
                }
                if let Some(slot) = messages_mut(&mut thread) {
                    *slot = messages;
                }
                thread
            }
            fetched => fetched?,
        };
        normalize(&mut thread);
        Ok(stringify(&thread))
    }
}

/// Tries per call, since someone is waiting.
const ATTEMPTS: u32 = 3;

/// A thread's `messages`.
fn messages_mut(thread: &mut Value) -> Option<&mut Vec<Value>> {
    thread.get_mut("messages")?.as_array_mut()
}

/// The `id` of each of a thread's messages.
fn message_ids(thread: &Value) -> Vec<String> {
    thread
        .get("messages")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(|message| message.get("id")?.as_str().map(str::to_owned))
        .collect()
}

/// Decodes the HTML entities in each message's `snippet`.
fn normalize(thread: &mut Value) {
    let Some(messages) = messages_mut(thread) else {
        return;
    };
    for message in messages {
        if let Some(Value::String(snippet)) = message.get_mut("snippet") {
            *snippet = unescape_html(snippet);
        }
    }
}

/// `JSON.stringify(value, null, 4)` with member order preserved.
fn stringify(value: &Value) -> String {
    let mut buf = Vec::new();
    let mut serializer =
        Serializer::with_formatter(&mut buf, PrettyFormatter::with_indent(b"    "));
    value
        .serialize(&mut serializer)
        .expect("serializing JSON into memory doesn't fail");
    String::from_utf8(buf).expect("serde_json writes UTF-8")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn source(json: &str) -> String {
        let mut thread: Value = serde_json::from_str(json).expect("valid JSON");
        normalize(&mut thread);
        stringify(&thread)
    }

    #[test]
    fn writes_what_gmail_sent_in_its_order_four_spaces_deep() {
        let json = r#"{"id":"t1","historyId":"42","messages":[{"id":"m1","labelIds":["INBOX","UNREAD"],"sizeEstimate":1234,"payload":{"headers":[],"parts":[{"partId":"0","body":{"size":0}}]},"draft":false,"x":null}]}"#;
        let expected = r#"{
    "id": "t1",
    "historyId": "42",
    "messages": [
        {
            "id": "m1",
            "labelIds": [
                "INBOX",
                "UNREAD"
            ],
            "sizeEstimate": 1234,
            "payload": {
                "headers": [],
                "parts": [
                    {
                        "partId": "0",
                        "body": {
                            "size": 0
                        }
                    }
                ]
            },
            "draft": false,
            "x": null
        }
    ]
}"#;
        assert_eq!(source(json), expected);
    }

    #[test]
    fn snippets_are_decoded_and_strings_quoted_as_json_stringify_does() {
        let json = r#"{"messages":[{"snippet":"today&#39;s <b> &amp; more","payload":{"headers":[{"name":"To","value":"\"Maya\" <m@x.co>,\tPriya\u0001 é"}]}}]}"#;
        let shown = source(json);
        assert!(
            shown.contains(r#""snippet": "today's <b> & more""#),
            "{shown}"
        );
        assert!(
            shown.contains(r#""value": "\"Maya\" <m@x.co>,\tPriya\u0001 é""#),
            "{shown}"
        );
    }
}

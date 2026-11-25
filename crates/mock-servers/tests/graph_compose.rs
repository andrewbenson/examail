//! Drafts and sending through Graph: JSON and MIME, replies and forwards, attachments small and
//! large, and the IDs a draft keeps once it's sent.

mod graph;

use graph::{Graph, assert_error, mime, recipients, urlencode};
use reqwest::Method;
use serde_json::{Value, json};

const IMMUTABLE: (&str, &str) = ("Prefer", "IdType=\"ImmutableId\"");

async fn folder_id(graph: &Graph, name: &str) -> String {
    graph
        .get(&format!("/v1.0/me/mailFolders/{name}"))
        .await
        .json["id"]
        .as_str()
        .unwrap()
        .to_owned()
}

async fn sent_with_subject(graph: &Graph, subject: &str) -> Vec<Value> {
    let filter = urlencode(&format!("subject eq '{subject}'"));
    graph
        .get(&format!(
            "/v1.0/me/mailFolders/sentitems/messages?$filter={filter}"
        ))
        .await
        .json["value"]
        .as_array()
        .unwrap()
        .clone()
}

#[tokio::test]
async fn mime_in_and_out() {
    let graph = Graph::start().await;
    let message = graph.first("inbox", "hasAttachments eq true").await;
    let id = message["id"].as_str().unwrap();
    let raw = graph.get(&format!("/v1.0/me/messages/{id}/$value")).await;
    assert_eq!(raw.status, 200);
    let text = String::from_utf8_lossy(&raw.bytes);
    assert!(
        text.contains(&format!(
            "Subject: {}",
            message["subject"].as_str().unwrap()
        )) || text.contains("Subject:")
    );
    assert!(
        text.contains("Content-Disposition: attachment"),
        "the MIME keeps its attachments"
    );

    let eml = "From: Maya Okafor <maya@brightmoor.co>\nTo: Grace Liu <grace@juniperpartners.vc>\nSubject: Board deck, final\nX-Exo-Probe: 7\nMIME-Version: 1.0\nContent-Type: text/plain; charset=utf-8\n\nAttached is the final deck.\n";
    let draft = graph
        .request(
            Method::POST,
            "/v1.0/me/messages",
            &[],
            Some(("text/plain", mime(eml))),
        )
        .await;
    assert_eq!(draft.status, 201, "{}", draft.json);
    assert_eq!(draft.json["isDraft"], true);
    assert_eq!(draft.json["subject"], "Board deck, final");
    assert_eq!(
        draft.json["parentFolderId"],
        folder_id(&graph, "drafts").await
    );
    assert_eq!(
        draft.json["toRecipients"],
        json!([{ "emailAddress": { "name": "Grace Liu", "address": "grace@juniperpartners.vc" } }])
    );
    let draft_id = draft.json["id"].as_str().unwrap();
    let back = graph
        .get(&format!("/v1.0/me/messages/{draft_id}/$value"))
        .await;
    assert!(String::from_utf8_lossy(&back.bytes).contains("X-Exo-Probe: 7"));

    let refused = graph
        .request(
            Method::PATCH,
            &format!("/v1.0/me/messages/{draft_id}"),
            &[],
            Some(("text/plain", mime(eml))),
        )
        .await;
    assert_eq!(
        refused.status, 415,
        "a draft's MIME can't be patched: {}",
        refused.json
    );

    let send = "From: Maya Okafor <maya@brightmoor.co>\nTo: Ben Ortiz <bortiz@marlowfinch.law>\nSubject: Countersigned MSA\nMIME-Version: 1.0\nContent-Type: text/plain\n\nSigned copy attached.\n";
    let sent = graph
        .request(
            Method::POST,
            "/v1.0/me/sendMail",
            &[],
            Some(("text/plain", mime(send))),
        )
        .await;
    assert_eq!(sent.status, 202, "{}", sent.json);
    assert_eq!(
        sent_with_subject(&graph, "Countersigned MSA").await.len(),
        1
    );

    let invalid = graph
        .request(
            Method::POST,
            "/v1.0/me/sendMail",
            &[],
            Some(("text/plain", b"not base64!".to_vec())),
        )
        .await;
    assert_error(&invalid, 400, "ErrorMimeContentInvalidBase64String");
    assert_eq!(
        invalid.json["error"]["message"],
        "Invalid base64 string for MIME content."
    );
}

#[tokio::test]
async fn a_sent_draft_keeps_its_immutable_id() {
    let graph = Graph::start().await;
    let draft = graph
        .json_with(
            Method::POST,
            "/v1.0/me/messages",
            &[IMMUTABLE],
            json!({
                "subject": "Pricing v2",
                "body": { "contentType": "text", "content": "First pass." },
                "toRecipients": recipients(&["lena@brightmoor.co"]),
                "importance": "high",
            }),
        )
        .await;
    assert_eq!(draft.status, 201, "{}", draft.json);
    let id = draft.json["id"].as_str().unwrap().to_owned();
    assert_eq!(draft.json["importance"], "high");
    assert_eq!(
        draft.json["body"]["contentType"], "html",
        "bodies come back as HTML by default"
    );

    let edited = graph
        .json_with(
            Method::PATCH,
            &format!("/v1.0/me/messages/{id}"),
            &[IMMUTABLE],
            json!({ "subject": "Pricing v2 — mockups", "ccRecipients": recipients(&["tom@brightmoor.co"]), "body": { "contentType": "html", "content": "<p>Second pass.</p>" } }),
        )
        .await;
    assert_eq!(edited.status, 200, "{}", edited.json);
    assert_eq!(
        edited.json["id"],
        id.as_str(),
        "saving a draft keeps its ID"
    );
    assert_eq!(edited.json["subject"], "Pricing v2 — mockups");
    assert_eq!(
        edited.json["ccRecipients"][0]["emailAddress"]["address"],
        "tom@brightmoor.co"
    );
    assert_eq!(
        edited.json["importance"], "high",
        "the edit keeps what it didn't change"
    );

    let sent = graph
        .request(
            Method::POST,
            &format!("/v1.0/me/messages/{id}/send"),
            &[IMMUTABLE],
            None,
        )
        .await;
    assert_eq!(sent.status, 202, "{}", sent.json);
    let copy = graph
        .get_with(&format!("/v1.0/me/messages/{id}"), &[IMMUTABLE])
        .await;
    assert_eq!(
        copy.status, 200,
        "the Sent Items copy has the draft's immutable ID"
    );
    assert_eq!(copy.json["isDraft"], false);
    assert_eq!(
        copy.json["parentFolderId"],
        folder_id(&graph, "sentitems").await
    );
    assert_eq!(copy.json["subject"], "Pricing v2 — mockups");

    let mutable = graph
        .json(
            Method::POST,
            "/v1.0/me/messages",
            json!({ "subject": "Lunch", "toRecipients": recipients(&["sam@brightmoor.co"]) }),
        )
        .await;
    let old = mutable.json["id"].as_str().unwrap();
    assert_eq!(
        graph
            .request(
                Method::POST,
                &format!("/v1.0/me/messages/{old}/send"),
                &[],
                None
            )
            .await
            .status,
        202
    );
    assert_error(
        &graph.get(&format!("/v1.0/me/messages/{old}")).await,
        404,
        "ErrorItemNotFound",
    );

    let empty = graph
        .json(
            Method::POST,
            "/v1.0/me/messages",
            json!({ "subject": "Nobody" }),
        )
        .await;
    let empty_id = empty.json["id"].as_str().unwrap();
    assert_error(
        &graph
            .request(
                Method::POST,
                &format!("/v1.0/me/messages/{empty_id}/send"),
                &[],
                None,
            )
            .await,
        400,
        "ErrorInvalidRecipients",
    );
    let received = graph.first("inbox", "isDraft eq false").await;
    let received_id = received["id"].as_str().unwrap();
    assert_error(
        &graph
            .request(
                Method::POST,
                &format!("/v1.0/me/messages/{received_id}/send"),
                &[],
                None,
            )
            .await,
        400,
        "ErrorInvalidRequest",
    );
}

#[tokio::test]
async fn send_mail_and_sent_items() {
    let graph = Graph::start().await;
    let sent = graph
        .json(Method::POST, "/v1.0/me/sendMail", json!({ "message": { "subject": "Kept", "body": { "contentType": "Text", "content": "Hi" }, "toRecipients": recipients(&["daniel@brightmoor.co"]) } }))
        .await;
    assert_eq!(sent.status, 202, "{}", sent.json);
    let kept = sent_with_subject(&graph, "Kept").await;
    assert_eq!(kept.len(), 1);
    assert_eq!(
        kept[0]["from"]["emailAddress"]["address"],
        "maya@brightmoor.co"
    );
    assert_eq!(kept[0]["body"]["contentType"], "html");
    let unsaved = graph
        .json(Method::POST, "/v1.0/me/sendMail", json!({ "message": { "subject": "Not kept", "toRecipients": recipients(&["daniel@brightmoor.co"]) }, "saveToSentItems": false }))
        .await;
    assert_eq!(unsaved.status, 202);
    assert!(sent_with_subject(&graph, "Not kept").await.is_empty());
    let headers = graph
        .json(Method::POST, "/v1.0/me/sendMail", json!({ "message": { "subject": "Bad", "toRecipients": recipients(&["a@b.co"]), "internetMessageHeaders": [{ "name": "Reply-By", "value": "soon" }] } }))
        .await;
    assert_error(&headers, 400, "InvalidInternetMessageHeader");
}

#[tokio::test]
async fn replies_and_forwards_join_the_conversation() {
    let graph = Graph::start().await;
    let original = graph.first("inbox", "hasAttachments eq true").await;
    let id = original["id"].as_str().unwrap();
    let reply = graph
        .json(
            Method::POST,
            &format!("/v1.0/me/messages/{id}/createReply"),
            json!({ "comment": "Thanks, looking now." }),
        )
        .await;
    assert_eq!(reply.status, 201, "{}", reply.json);
    assert_eq!(reply.json["isDraft"], true);
    assert!(
        reply.json["subject"].as_str().unwrap().starts_with("RE: ")
            || reply.json["subject"]
                .as_str()
                .unwrap()
                .to_lowercase()
                .starts_with("re:")
    );
    assert_eq!(reply.json["conversationId"], original["conversationId"]);
    assert_eq!(
        reply.json["toRecipients"][0]["emailAddress"]["address"],
        original["from"]["emailAddress"]["address"]
    );
    assert!(
        reply.json["body"]["content"]
            .as_str()
            .unwrap()
            .contains("Thanks, looking now.")
    );
    assert!(
        reply.json["body"]["content"]
            .as_str()
            .unwrap()
            .contains("divRplyFwdMsg")
    );
    assert_eq!(reply.json["hasAttachments"], false);

    let all = graph
        .json(
            Method::POST,
            &format!("/v1.0/me/messages/{id}/createReplyAll"),
            json!({}),
        )
        .await;
    assert_eq!(all.status, 201, "{}", all.json);
    let everyone: Vec<&str> = all.json["toRecipients"]
        .as_array()
        .unwrap()
        .iter()
        .chain(all.json["ccRecipients"].as_array().unwrap())
        .map(|r| r["emailAddress"]["address"].as_str().unwrap())
        .collect();
    assert!(
        !everyone.contains(&"maya@brightmoor.co"),
        "reply-all leaves the sender out: {everyone:?}"
    );

    let forward = graph
        .json(
            Method::POST,
            &format!("/v1.0/me/messages/{id}/createForward"),
            json!({ "toRecipients": recipients(&["noor@brightmoor.co"]) }),
        )
        .await;
    assert_eq!(forward.status, 201, "{}", forward.json);
    assert!(
        forward.json["subject"]
            .as_str()
            .unwrap()
            .starts_with("FW: ")
    );
    assert_eq!(
        forward.json["hasAttachments"], true,
        "forwards carry the attachments"
    );
    assert_eq!(
        forward.json["toRecipients"][0]["emailAddress"]["address"],
        "noor@brightmoor.co"
    );
    let forward_id = forward.json["id"].as_str().unwrap();
    assert_eq!(
        graph
            .request(
                Method::POST,
                &format!("/v1.0/me/messages/{forward_id}/send"),
                &[],
                None
            )
            .await
            .status,
        202
    );
    let filter = urlencode(&format!(
        "conversationId eq '{}'",
        original["conversationId"].as_str().unwrap()
    ));
    let thread = graph
        .get(&format!("/v1.0/me/messages?$filter={filter}&$top=50"))
        .await
        .json;
    assert!(
        thread["value"]
            .as_array()
            .unwrap()
            .iter()
            .any(|m| m["subject"].as_str().unwrap().starts_with("FW: ") && m["isDraft"] == false)
    );
}

#[tokio::test]
async fn attachments_small_and_large() {
    let graph = Graph::start().await;
    let draft = graph
        .json(
            Method::POST,
            "/v1.0/me/messages",
            json!({ "subject": "Deck", "toRecipients": recipients(&["grace@juniperpartners.vc"]) }),
        )
        .await;
    let id = draft.json["id"].as_str().unwrap().to_owned();
    let small = graph
        .json(Method::POST, &format!("/v1.0/me/messages/{id}/attachments"), json!({ "@odata.type": "#microsoft.graph.fileAttachment", "name": "notes.txt", "contentType": "text/plain", "contentBytes": "SGVsbG8gV29ybGQh" }))
        .await;
    assert_eq!(small.status, 201, "{}", small.json);
    assert_eq!(
        (small.json["name"].as_str(), small.json["size"].as_u64()),
        (Some("notes.txt"), Some(12))
    );
    let small_id = small.json["id"].as_str().unwrap();
    let bytes = graph
        .get(&format!(
            "/v1.0/me/messages/{id}/attachments/{small_id}/$value"
        ))
        .await;
    assert_eq!(bytes.bytes, b"Hello World!");

    let file: Vec<u8> = (0..5_000_000u32).map(|n| (n % 251) as u8).collect();
    let session = graph
        .json(Method::POST, &format!("/v1.0/me/messages/{id}/attachments/createUploadSession"), json!({ "AttachmentItem": { "attachmentType": "file", "name": "deck.pdf", "size": file.len(), "contentType": "application/pdf" } }))
        .await;
    assert_eq!(session.status, 201, "{}", session.json);
    assert_eq!(session.json["nextExpectedRanges"], json!(["0-"]));
    let url = session.json["uploadUrl"].as_str().unwrap().to_owned();
    let put = |range: (usize, usize), body: Vec<u8>| {
        let http = graph.http.clone();
        let url = url.clone();
        let total = file.len();
        async move {
            http.put(url)
                .header(
                    "Content-Range",
                    format!("bytes {}-{}/{total}", range.0, range.1),
                )
                .header("Content-Type", "application/octet-stream")
                .body(body)
                .send()
                .await
                .unwrap()
        }
    };
    let chunk = 4 * 1024 * 1024;
    let out_of_order = put((10, 19), file[10..20].to_vec()).await;
    assert_eq!(out_of_order.status(), 400);
    let too_big = put((0, chunk), file[..=chunk].to_vec()).await;
    assert_eq!(too_big.status(), 413);
    let first = put((0, chunk - 1), file[..chunk].to_vec()).await;
    assert_eq!(first.status(), 200);
    let progress: Value = first.json().await.unwrap();
    assert_eq!(progress["nextExpectedRanges"], json!([format!("{chunk}-")]));
    let last = put((chunk, file.len() - 1), file[chunk..].to_vec()).await;
    assert_eq!(last.status(), 201);
    let location = last.headers()["location"].to_str().unwrap().to_owned();
    assert!(location.contains("/Attachments('"), "{location}");

    let listed = graph
        .get(&format!(
            "/v1.0/me/messages/{id}/attachments?$select=name,size"
        ))
        .await
        .json;
    let sizes: Vec<(String, u64)> = listed["value"]
        .as_array()
        .unwrap()
        .iter()
        .map(|a| {
            (
                a["name"].as_str().unwrap().to_owned(),
                a["size"].as_u64().unwrap(),
            )
        })
        .collect();
    assert_eq!(
        sizes,
        [
            ("notes.txt".to_owned(), 12),
            ("deck.pdf".to_owned(), 5_000_000)
        ]
    );
    let large_id = location
        .rsplit("Attachments('")
        .next()
        .unwrap()
        .trim_end_matches("')");
    let large = graph
        .get(&format!(
            "/v1.0/me/messages/{id}/attachments/{large_id}/$value"
        ))
        .await;
    assert_eq!(large.bytes, file);
    assert_eq!(large.headers["content-type"], "application/pdf");

    let tiny = graph
        .json(Method::POST, &format!("/v1.0/me/messages/{id}/attachments/createUploadSession"), json!({ "AttachmentItem": { "attachmentType": "file", "name": "tiny.txt", "size": 1024 } }))
        .await;
    assert_error(
        &tiny,
        400,
        "ErrorAttachmentSizeShouldNotBeLessThanMinimumSize",
    );

    assert_eq!(
        graph
            .request(
                Method::DELETE,
                &format!("/v1.0/me/messages/{id}/attachments/{small_id}"),
                &[],
                None
            )
            .await
            .status,
        204
    );
    let listed = graph
        .get(&format!("/v1.0/me/messages/{id}/attachments"))
        .await
        .json;
    assert_eq!(listed["value"].as_array().unwrap().len(), 1);
}

#[tokio::test]
async fn replies_and_forwards_sent_at_once() {
    let graph = Graph::start().await;
    let original = graph.first("inbox", "hasAttachments eq true").await;
    let id = original["id"].as_str().unwrap();
    let subject = original["subject"].as_str().unwrap();
    let conversation = urlencode(&format!(
        "conversationId eq '{}'",
        original["conversationId"].as_str().unwrap()
    ));
    let before = sent_in_thread(&graph, &conversation).await.len();

    let replied = graph
        .json(
            Method::POST,
            &format!("/v1.0/me/messages/{id}/reply"),
            json!({ "comment": "On it." }),
        )
        .await;
    assert_eq!(replied.status, 202, "{}", replied.json);
    let sent = sent_in_thread(&graph, &conversation).await;
    assert_eq!(sent.len(), before + 1);
    let reply = sent
        .iter()
        .find(|m| m["body"]["content"].as_str().unwrap().contains("On it."))
        .unwrap();
    assert_eq!(reply["subject"], format!("RE: {subject}"));
    assert_eq!(
        reply["toRecipients"][0]["emailAddress"]["address"],
        original["from"]["emailAddress"]["address"]
    );
    assert_eq!(reply["isDraft"], false);

    assert_eq!(
        graph
            .json(
                Method::POST,
                &format!("/v1.0/me/messages/{id}/replyAll"),
                json!({ "comment": "Thanks all." })
            )
            .await
            .status,
        202
    );
    let both = graph
        .json(
            Method::POST,
            &format!("/v1.0/me/messages/{id}/forward"),
            json!({ "toRecipients": recipients(&["noor@brightmoor.co"]), "message": { "toRecipients": recipients(&["noor@brightmoor.co"]) } }),
        )
        .await;
    assert_error(&both, 400, "ErrorInvalidRequest");
    let comment_and_body = graph
        .json(
            Method::POST,
            &format!("/v1.0/me/messages/{id}/reply"),
            json!({ "comment": "Hi", "message": { "body": { "contentType": "text", "content": "Hi" } } }),
        )
        .await;
    assert_error(&comment_and_body, 400, "ErrorInvalidRequest");
    let nobody = graph
        .json(
            Method::POST,
            &format!("/v1.0/me/messages/{id}/forward"),
            json!({ "comment": "FYI" }),
        )
        .await;
    assert_error(&nobody, 400, "ErrorInvalidRecipients");
    let forwarded = graph
        .json(
            Method::POST,
            &format!("/v1.0/me/messages/{id}/forward"),
            json!({ "comment": "FYI", "toRecipients": recipients(&["noor@brightmoor.co"]) }),
        )
        .await;
    assert_eq!(forwarded.status, 202, "{}", forwarded.json);
    let sent = sent_in_thread(&graph, &conversation).await;
    let forward = sent
        .iter()
        .find(|m| m["subject"] == format!("FW: {subject}"))
        .expect("the forward is in Sent Items");
    assert_eq!(forward["hasAttachments"], true);
    assert_eq!(
        graph.get("/v1.0/me/mailFolders/drafts").await.json["totalItemCount"],
        4,
        "no drafts are left behind"
    );

    let eml = format!(
        "From: Maya Okafor <maya@brightmoor.co>\nTo: Ben Ortiz <bortiz@marlowfinch.law>\nSubject: Re: {subject}\nMIME-Version: 1.0\nContent-Type: text/plain\n\nSent as MIME.\n"
    );
    let mime_reply = graph
        .request(
            Method::POST,
            &format!("/v1.0/me/messages/{id}/reply"),
            &[],
            Some(("text/plain", mime(&eml))),
        )
        .await;
    assert_eq!(mime_reply.status, 202, "{}", mime_reply.json);
    assert_eq!(
        sent_in_thread(&graph, &conversation).await.len(),
        before + 4
    );
}

/// Sent Items' messages matching a `$filter`.
async fn sent_in_thread(graph: &Graph, filter: &str) -> Vec<Value> {
    let path = format!("/v1.0/me/mailFolders/sentitems/messages?$filter={filter}&$top=50");
    graph.get(&path).await.json["value"]
        .as_array()
        .unwrap()
        .clone()
}

//! Wire-format tests for the Graph side: raw responses checked against Microsoft's published
//! v1.0 metadata (`tests/graph/metadata.xml`) and against OData's conventions.

mod graph;

use graph::{Graph, Metadata, assert_error, recipients};
use reqwest::Method;
use serde_json::{Value, json};

#[tokio::test]
async fn mail_resources_match_the_metadata() {
    let graph = Graph::start().await;
    let metadata = Metadata::load();

    let me = graph.get("/v1.0/me").await;
    assert_eq!(me.status, 200, "{}", me.json);
    metadata.assert("user", &me.json);

    let folders = graph
        .get("/v1.0/me/mailFolders?includeHiddenFolders=true")
        .await;
    metadata.assert_collection("mailFolder", &folders.json);
    for name in ["inbox", "msgfolderroot", "sentitems"] {
        let folder = graph.get(&format!("/v1.0/me/mailFolders/{name}")).await;
        assert_eq!(folder.status, 200, "{name}: {}", folder.json);
        metadata.assert("mailFolder", &folder.json);
    }
    let (folder_delta, _) = graph.pages("/v1.0/me/mailFolders/delta", &[]).await;
    for folder in &folder_delta {
        metadata.assert("mailFolder", folder);
    }

    let messages = graph.get("/v1.0/me/messages?$top=25").await;
    metadata.assert_collection("message", &messages.json);
    let selected = graph
        .get("/v1.0/me/messages?$top=5&$select=subject,uniqueBody,internetMessageHeaders,flag,from")
        .await;
    metadata.assert_collection("message", &selected.json);
    let attached = graph
        .get("/v1.0/me/messages?$search=%22hasAttachments:true%22&$top=3&$expand=attachments")
        .await;
    assert!(
        !attached.json["value"].as_array().unwrap().is_empty(),
        "{}",
        attached.json
    );
    metadata.assert_collection("message", &attached.json);
    let id = attached.json["value"][0]["id"].as_str().unwrap();
    let attachments = graph
        .get(&format!("/v1.0/me/messages/{id}/attachments"))
        .await;
    metadata.assert_collection("attachment", &attachments.json);
    let text = graph
        .get_with(
            &format!("/v1.0/me/messages/{id}"),
            &[("Prefer", "outlook.body-content-type=\"text\"")],
        )
        .await;
    metadata.assert("message", &text.json);
    assert_eq!(text.json["body"]["contentType"], "text");

    let (delta, _) = graph
        .pages(
            "/v1.0/me/mailFolders/inbox/messages/delta",
            &[("Prefer", "odata.maxpagesize=50")],
        )
        .await;
    assert!(!delta.is_empty());
    for message in &delta {
        metadata.assert("message", message);
    }

    for (path, kind) in [
        ("/v1.0/me/outlook/masterCategories", "outlookCategory"),
        ("/v1.0/me/mailFolders/inbox/messageRules", "messageRule"),
        ("/v1.0/me/people?$top=20", "person"),
        ("/v1.0/users", "user"),
        ("/v1.0/me/contacts?$top=50", "contact"),
    ] {
        let reply = graph.get(path).await;
        assert_eq!(reply.status, 200, "{path}: {}", reply.json);
        assert!(
            !reply.json["value"].as_array().unwrap().is_empty(),
            "{path} is empty"
        );
        metadata.assert_collection(kind, &reply.json);
    }
    let settings = graph.get("/v1.0/me/mailboxSettings").await;
    metadata.assert("mailboxSettings", &settings.json);
    let (users, _) = graph.pages("/v1.0/users/delta", &[]).await;
    for user in &users {
        metadata.assert("user", user);
    }
    let (contacts, _) = graph.pages("/v1.0/me/contacts/delta", &[]).await;
    for contact in &contacts {
        metadata.assert("contact", contact);
    }
    let folder = contacts[0]["parentFolderId"].as_str().unwrap();
    let contact_folder = graph
        .get(&format!("/v1.0/me/contactFolders/{folder}"))
        .await;
    metadata.assert("contactFolder", &contact_folder.json);
}

#[tokio::test]
async fn written_resources_match_the_metadata() {
    let graph = Graph::start().await;
    let metadata = Metadata::load();
    let draft = graph
        .json(
            Method::POST,
            "/v1.0/me/messages",
            json!({
                "subject": "Atlas pilot numbers",
                "body": { "contentType": "html", "content": "<p>Here they are.</p>" },
                "toRecipients": recipients(&["rachel.kim@callowayfreight.com"]),
                "flag": { "flagStatus": "flagged" },
                "categories": ["Clients/Calloway"],
            }),
        )
        .await;
    assert_eq!(draft.status, 201, "{}", draft.json);
    metadata.assert("message", &draft.json);
    let id = draft.json["id"].as_str().unwrap();
    let attachment = graph
        .json(
            Method::POST,
            &format!("/v1.0/me/messages/{id}/attachments"),
            json!({ "@odata.type": "#microsoft.graph.fileAttachment", "name": "pilot.csv", "contentBytes": "bGFuZSx0aW1lCg==" }),
        )
        .await;
    assert_eq!(attachment.status, 201, "{}", attachment.json);
    metadata.assert("attachment", &attachment.json);
    let session = graph
        .json(
            Method::POST,
            &format!("/v1.0/me/messages/{id}/attachments/createUploadSession"),
            json!({ "AttachmentItem": { "attachmentType": "file", "name": "deck.pdf", "size": 3_500_000 } }),
        )
        .await;
    assert_eq!(session.status, 201, "{}", session.json);
    metadata.assert("uploadSession", &session.json);
    let category = graph
        .json(
            Method::POST,
            "/v1.0/me/outlook/masterCategories",
            json!({ "displayName": "Board", "color": "preset9" }),
        )
        .await;
    assert_eq!(category.status, 201, "{}", category.json);
    metadata.assert("outlookCategory", &category.json);
    let rule = graph
        .json(
            Method::POST,
            "/v1.0/me/mailFolders/inbox/messageRules",
            json!({
                "displayName": "Ledgerly receipts",
                "sequence": 9,
                "isEnabled": true,
                "conditions": { "senderContains": ["ledgerly.app"] },
                "actions": { "assignCategories": ["Travel"], "markAsRead": true, "stopProcessingRules": true },
            }),
        )
        .await;
    assert_eq!(rule.status, 201, "{}", rule.json);
    metadata.assert("messageRule", &rule.json);
    let override_ = graph
        .json(
            Method::POST,
            "/v1.0/me/inferenceClassification/overrides",
            json!({ "classifyAs": "focused", "senderEmailAddress": { "name": "Freight Signals", "address": "newsletter@freightsignals.com" } }),
        )
        .await;
    assert_eq!(override_.status, 201, "{}", override_.json);
    metadata.assert("inferenceClassificationOverride", &override_.json);
    let folder = graph
        .json(
            Method::POST,
            "/v1.0/me/mailFolders/inbox/childFolders",
            json!({ "displayName": "Invoices" }),
        )
        .await;
    assert_eq!(folder.status, 201, "{}", folder.json);
    metadata.assert("mailFolder", &folder.json);
}

#[tokio::test]
async fn photos_follow_the_photo_settings() {
    let graph = Graph::with(mock_servers::Config {
        photo: true,
        contact_photos: true,
        ..mock_servers::Config::default()
    })
    .await;
    let metadata = Metadata::load();
    let photo = graph.get("/v1.0/me/photo").await;
    assert_eq!(photo.status, 200, "{}", photo.json);
    metadata.assert("profilePhoto", &photo.json);
    let bytes = graph.get("/v1.0/me/photo/$value").await;
    assert_eq!(bytes.headers["content-type"], "image/png");
    assert!(bytes.bytes.starts_with(b"\x89PNG"));
    let users = graph
        .get("/v1.0/users?$filter=mail%20eq%20'priya@brightmoor.co'")
        .await;
    let priya = users.json["value"][0]["id"].as_str().unwrap();
    let theirs = graph
        .get(&format!("/v1.0/users/{priya}/photo/$value"))
        .await;
    assert_eq!(theirs.status, 200);

    let plain = Graph::start().await;
    assert_error(
        &plain.get("/v1.0/me/photo/$value").await,
        404,
        "ImageNotFound",
    );
}

#[tokio::test]
async fn odata_conventions_hold() {
    let graph = Graph::start().await;
    let list = graph
        .get("/v1.0/me/mailFolders/inbox/messages?$top=3&$select=subject")
        .await;
    assert_eq!(
        list.headers["content-type"],
        "application/json;odata.metadata=minimal;odata.streaming=true;IEEE754Compatible=false;charset=utf-8"
    );
    assert_eq!(list.headers["odata-version"], "4.0");
    assert!(list.headers.contains_key("request-id"));
    let context = list.json["@odata.context"].as_str().unwrap();
    assert!(
        context.contains("/v1.0/$metadata#users('") && context.ends_with("/messages(subject)"),
        "{context}"
    );
    let message = &list.json["value"][0];
    let keys: Vec<&str> = message
        .as_object()
        .unwrap()
        .keys()
        .map(String::as_str)
        .collect();
    assert_eq!(
        keys,
        ["@odata.etag", "id", "subject"],
        "$select keeps the ETag and the ID"
    );
    let next = list.json["@odata.nextLink"].as_str().unwrap();
    assert!(
        next.contains("$skip=3") && next.contains("$top=3"),
        "{next}"
    );
    assert!(
        next.starts_with(&graph.server.url().to_string()),
        "links point back at the mock: {next}"
    );

    let full = graph
        .get("/v1.0/me/mailFolders/inbox/messages?$top=1")
        .await;
    let message = &full.json["value"][0];
    assert!(
        message["ccRecipients"].is_array(),
        "Graph sends empty collections"
    );
    assert!(message.get("uniqueBody").is_none() && message.get("internetMessageHeaders").is_none());
    assert!(message["receivedDateTime"].as_str().unwrap().ends_with('Z'));

    let echoed = graph
        .get_with(
            "/v1.0/me/messages/nope",
            &[("client-request-id", "abc-123")],
        )
        .await;
    assert_error(&echoed, 400, "ErrorInvalidIdMalformed");
    assert_eq!(
        echoed.json["error"]["innerError"]["client-request-id"],
        "abc-123"
    );
    assert_eq!(echoed.headers["client-request-id"], "abc-123");

    assert_error(
        &graph.get("/v1.0/me/messages?$expnad=attachments").await,
        400,
        "BadRequest",
    );
    assert_error(
        &graph.get("/v1.0/me/messages?$select=subjcet").await,
        400,
        "RequestBroker--ParseUri",
    );
    assert_error(&graph.get("/v1.0/me/notAThing").await, 400, "BadRequest");
    let unknown = graph
        .json(Method::PATCH, "/v1.0/me/messages/x", json!({}))
        .await;
    assert_error(&unknown, 400, "ErrorInvalidIdMalformed");

    let lowercase = graph
        .get("/v1.0/me/mailfolders('inbox')/MESSAGES?$TOP=1")
        .await;
    assert_eq!(
        lowercase.status, 200,
        "segments and options match in any case: {}",
        lowercase.json
    );
    assert_eq!(lowercase.json["value"].as_array().unwrap().len(), 1);
    let _: Value = lowercase.json;
}

#[test]
fn the_metadata_check_catches_violations() {
    let metadata = Metadata::load();
    let bad = json!({
        "id": "x",
        "isRead": "yes",
        "subjct": "typo",
        "importance": "urgent",
        "receivedDateTime": "yesterday",
        "toRecipients": [{ "emailAddress": { "adress": "a@b.co" } }],
        "attachments": [{ "@odata.type": "#microsoft.graph.fileAttachment", "isInline": null, "contentBytes": "not base64!" }],
    });
    let failure = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        metadata.assert("message", &bad)
    }))
    .expect_err("the message is invalid");
    let report = failure
        .downcast_ref::<String>()
        .cloned()
        .unwrap_or_default();
    for expected in [
        "message.isRead",
        "message.subjct: not a property",
        "message.importance",
        "message.receivedDateTime",
        "message.toRecipients[0].emailAddress.adress: not a property",
        "message.attachments[0].isInline: null",
        "message.attachments[0].contentBytes",
    ] {
        assert!(
            report.contains(expected),
            "missing {expected} in:\n{report}"
        );
    }
}

#[tokio::test]
async fn translated_ids_and_meeting_mail_match_the_metadata() {
    let graph = Graph::start().await;
    let metadata = Metadata::load();
    let message = graph.first("inbox", "isRead eq true").await;
    let translated = graph
        .json(
            Method::POST,
            "/v1.0/me/translateExchangeIds",
            json!({ "inputIds": [message["id"], "nope"], "sourceIdType": "restId", "targetIdType": "restImmutableEntryId" }),
        )
        .await;
    assert_eq!(translated.status, 200, "{}", translated.json);
    metadata.assert_collection("convertIdResult", &translated.json);
    let invites = graph
        .get("/v1.0/me/messages?$search=%22subject:Invitation%22&$expand=microsoft.graph.eventMessage/event")
        .await;
    assert!(!invites.json["value"].as_array().unwrap().is_empty());
    metadata.assert_collection("message", &invites.json);
}

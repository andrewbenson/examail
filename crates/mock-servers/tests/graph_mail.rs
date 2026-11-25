//! The mailbox as Outlook sees it through Graph: the projection of Gmail's labels, folder
//! deltas (with default and immutable IDs across moves), queries, and changes made on either
//! side showing up on the other.

mod graph;

use std::collections::BTreeSet;

use graph::{Graph, assert_error, urlencode};
use reqwest::Method;
use serde_json::{Value, json};

const IMMUTABLE: (&str, &str) = ("Prefer", "IdType=\"ImmutableId\"");

/// A Gmail label's message counts, from Gmail's own API.
async fn gmail_label(graph: &Graph, id: &str) -> (u64, u64) {
    let label = graph
        .get(&format!("/gmail/v1/users/me/labels/{id}"))
        .await
        .json;
    (
        label["messagesTotal"].as_u64().unwrap(),
        label["messagesUnread"].as_u64().unwrap(),
    )
}

/// A Graph message's Gmail labels, found by its Message-ID.
async fn gmail_labels(graph: &Graph, message: &Value) -> BTreeSet<String> {
    let id = gmail_id(graph, message).await;
    let gmail = graph
        .get(&format!("/gmail/v1/users/me/messages/{id}?format=minimal"))
        .await
        .json;
    gmail["labelIds"]
        .as_array()
        .unwrap()
        .iter()
        .map(|l| l.as_str().unwrap().to_owned())
        .collect()
}

/// A Graph message's Gmail ID, found by its Message-ID.
async fn gmail_id(graph: &Graph, message: &Value) -> String {
    let rfc822 = message["internetMessageId"]
        .as_str()
        .unwrap()
        .trim_matches(['<', '>']);
    let found = graph
        .get(&format!(
            "/gmail/v1/users/me/messages?includeSpamTrash=true&q={}",
            urlencode(&format!("rfc822msgid:{rfc822}"))
        ))
        .await
        .json;
    found["messages"][0]["id"]
        .as_str()
        .expect("the message is in Gmail")
        .to_owned()
}

/// How many messages a Gmail search finds.
async fn gmail_count(graph: &Graph, query: &str) -> usize {
    let found = graph
        .get(&format!(
            "/gmail/v1/users/me/messages?includeSpamTrash=true&maxResults=500&q={}",
            urlencode(query)
        ))
        .await
        .json;
    found["messages"].as_array().map_or(0, Vec::len)
}

/// The ID of the Inbox's user folder named `name`.
async fn child(graph: &Graph, name: &str) -> String {
    let children = graph
        .get("/v1.0/me/mailFolders/inbox/childFolders")
        .await
        .json;
    children["value"]
        .as_array()
        .unwrap()
        .iter()
        .find(|folder| folder["displayName"] == name)
        .unwrap_or_else(|| panic!("no {name} under the Inbox: {children}"))["id"]
        .as_str()
        .unwrap()
        .to_owned()
}

fn removed(id: &str) -> Value {
    json!({ "@odata.type": "#microsoft.graph.message", "id": id, "@removed": { "reason": "deleted" } })
}

async fn folder(graph: &Graph, name: &str) -> Value {
    let reply = graph.get(&format!("/v1.0/me/mailFolders/{name}")).await;
    assert_eq!(reply.status, 200, "{}", reply.json);
    reply.json
}

/// Follows a delta round from `link` to its `@odata.deltaLink`.
async fn delta_round(graph: &Graph, link: &str, headers: &[(&str, &str)]) -> (Vec<Value>, String) {
    let (items, last) = graph.pages(link, headers).await;
    let next = last["@odata.deltaLink"]
        .as_str()
        .expect("a round ends with a delta link")
        .to_owned();
    (items, next)
}

fn ids(items: &[Value]) -> Vec<String> {
    items
        .iter()
        .map(|item| item["id"].as_str().unwrap().to_owned())
        .collect()
}

#[tokio::test]
async fn folders_mirror_gmail_labels() {
    let graph = Graph::start().await;
    for (name, label) in [
        ("inbox", "INBOX"),
        ("deleteditems", "TRASH"),
        ("junkemail", "SPAM"),
        ("drafts", "DRAFT"),
    ] {
        let folder = folder(&graph, name).await;
        let (total, unread) = gmail_label(&graph, label).await;
        assert_eq!(folder["totalItemCount"], total, "{name}");
        assert_eq!(folder["unreadItemCount"], unread, "{name}");
    }
    let profile = graph.get("/gmail/v1/users/me/profile").await.json;
    let (every_folder, _) = graph.pages("/v1.0/me/mailFolders/delta", &[]).await;
    let sum: u64 = every_folder
        .iter()
        .map(|f| f["totalItemCount"].as_u64().unwrap())
        .sum();
    assert_eq!(
        sum,
        profile["messagesTotal"].as_u64().unwrap(),
        "every message sits in exactly one folder"
    );
    let folders = graph.get("/v1.0/me/mailFolders").await.json;
    let names: Vec<&str> = folders["value"]
        .as_array()
        .unwrap()
        .iter()
        .map(|f| f["displayName"].as_str().unwrap())
        .collect();
    assert_eq!(
        names,
        [
            "Archive",
            "Conversation History",
            "Deleted Items",
            "Drafts",
            "Inbox",
            "Junk Email",
            "Outbox",
            "Sent Items"
        ]
    );
    let root = folder(&graph, "msgfolderroot").await;
    assert_eq!(root["childFolderCount"], 8);
    assert_eq!(
        folder(&graph, "Inbox").await["id"],
        folder(&graph, "inbox").await["id"],
        "well-known names match in any case"
    );
    assert_error(
        &graph.get("/v1.0/me/mailFolders/recoverable").await,
        400,
        "ErrorInvalidIdMalformed",
    );

    let (starred, _) = gmail_label(&graph, "STARRED").await;
    let (flagged, _) = graph
        .pages(
            "/v1.0/me/messages?$top=200&$filter=flag/flagStatus%20eq%20'flagged'",
            &[],
        )
        .await;
    assert_eq!(flagged.len() as u64, starred, "flagged is starred");
    let (unread, _) = graph
        .pages(
            "/v1.0/me/mailFolders/inbox/messages?$top=200&$filter=isRead%20eq%20false",
            &[],
        )
        .await;
    assert_eq!(unread.len() as u64, gmail_label(&graph, "INBOX").await.1);
}

#[tokio::test]
async fn messages_carry_outlooks_view_of_their_labels() {
    let graph = Graph::start().await;
    let filter = urlencode("contains(subject, 'Q4 planning doc — comments by Thursday?')");
    let thread = graph
        .get(&format!("/v1.0/me/messages?$filter={filter}&$top=10"))
        .await
        .json;
    let messages = thread["value"].as_array().unwrap();
    assert_eq!(messages.len(), 4);
    let conversation = messages[0]["conversationId"].as_str().unwrap();
    assert!(messages.iter().all(|m| m["conversationId"] == conversation));
    for message in messages {
        let labels = gmail_labels(&graph, message).await;
        assert_eq!(message["isRead"], !labels.contains("UNREAD"));
        assert_eq!(
            message["flag"]["flagStatus"] == "flagged",
            labels.contains("STARRED")
        );
        assert_eq!(message["categories"], json!(["Projects/Atlas Launch"]));
        assert_eq!(message["inferenceClassification"], "focused");
    }
    let inbox = folder(&graph, "inbox").await;
    let sent = folder(&graph, "sentitems").await;
    let parents: BTreeSet<&str> = messages
        .iter()
        .map(|m| m["parentFolderId"].as_str().unwrap())
        .collect();
    assert!(
        parents.contains(inbox["id"].as_str().unwrap()),
        "received copies are in the Inbox"
    );
    assert!(
        parents.contains(sent["id"].as_str().unwrap()),
        "Maya's reply is in Sent Items"
    );

    // A conversation, oldest first: the filter names the sort's property first.
    let by_conversation = urlencode(&format!(
        "receivedDateTime gt 1970-01-01 and conversationId eq '{conversation}'"
    ));
    let ordered = graph
        .get(&format!(
            "/v1.0/me/messages?$filter={by_conversation}&$orderby=receivedDateTime&$top=100"
        ))
        .await;
    assert_eq!(ordered.status, 200, "{}", ordered.json);
    let times: Vec<&str> = ordered.json["value"]
        .as_array()
        .unwrap()
        .iter()
        .map(|m| m["receivedDateTime"].as_str().unwrap())
        .collect();
    assert_eq!(times.len(), 4);
    assert!(times.windows(2).all(|pair| pair[0] <= pair[1]), "{times:?}");
    let backwards = urlencode(&format!("conversationId eq '{conversation}'"));
    let refused = graph
        .get(&format!(
            "/v1.0/me/messages?$filter={backwards}&$orderby=receivedDateTime"
        ))
        .await;
    assert_error(&refused, 400, "InefficientFilter");

    let promotions = graph
        .get("/gmail/v1/users/me/messages?q=category:promotions%20-is:important&maxResults=1")
        .await
        .json;
    let id = promotions["messages"][0]["id"].as_str().unwrap();
    let raw = graph
        .get(&format!(
            "/gmail/v1/users/me/messages/{id}?format=metadata&metadataHeaders=Message-ID"
        ))
        .await
        .json;
    let message_id = raw["payload"]["headers"][0]["value"].as_str().unwrap();
    let filter = urlencode(&format!("internetMessageId eq '{message_id}'"));
    let promotion = graph
        .get(&format!("/v1.0/me/messages?$filter={filter}"))
        .await
        .json;
    assert_eq!(
        promotion["value"][0]["inferenceClassification"], "other",
        "Gmail's Promotions tab is Other"
    );
}

#[tokio::test]
async fn searching_and_paging() {
    let graph = Graph::start().await;
    let search = graph
        .get("/v1.0/me/messages?$search=%22from:daniel@brightmoor.co%22&$top=100")
        .await;
    assert_eq!(search.status, 200, "{}", search.json);
    let found = search.json["value"].as_array().unwrap();
    assert!(!found.is_empty());
    assert!(
        found
            .iter()
            .all(|m| m["from"]["emailAddress"]["address"] == "daniel@brightmoor.co")
    );
    let sent: Vec<&str> = found
        .iter()
        .map(|m| m["sentDateTime"].as_str().unwrap())
        .collect();
    assert!(
        sent.windows(2).all(|pair| pair[0] >= pair[1]),
        "search results come newest sent first"
    );

    let phrase = graph
        .get(&format!(
            "/v1.0/me/messages?$search={}",
            urlencode("\"\\\"rate-card import\\\" AND received>=2026-11-01\"")
        ))
        .await;
    assert_eq!(phrase.status, 200, "{}", phrase.json);
    assert!(
        phrase.json["value"]
            .as_array()
            .unwrap()
            .iter()
            .all(|m| m["receivedDateTime"].as_str().unwrap() >= "2026-11-01")
    );
    assert_error(
        &graph
            .get("/v1.0/me/messages?$search=%22atlas%22&$orderby=subject")
            .await,
        400,
        "ErrorInvalidUrlQuery",
    );

    let (all, last) = graph
        .pages("/v1.0/me/messages?$top=97&$select=id&$count=true", &[])
        .await;
    let unique: BTreeSet<String> = ids(&all).into_iter().collect();
    assert_eq!(unique.len(), all.len(), "pages don't repeat messages");
    let profile = graph.get("/gmail/v1/users/me/profile").await.json;
    assert_eq!(all.len() as u64, profile["messagesTotal"].as_u64().unwrap());
    assert_eq!(last["@odata.count"], all.len());

    let filtered = graph
        .get(&format!(
            "/v1.0/me/mailFolders/inbox/messages?$filter={}",
            urlencode("categories/any(c:c eq 'Projects/Atlas Launch') and hasAttachments eq false")
        ))
        .await;
    assert_eq!(filtered.status, 200, "{}", filtered.json);
    assert!(!filtered.json["value"].as_array().unwrap().is_empty());
}

#[tokio::test]
async fn deltas_follow_deliveries_moves_and_gmail() {
    let graph = Graph::start().await;
    let page = [("Prefer", "odata.maxpagesize=40")];
    let (first, mut inbox) = delta_round(
        &graph,
        "/v1.0/me/mailFolders/inbox/messages/delta?$select=subject,isRead,parentFolderId",
        &page,
    )
    .await;
    assert_eq!(first.len() as u64, gmail_label(&graph, "INBOX").await.0);
    assert!(
        first
            .iter()
            .all(|m| m["@odata.type"] == "#microsoft.graph.message"
                || m["@odata.type"] == "#microsoft.graph.eventMessageRequest")
    );
    assert!(
        first.iter().all(|m| m.get("body").is_none()),
        "the round keeps its $select"
    );
    let (_, mut archive) = delta_round(
        &graph,
        "/v1.0/me/mailFolders/archive/messages/delta?$select=subject",
        &page,
    )
    .await;

    let quiet = graph.get(&inbox).await.json;
    assert_eq!(quiet["value"], json!([]), "nothing changed");
    inbox = quiet["@odata.deltaLink"].as_str().unwrap().to_owned();

    // A delivery shows up as new in the Inbox.
    let delivered = graph
        .http
        .post(graph.url("/__mock/deliver"))
        .json(&json!({ "from": "Rachel Kim <rachel.kim@callowayfreight.com>", "subject": "Pilot lanes for December", "text": "Attached are the lanes." }))
        .send()
        .await
        .unwrap()
        .json::<Value>()
        .await
        .unwrap();
    let (changes, next) = delta_round(&graph, &inbox, &page).await;
    inbox = next;
    assert_eq!(changes.len(), 1, "{changes:#?}");
    assert_eq!(changes[0]["subject"], "Pilot lanes for December");
    assert_eq!(changes[0]["isRead"], false);
    let old_id = changes[0]["id"].as_str().unwrap().to_owned();

    // Marking it read in Gmail is an update.
    let gmail_id = delivered["id"].as_str().unwrap();
    graph
        .json(
            Method::POST,
            &format!("/gmail/v1/users/me/messages/{gmail_id}/modify"),
            json!({ "removeLabelIds": ["UNREAD"] }),
        )
        .await;
    let (changes, next) = delta_round(&graph, &inbox, &page).await;
    inbox = next;
    assert_eq!(changes.len(), 1);
    assert_eq!(
        (changes[0]["id"].as_str(), &changes[0]["isRead"]),
        (Some(old_id.as_str()), &json!(true))
    );

    // Archiving it through Graph gives it a new default ID.
    let moved = graph
        .json(
            Method::POST,
            &format!("/v1.0/me/messages/{old_id}/move"),
            json!({ "destinationId": "archive" }),
        )
        .await;
    assert_eq!(moved.status, 201, "{}", moved.json);
    let new_id = moved.json["id"].as_str().unwrap().to_owned();
    assert_ne!(new_id, old_id);
    assert_eq!(
        moved.json["parentFolderId"],
        folder(&graph, "archive").await["id"]
    );
    assert_error(
        &graph.get(&format!("/v1.0/me/messages/{old_id}")).await,
        404,
        "ErrorItemNotFound",
    );
    assert!(
        !gmail_labels(&graph, &moved.json).await.contains("INBOX"),
        "archived in Gmail too"
    );
    let (changes, next) = delta_round(&graph, &inbox, &page).await;
    inbox = next;
    assert_eq!(
        changes,
        vec![
            json!({ "@odata.type": "#microsoft.graph.message", "id": old_id, "@removed": { "reason": "deleted" } })
        ]
    );
    let (changes, next) = delta_round(&graph, &archive, &page).await;
    archive = next;
    assert_eq!(ids(&changes), [new_id.as_str()]);

    // With immutable IDs, a message keeps its ID wherever it goes.
    let immutable = [IMMUTABLE, ("Prefer", "odata.maxpagesize=40")];
    let (_, mut inbox_fixed) = delta_round(
        &graph,
        "/v1.0/me/mailFolders/inbox/messages/delta",
        &immutable,
    )
    .await;
    let (_, mut archive_fixed) = delta_round(
        &graph,
        "/v1.0/me/mailFolders/archive/messages/delta",
        &immutable,
    )
    .await;
    let fixed = graph
        .get_with(&format!("/v1.0/me/messages/{new_id}"), &[IMMUTABLE])
        .await
        .json["id"]
        .as_str()
        .unwrap()
        .to_owned();
    let back = graph
        .json_with(
            Method::POST,
            &format!("/v1.0/me/messages/{fixed}/move"),
            &[IMMUTABLE],
            json!({ "destinationId": "inbox" }),
        )
        .await;
    assert_eq!(back.status, 201, "{}", back.json);
    assert_eq!(back.json["id"], fixed.as_str());
    let (changes, next) = delta_round(&graph, &inbox_fixed, &immutable).await;
    inbox_fixed = next;
    assert_eq!(
        ids(&changes),
        [fixed.as_str()],
        "the same ID arrives in the Inbox"
    );
    let (changes, next) = delta_round(&graph, &archive_fixed, &immutable).await;
    archive_fixed = next;
    assert_eq!(
        changes,
        vec![
            json!({ "@odata.type": "#microsoft.graph.message", "id": fixed, "@removed": { "reason": "deleted" } })
        ]
    );
    assert_eq!(
        graph
            .get_with(&format!("/v1.0/me/messages/{fixed}"), &[IMMUTABLE])
            .await
            .status,
        200
    );

    // Deleting removes it from the folder.
    assert_eq!(
        graph
            .request(
                Method::DELETE,
                &format!("/v1.0/me/messages/{fixed}"),
                &[IMMUTABLE],
                None
            )
            .await
            .status,
        204
    );
    let (changes, _) = delta_round(&graph, &inbox_fixed, &immutable).await;
    assert_eq!(changes[0]["@removed"]["reason"], "deleted");
    let _ = (archive, archive_fixed);

    // Expiring history expires every delta token.
    assert_eq!(graph.admin("/__mock/history/expire", json!({})).await, 204);
    assert_error(&graph.get(&inbox).await, 410, "SyncStateNotFound");
}

#[tokio::test]
async fn delta_supports_top_filter_and_change_type() {
    let graph = Graph::start().await;
    let page = [("Prefer", "odata.maxpagesize=3")];
    let (capped, _) = delta_round(
        &graph,
        "/v1.0/me/mailFolders/inbox/messages/delta?$top=5&$orderby=receivedDateTime%20desc",
        &page,
    )
    .await;
    assert_eq!(capped.len(), 5, "$top caps the initial round");
    let filter = urlencode("receivedDateTime ge 2026-11-16T00:00:00Z");
    let (recent, _) = delta_round(
        &graph,
        &format!(
            "/v1.0/me/mailFolders/inbox/messages/delta?$filter={filter}&$select=receivedDateTime"
        ),
        &page,
    )
    .await;
    assert!(!recent.is_empty());
    assert!(
        recent
            .iter()
            .all(|m| m["receivedDateTime"].as_str().unwrap() >= "2026-11-16")
    );
    assert_error(
        &graph
            .get("/v1.0/me/mailFolders/inbox/messages/delta?$filter=isRead%20eq%20false")
            .await,
        400,
        "ErrorInvalidUrlQueryFilter",
    );
    assert_error(
        &graph
            .get("/v1.0/me/mailFolders/inbox/messages/delta?$orderby=subject")
            .await,
        400,
        "BadRequest",
    );

    let (_, created_only) = delta_round(
        &graph,
        "/v1.0/me/mailFolders/inbox/messages/delta?changeType=created&$select=subject",
        &[("Prefer", "odata.maxpagesize=100")],
    )
    .await;
    let first = graph.first("inbox", "isRead eq false").await;
    let id = first["id"].as_str().unwrap();
    graph
        .json(
            Method::PATCH,
            &format!("/v1.0/me/messages/{id}"),
            json!({ "isRead": true }),
        )
        .await;
    graph
        .admin(
            "/__mock/deliver",
            json!({ "subject": "Fresh one", "text": "hi" }),
        )
        .await;
    let (changes, _) = delta_round(&graph, &created_only, &[]).await;
    assert_eq!(changes.len(), 1, "only the new message: {changes:#?}");
    assert_eq!(changes[0]["subject"], "Fresh one");
}

#[tokio::test]
async fn patches_reach_gmail() {
    let graph = Graph::start().await;
    let message = graph.first("inbox", "isRead eq false").await;
    let id = message["id"].as_str().unwrap();
    let path = format!("/v1.0/me/messages/{id}");

    let read = graph
        .json(
            Method::PATCH,
            &path,
            json!({ "isRead": true, "flag": { "flagStatus": "flagged" } }),
        )
        .await;
    assert_eq!(read.status, 200, "{}", read.json);
    assert_eq!(
        (
            read.json["isRead"].as_bool(),
            read.json["flag"]["flagStatus"].as_str()
        ),
        (Some(true), Some("flagged"))
    );
    let labels = gmail_labels(&graph, &message).await;
    assert!(
        !labels.contains("UNREAD") && labels.contains("STARRED"),
        "{labels:?}"
    );

    let done = graph
        .json(
            Method::PATCH,
            &path,
            json!({ "flag": { "flagStatus": "complete" } }),
        )
        .await;
    assert_eq!(done.json["flag"]["flagStatus"], "complete", "{}", done.json);
    assert!(done.json["flag"]["completedDateTime"]["dateTime"].is_string());
    assert!(!gmail_labels(&graph, &message).await.contains("STARRED"));

    let tagged = graph
        .json(
            Method::PATCH,
            &path,
            json!({ "categories": ["Hiring", "Board 2027"] }),
        )
        .await;
    assert_eq!(
        tagged.json["categories"],
        json!(["Hiring", "Board 2027"]),
        "{}",
        tagged.json
    );
    let gmail = graph.get("/gmail/v1/users/me/labels").await.json;
    let board = gmail["labels"]
        .as_array()
        .unwrap()
        .iter()
        .find(|l| l["name"] == "Board 2027")
        .expect("a new label")
        .clone();
    assert!(
        gmail_labels(&graph, &message)
            .await
            .contains(board["id"].as_str().unwrap())
    );
    let master = graph.get("/v1.0/me/outlook/masterCategories").await.json;
    assert!(
        master["value"]
            .as_array()
            .unwrap()
            .iter()
            .any(|c| c["displayName"] == "Board 2027")
    );

    let other = graph
        .json(
            Method::PATCH,
            &path,
            json!({ "inferenceClassification": "other" }),
        )
        .await;
    assert_eq!(other.json["inferenceClassification"], "other");
    let labels = gmail_labels(&graph, &message).await;
    assert!(
        !labels.contains("CATEGORY_PERSONAL") && !labels.contains("IMPORTANT"),
        "{labels:?}"
    );

    let important = graph
        .json(Method::PATCH, &path, json!({ "importance": "high" }))
        .await;
    assert_eq!(
        important.json["importance"], "high",
        "stored beside Gmail for received mail"
    );

    assert_error(
        &graph
            .json(Method::PATCH, &path, json!({ "subject": "Edited" }))
            .await,
        400,
        "ErrorInvalidPropertyUpdateSentMessage",
    );
    assert_error(
        &graph
            .json(Method::PATCH, &path, json!({ "isReed": true }))
            .await,
        400,
        "RequestBodyRead",
    );
}

#[tokio::test]
async fn user_folders_are_labels() {
    let graph = Graph::start().await;
    let made = graph
        .json(
            Method::POST,
            "/v1.0/me/mailFolders/inbox/childFolders",
            json!({ "displayName": "Receipts 2026" }),
        )
        .await;
    assert_eq!(made.status, 201, "{}", made.json);
    let inbox = folder(&graph, "inbox").await;
    assert_eq!(made.json["parentFolderId"], inbox["id"]);
    assert_eq!(
        inbox["childFolderCount"], 3,
        "beside Receipts and Newsletters"
    );
    let children = graph
        .get("/v1.0/me/mailFolders/inbox/childFolders")
        .await
        .json;
    assert_eq!(children["value"][2]["displayName"], "Receipts 2026");
    let duplicate = graph
        .json(
            Method::POST,
            "/v1.0/me/mailFolders/inbox/childFolders",
            json!({ "displayName": "receipts 2026" }),
        )
        .await;
    assert_error(&duplicate, 409, "ErrorFolderExists");

    let folder_id = made.json["id"].as_str().unwrap();
    let message = graph.first("inbox", "isRead eq true").await;
    let moved = graph
        .json(
            Method::POST,
            &format!("/v1.0/me/messages/{}/move", message["id"].as_str().unwrap()),
            json!({ "destinationId": folder_id }),
        )
        .await;
    assert_eq!(moved.json["parentFolderId"], folder_id);
    let labels = gmail_labels(&graph, &moved.json).await;
    let gmail = graph.get("/gmail/v1/users/me/labels").await.json;
    let label = gmail["labels"]
        .as_array()
        .unwrap()
        .iter()
        .find(|l| l["name"] == "Inbox/Receipts 2026")
        .expect("a label for the folder");
    assert!(
        labels.contains(label["id"].as_str().unwrap()) && !labels.contains("INBOX"),
        "{labels:?}"
    );
    assert!(
        moved.json["categories"]
            .as_array()
            .unwrap()
            .iter()
            .all(|c| c != "Inbox/Receipts 2026"),
        "a folder isn't a category"
    );

    let (folders, mut link) = delta_round(&graph, "/v1.0/me/mailFolders/delta", &[]).await;
    assert!(folders.iter().any(|f| f["id"] == folder_id));
    let renamed = graph
        .json(
            Method::PATCH,
            &format!("/v1.0/me/mailFolders/{folder_id}"),
            json!({ "displayName": "Receipts" }),
        )
        .await;
    assert_eq!(renamed.json["displayName"], "Receipts", "{}", renamed.json);
    let (changes, next) = delta_round(&graph, &link, &[]).await;
    link = next;
    assert_eq!(changes.len(), 1);
    assert_eq!(changes[0]["displayName"], "Receipts");
    assert_eq!(
        graph
            .request(
                Method::DELETE,
                &format!("/v1.0/me/mailFolders/{folder_id}"),
                &[],
                None
            )
            .await
            .status,
        204
    );
    let (changes, _) = delta_round(&graph, &link, &[]).await;
    assert_eq!(
        changes,
        vec![json!({ "id": folder_id, "@removed": { "reason": "deleted" } })]
    );
    assert!(
        gmail_labels(&graph, &moved.json).await.contains("TRASH"),
        "a deleted folder's mail goes to Deleted Items"
    );
}

#[tokio::test]
async fn receipts_and_newsletters_are_inbox_folders() {
    let graph = Graph::start().await;
    let inbox = folder(&graph, "inbox").await;
    assert_eq!(inbox["childFolderCount"], 2);
    let children = graph
        .get("/v1.0/me/mailFolders/inbox/childFolders")
        .await
        .json;
    let listed = children["value"].as_array().unwrap();
    let names: Vec<&str> = listed
        .iter()
        .map(|f| f["displayName"].as_str().unwrap())
        .collect();
    assert_eq!(names, ["Receipts", "Newsletters"]);
    for listed in listed {
        assert_eq!(listed["parentFolderId"], inbox["id"]);
        assert_eq!(
            (&listed["isHidden"], &listed["childFolderCount"]),
            (&json!(false), &json!(0))
        );
    }

    // A labelled message the Inbox has stays in the Inbox; the folder holds the rest, sent mail
    // included.
    assert_eq!(
        listed[0]["totalItemCount"],
        gmail_count(&graph, "label:Receipts -in:inbox").await
    );
    assert_eq!(
        listed[1]["totalItemCount"],
        gmail_count(&graph, "label:Newsletters -in:inbox").await
    );
    assert_eq!(gmail_count(&graph, "label:Newsletters in:inbox").await, 1);
    let latest = graph
        .first(
            "inbox",
            "from/emailAddress/address eq 'newsletter@freightsignals.com'",
        )
        .await;
    assert_eq!(latest["parentFolderId"], inbox["id"]);
    let receipts = listed[0]["id"].as_str().unwrap();
    let held = graph
        .get(&format!("/v1.0/me/mailFolders/{receipts}/messages?$top=50"))
        .await
        .json;
    let held = held["value"].as_array().unwrap();
    assert!(
        held.iter()
            .any(|m| m["from"]["emailAddress"]["address"] == "maya@brightmoor.co"),
        "Maya's labelled reply is in Receipts, not Sent Items"
    );
    assert!(
        held.iter().all(|m| m["categories"] == json!([])),
        "a folder isn't a category"
    );

    let (every_folder, _) = graph.pages("/v1.0/me/mailFolders/delta", &[]).await;
    for name in ["Receipts", "Newsletters"] {
        let found = every_folder
            .iter()
            .find(|f| f["displayName"] == name)
            .expect("the hierarchy's delta lists the folder");
        assert_eq!(found["parentFolderId"], inbox["id"]);
    }
    let master = graph.get("/v1.0/me/outlook/masterCategories").await.json;
    assert!(
        master["value"]
            .as_array()
            .unwrap()
            .iter()
            .all(|c| c["displayName"] != "Receipts" && c["displayName"] != "Newsletters")
    );
    let rules = graph
        .get("/v1.0/me/mailFolders/inbox/messageRules")
        .await
        .json;
    let receipts_rule = rules["value"]
        .as_array()
        .unwrap()
        .iter()
        .find(|r| r["displayName"] == "From receipts@ledgerly.app")
        .unwrap();
    assert_eq!(
        receipts_rule["actions"],
        json!({ "stopProcessingRules": false }),
        "a filter that labels without skipping the inbox moves nothing in Outlook's terms"
    );
    let id = latest["id"].as_str().unwrap();
    let clash = graph
        .json(
            Method::PATCH,
            &format!("/v1.0/me/messages/{id}"),
            json!({ "categories": ["Receipts"] }),
        )
        .await;
    assert_error(&clash, 400, "ErrorInvalidRequest");

    let gmail = graph.get("/gmail/v1/users/me/labels").await.json;
    let names: Vec<&str> = gmail["labels"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|l| l["name"].as_str())
        .collect();
    assert!(
        names.contains(&"Receipts") && names.contains(&"Newsletters"),
        "Gmail's labels are as they were"
    );
}

#[tokio::test]
async fn moving_into_a_user_folder_and_back() {
    let graph = Graph::start().await;
    let page = [("Prefer", "odata.maxpagesize=50")];
    let receipts = child(&graph, "Receipts").await;
    let (held, mut in_receipts) = delta_round(
        &graph,
        &format!("/v1.0/me/mailFolders/{receipts}/messages/delta?$select=subject"),
        &page,
    )
    .await;
    assert_eq!(
        held.len(),
        gmail_count(&graph, "label:Receipts -in:inbox").await
    );
    let (_, mut in_inbox) = delta_round(
        &graph,
        "/v1.0/me/mailFolders/inbox/messages/delta?$select=subject",
        &page,
    )
    .await;

    // With default IDs, each move gives the message a new one.
    let message = graph.first("inbox", "isRead eq true").await;
    let first = message["id"].as_str().unwrap().to_owned();
    let moved = graph
        .json(
            Method::POST,
            &format!("/v1.0/me/messages/{first}/move"),
            json!({ "destinationId": receipts }),
        )
        .await;
    assert_eq!(moved.status, 201, "{}", moved.json);
    assert_eq!(moved.json["parentFolderId"], receipts.as_str());
    let second = moved.json["id"].as_str().unwrap().to_owned();
    assert_ne!(second, first);
    let labels = gmail_labels(&graph, &moved.json).await;
    assert!(
        labels.contains("Label_9") && !labels.contains("INBOX"),
        "{labels:?}"
    );
    let (changes, next) = delta_round(&graph, &in_receipts, &page).await;
    in_receipts = next;
    assert_eq!(ids(&changes), [second.as_str()]);
    let (changes, next) = delta_round(&graph, &in_inbox, &page).await;
    in_inbox = next;
    assert_eq!(changes, vec![removed(&first)]);

    let back = graph
        .json(
            Method::POST,
            &format!("/v1.0/me/messages/{second}/move"),
            json!({ "destinationId": "inbox" }),
        )
        .await;
    assert_eq!(back.status, 201, "{}", back.json);
    let third = back.json["id"].as_str().unwrap().to_owned();
    assert_ne!(third, second);
    let labels = gmail_labels(&graph, &back.json).await;
    assert!(
        labels.contains("INBOX") && !labels.contains("Label_9"),
        "{labels:?}"
    );
    assert_error(
        &graph.get(&format!("/v1.0/me/messages/{second}")).await,
        404,
        "ErrorItemNotFound",
    );
    let (changes, next) = delta_round(&graph, &in_receipts, &page).await;
    in_receipts = next;
    assert_eq!(changes, vec![removed(&second)]);
    let (changes, next) = delta_round(&graph, &in_inbox, &page).await;
    in_inbox = next;
    assert_eq!(ids(&changes), [third.as_str()]);

    // With immutable IDs, it keeps one throughout.
    let fixed_page = [IMMUTABLE, ("Prefer", "odata.maxpagesize=50")];
    let (_, mut receipts_fixed) = delta_round(
        &graph,
        &format!("/v1.0/me/mailFolders/{receipts}/messages/delta"),
        &fixed_page,
    )
    .await;
    let (_, mut inbox_fixed) = delta_round(
        &graph,
        "/v1.0/me/mailFolders/inbox/messages/delta",
        &fixed_page,
    )
    .await;
    let fixed = graph
        .get_with(&format!("/v1.0/me/messages/{third}"), &[IMMUTABLE])
        .await
        .json["id"]
        .as_str()
        .unwrap()
        .to_owned();
    for (target, into_receipts) in [(receipts.as_str(), true), ("inbox", false)] {
        let moved = graph
            .json_with(
                Method::POST,
                &format!("/v1.0/me/messages/{fixed}/move"),
                &[IMMUTABLE],
                json!({ "destinationId": target }),
            )
            .await;
        assert_eq!(moved.json["id"], fixed.as_str(), "{}", moved.json);
        let (to_receipts, next) = delta_round(&graph, &receipts_fixed, &fixed_page).await;
        receipts_fixed = next;
        let (to_inbox, next) = delta_round(&graph, &inbox_fixed, &fixed_page).await;
        inbox_fixed = next;
        let (arrived, left) = if into_receipts {
            (to_receipts, to_inbox)
        } else {
            (to_inbox, to_receipts)
        };
        assert_eq!(ids(&arrived), [fixed.as_str()]);
        assert_eq!(left, vec![removed(&fixed)]);
    }
    let _ = (in_receipts, in_inbox);

    // Archiving in Gmail takes the Inbox's newsletter to its folder.
    let newsletters = child(&graph, "Newsletters").await;
    let (_, in_newsletters) = delta_round(
        &graph,
        &format!("/v1.0/me/mailFolders/{newsletters}/messages/delta?$select=subject"),
        &page,
    )
    .await;
    let latest = graph
        .first(
            "inbox",
            "from/emailAddress/address eq 'newsletter@freightsignals.com'",
        )
        .await;
    let gmail = gmail_id(&graph, &latest).await;
    graph
        .json(
            Method::POST,
            &format!("/gmail/v1/users/me/messages/{gmail}/modify"),
            json!({ "removeLabelIds": ["INBOX"] }),
        )
        .await;
    let (changes, _) = delta_round(&graph, &in_newsletters, &page).await;
    assert_eq!(changes.len(), 1, "{changes:#?}");
    assert_eq!(changes[0]["subject"], latest["subject"]);
    assert_eq!(
        folder(&graph, &newsletters).await["totalItemCount"],
        gmail_count(&graph, "label:Newsletters").await
    );
}

#[tokio::test]
async fn a_users_own_mailbox_through_users() {
    let graph = Graph::start().await;
    let me = graph.get("/v1.0/me").await.json;
    let id = me["id"].as_str().unwrap();
    let mine = graph
        .get("/v1.0/me/mailFolders/inbox/messages?$top=3&$select=subject")
        .await
        .json;
    for user in [id, "maya@brightmoor.co", "Maya@Brightmoor.co"] {
        let theirs = graph
            .get(&format!(
                "/v1.0/users/{user}/mailFolders/inbox/messages?$top=3&$select=subject"
            ))
            .await;
        assert_eq!(theirs.status, 200, "{user}: {}", theirs.json);
        assert_eq!(theirs.json["value"], mine["value"], "{user}");
    }
    let settings = graph
        .get(&format!("/v1.0/users/{id}/mailboxSettings/timeZone"))
        .await;
    assert_eq!(settings.status, 200, "{}", settings.json);
    assert_error(
        &graph.get("/v1.0/users/priya@brightmoor.co/messages").await,
        403,
        "ErrorAccessDenied",
    );
    assert_error(
        &graph
            .get("/v1.0/users/nobody@brightmoor.co/mailFolders/inbox")
            .await,
        404,
        "ErrorInvalidUser",
    );
    assert_eq!(
        graph.get("/v1.0/users/priya@brightmoor.co").await.status,
        200,
        "the directory still answers"
    );
    let batch = graph
        .json(
            Method::POST,
            "/v1.0/$batch",
            json!({ "requests": [
                { "id": "1", "method": "GET", "url": "/users/maya@brightmoor.co/mailFolders/inbox" },
                { "id": "2", "method": "GET", "url": "/users/priya@brightmoor.co/mailFolders/inbox" },
            ]}),
        )
        .await;
    let responses = batch.json["responses"].as_array().unwrap();
    assert_eq!(
        (
            responses[0]["status"].as_u64(),
            responses[1]["status"].as_u64()
        ),
        (Some(200), Some(403))
    );
}

#[tokio::test]
async fn copies_land_beside_the_original() {
    let graph = Graph::start().await;
    let original = graph.first("inbox", "isRead eq false").await;
    let id = original["id"].as_str().unwrap();
    graph
        .json(
            Method::PATCH,
            &format!("/v1.0/me/messages/{id}"),
            json!({ "flag": { "flagStatus": "complete" } }),
        )
        .await;
    let copy = graph
        .json(
            Method::POST,
            &format!("/v1.0/me/messages/{id}/copy"),
            json!({ "destinationId": "archive" }),
        )
        .await;
    assert_eq!(copy.status, 201, "{}", copy.json);
    assert_ne!(copy.json["id"], original["id"]);
    assert_eq!(
        copy.json["parentFolderId"],
        folder(&graph, "archive").await["id"]
    );
    assert_eq!(
        copy.json["conversationId"], original["conversationId"],
        "the same conversation"
    );
    assert_eq!(copy.json["isRead"], false);
    assert_eq!(copy.json["flag"]["flagStatus"], "complete");
    assert_eq!(copy.json["subject"], original["subject"]);
    assert_eq!(
        graph.get(&format!("/v1.0/me/messages/{id}")).await.json["parentFolderId"],
        folder(&graph, "inbox").await["id"],
        "the original stays"
    );
    let rfc822 = original["internetMessageId"]
        .as_str()
        .unwrap()
        .trim_matches(['<', '>']);
    assert_eq!(
        gmail_count(&graph, &format!("rfc822msgid:{rfc822}")).await,
        2
    );

    let refused = graph
        .json(
            Method::POST,
            &format!("/v1.0/me/messages/{id}/copy"),
            json!({ "destinationId": "drafts" }),
        )
        .await;
    assert_error(&refused, 400, "ErrorMoveCopyFailed");
    let draft = graph.first("drafts", "isDraft eq true").await;
    let draft_copy = graph
        .json(
            Method::POST,
            &format!("/v1.0/me/messages/{}/copy", draft["id"].as_str().unwrap()),
            json!({ "destinationId": "inbox" }),
        )
        .await;
    assert_error(&draft_copy, 400, "ErrorMoveCopyFailed");
}

#[tokio::test]
async fn ids_translate_between_formats() {
    let graph = Graph::start().await;
    let message = graph.first("inbox", "isRead eq true").await;
    let rest = message["id"].as_str().unwrap().to_owned();
    let immutable = graph
        .get_with(&format!("/v1.0/me/messages/{rest}"), &[IMMUTABLE])
        .await
        .json["id"]
        .as_str()
        .unwrap()
        .to_owned();
    let translate = |ids: Vec<&str>, from: &str, to: &str| {
        let graph = &graph;
        let body = json!({ "inputIds": ids, "sourceIdType": from, "targetIdType": to });
        async move {
            graph
                .json(Method::POST, "/v1.0/me/translateExchangeIds", body)
                .await
        }
    };
    let to_immutable = translate(vec![&rest], "restId", "restImmutableEntryId").await;
    assert_eq!(to_immutable.status, 200, "{}", to_immutable.json);
    assert_eq!(
        to_immutable.json["value"],
        json!([{ "targetId": immutable, "sourceId": rest }])
    );
    let back = translate(vec![&immutable], "restImmutableEntryId", "restId").await;
    assert_eq!(back.json["value"][0]["targetId"], rest.as_str());
    let entry = translate(vec![&rest], "restId", "entryId").await.json["value"][0]["targetId"]
        .as_str()
        .unwrap()
        .to_owned();
    assert!(
        entry.ends_with(['0', '1', '2']) && !entry.contains(['+', '/', '=']),
        "binary IDs are URL-safe with a padding count: {entry}"
    );
    for format in ["ewsId", "entryId", "immutableEntryId"] {
        let there = translate(vec![&rest], "restId", format).await.json["value"][0]["targetId"]
            .as_str()
            .unwrap()
            .to_owned();
        let again = translate(vec![&there], format, "restId").await;
        assert_eq!(
            again.json["value"][0]["targetId"],
            rest.as_str(),
            "{format}: {there}"
        );
    }

    let moved = graph
        .json(
            Method::POST,
            &format!("/v1.0/me/messages/{rest}/move"),
            json!({ "destinationId": "archive" }),
        )
        .await;
    let current = moved.json["id"].as_str().unwrap();
    let after = translate(
        vec![&immutable, &rest, "nope"],
        "restImmutableEntryId",
        "restId",
    )
    .await;
    let results = after.json["value"].as_array().unwrap();
    assert_eq!(
        results[0]["targetId"], current,
        "an immutable ID finds the moved message"
    );
    assert_eq!(
        results[1]["errorDetails"]["code"], "ErrorItemNotFound",
        "an old default ID names nothing"
    );
    assert_eq!(
        results[2]["errorDetails"]["code"],
        "ErrorInvalidIdMalformed"
    );
    assert_error(
        &translate(vec![&rest], "restId", "mapiId").await,
        400,
        "BadRequest",
    );
}

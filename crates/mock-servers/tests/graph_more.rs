//! Graph's settings over Gmail's, `$batch`, throttling, and people.

mod graph;

use std::time::Duration;

use graph::{Graph, assert_error, recipients, urlencode};
use reqwest::Method;
use serde_json::{Value, json};

#[tokio::test]
async fn automatic_replies_are_the_vacation_responder() {
    let graph = Graph::start().await;
    let settings = graph.get("/v1.0/me/mailboxSettings").await.json;
    assert_eq!(settings["timeZone"], "Eastern Standard Time");
    let replies = &settings["automaticRepliesSetting"];
    assert_eq!(replies["status"], "disabled");
    assert!(
        replies["internalReplyMessage"]
            .as_str()
            .unwrap()
            .contains("Thanksgiving")
    );
    let only = graph
        .get("/v1.0/me/mailboxSettings/automaticRepliesSetting")
        .await;
    assert_eq!(only.json["status"], "disabled", "{}", only.json);
    let zone = graph.get("/v1.0/me/mailboxSettings/timeZone").await;
    assert_eq!(zone.json["value"], "Eastern Standard Time");

    let patched = graph
        .json(
            Method::PATCH,
            "/v1.0/me/mailboxSettings",
            json!({ "automaticRepliesSetting": {
                "status": "scheduled",
                "externalAudience": "contactsOnly",
                "internalReplyMessage": "<p>Back Monday.</p>",
                "externalReplyMessage": "<p>I'm away until Monday.</p>",
                "scheduledStartDateTime": { "dateTime": "2026-11-26T00:00:00", "timeZone": "Eastern Standard Time" },
                "scheduledEndDateTime": { "dateTime": "2026-11-30T00:00:00", "timeZone": "UTC" },
            }}),
        )
        .await;
    assert_eq!(patched.status, 200, "{}", patched.json);
    assert_eq!(
        patched
            .json
            .as_object()
            .unwrap()
            .keys()
            .filter(|k| !k.starts_with('@'))
            .count(),
        1,
        "the reply names what changed"
    );
    let vacation = graph.get("/gmail/v1/users/me/settings/vacation").await.json;
    assert_eq!(vacation["enableAutoReply"], true);
    assert_eq!(vacation["restrictToContacts"], true);
    assert_eq!(vacation["responseBodyHtml"], "<p>Back Monday.</p>");
    assert_eq!(
        vacation["startTime"], "1795669200000",
        "midnight in New York"
    );
    let replies =
        graph.get("/v1.0/me/mailboxSettings").await.json["automaticRepliesSetting"].clone();
    assert_eq!(
        replies["externalReplyMessage"],
        "<p>I'm away until Monday.</p>"
    );
    assert_eq!(
        replies["scheduledEndDateTime"]["dateTime"],
        "2026-11-30T00:00:00.0000000"
    );

    let other = graph
        .json(
            Method::PATCH,
            "/v1.0/me/mailboxSettings",
            json!({ "timeFormat": "HH:mm" }),
        )
        .await;
    assert_eq!(other.json["timeFormat"], "HH:mm");
    assert_error(
        &graph
            .json(
                Method::PATCH,
                "/v1.0/me/mailboxSettings",
                json!({ "timezone": "UTC" }),
            )
            .await,
        400,
        "RequestBodyRead",
    );
}

#[tokio::test]
async fn rules_are_gmail_filters() {
    let graph = Graph::start().await;
    let rules = graph
        .get("/v1.0/me/mailFolders/inbox/messageRules")
        .await
        .json;
    let filters = graph.get("/gmail/v1/users/me/settings/filters").await.json;
    assert_eq!(
        rules["value"].as_array().unwrap().len(),
        filters["filter"].as_array().unwrap().len()
    );
    let skip_inbox = rules["value"]
        .as_array()
        .unwrap()
        .iter()
        .find(|r| r["displayName"] == "From notifications@linkwise.com")
        .unwrap();
    assert_eq!(
        skip_inbox["actions"]["moveToFolder"],
        graph.get("/v1.0/me/mailFolders/archive").await.json["id"]
    );

    let created = graph
        .json(
            Method::POST,
            "/v1.0/me/mailFolders/inbox/messageRules",
            json!({ "displayName": "Hollis", "sequence": 1, "isEnabled": true, "conditions": { "senderContains": ["hollisconf.com"] }, "actions": { "assignCategories": ["Travel"], "markAsRead": true } }),
        )
        .await;
    assert_eq!(created.status, 201, "{}", created.json);
    let rule = created.json["id"].as_str().unwrap().to_owned();
    graph.admin("/__mock/deliver", json!({ "from": "Hollis Conference <speakers@hollisconf.com>", "subject": "Your session slot", "text": "Thursday 2pm." })).await;
    let filter = urlencode("subject eq 'Your session slot'");
    let delivered = graph
        .get(&format!("/v1.0/me/messages?$filter={filter}"))
        .await
        .json["value"][0]
        .clone();
    assert_eq!(delivered["categories"], json!(["Travel"]), "{delivered:#}");
    assert_eq!(delivered["isRead"], true);

    let disabled = graph
        .json(
            Method::PATCH,
            &format!("/v1.0/me/mailFolders/inbox/messageRules/{rule}"),
            json!({ "isEnabled": false }),
        )
        .await;
    assert_eq!(disabled.json["isEnabled"], false, "{}", disabled.json);
    assert_eq!(disabled.json["displayName"], "Hollis");
    let filters = graph.get("/gmail/v1/users/me/settings/filters").await.json;
    assert!(
        filters["filter"]
            .as_array()
            .unwrap()
            .iter()
            .all(|f| f["criteria"]["from"] != "hollisconf.com"),
        "a disabled rule's filter is off Gmail's list"
    );
    graph.admin("/__mock/deliver", json!({ "from": "speakers@hollisconf.com", "subject": "Travel details", "text": "Hotel." })).await;
    let filter = urlencode("subject eq 'Travel details'");
    let unfiltered = graph
        .get(&format!("/v1.0/me/messages?$filter={filter}"))
        .await
        .json["value"][0]
        .clone();
    assert_eq!(unfiltered["categories"], json!([]));
    assert_eq!(
        graph
            .request(
                Method::DELETE,
                &format!("/v1.0/me/mailFolders/inbox/messageRules/{rule}"),
                &[],
                None
            )
            .await
            .status,
        204
    );
    assert_error(
        &graph
            .get(&format!("/v1.0/me/mailFolders/inbox/messageRules/{rule}"))
            .await,
        404,
        "ErrorItemNotFound",
    );
}

#[tokio::test]
async fn overrides_classify_future_mail() {
    let graph = Graph::start().await;
    let made = graph
        .json(Method::POST, "/v1.0/me/inferenceClassification/overrides", json!({ "classifyAs": "other", "senderEmailAddress": { "address": "tom@brightmoor.co" } }))
        .await;
    assert_eq!(made.status, 201, "{}", made.json);
    let again = graph
        .json(Method::POST, "/v1.0/me/inferenceClassification/overrides", json!({ "classifyAs": "other", "senderEmailAddress": { "name": "Tom", "address": "TOM@brightmoor.co" } }))
        .await;
    assert_eq!(again.json["id"], made.json["id"], "one override per sender");
    let filter = urlencode("from/emailAddress/address eq 'tom@brightmoor.co'");
    let before = graph
        .get(&format!(
            "/v1.0/me/mailFolders/inbox/messages?$filter={filter}"
        ))
        .await
        .json;
    assert!(
        before["value"]
            .as_array()
            .unwrap()
            .iter()
            .all(|m| m["inferenceClassification"] == "focused"),
        "existing mail stays"
    );
    graph.admin("/__mock/deliver", json!({ "from": "Tom Becker <tom@brightmoor.co>", "subject": "Crit notes", "text": "See you." })).await;
    let after = graph.get(&format!("/v1.0/me/mailFolders/inbox/messages?$filter={filter}&$orderby=from/emailAddress/address")).await;
    assert_eq!(after.status, 200, "{}", after.json);
    let crit = after.json["value"]
        .as_array()
        .unwrap()
        .iter()
        .find(|m| m["subject"] == "Crit notes")
        .unwrap()
        .clone();
    assert_eq!(crit["inferenceClassification"], "other");
    let id = made.json["id"].as_str().unwrap();
    assert_eq!(
        graph
            .json(
                Method::PATCH,
                &format!("/v1.0/me/inferenceClassification/overrides/{id}"),
                json!({ "classifyAs": "focused" })
            )
            .await
            .json["classifyAs"],
        "focused"
    );
    assert_eq!(
        graph
            .request(
                Method::DELETE,
                &format!("/v1.0/me/inferenceClassification/overrides/{id}"),
                &[],
                None
            )
            .await
            .status,
        204
    );
    assert_eq!(
        graph
            .get("/v1.0/me/inferenceClassification/overrides")
            .await
            .json["value"],
        json!([])
    );
}

#[tokio::test]
async fn master_categories_are_labels() {
    let graph = Graph::start().await;
    let list = graph.get("/v1.0/me/outlook/masterCategories").await.json;
    let names: Vec<&str> = list["value"]
        .as_array()
        .unwrap()
        .iter()
        .map(|c| c["displayName"].as_str().unwrap())
        .collect();
    assert!(
        names.contains(&"Projects/Atlas Launch") && names.contains(&"Read later"),
        "{names:?}"
    );
    let made = graph
        .json(
            Method::POST,
            "/v1.0/me/outlook/masterCategories",
            json!({ "displayName": "Investors", "color": "preset1" }),
        )
        .await;
    assert_eq!(made.json["color"], "preset1", "{}", made.json);
    let labels = graph.get("/gmail/v1/users/me/labels").await.json;
    let label = labels["labels"]
        .as_array()
        .unwrap()
        .iter()
        .find(|l| l["name"] == "Investors")
        .unwrap();
    assert!(label["color"]["backgroundColor"].is_string());
    assert_error(
        &graph
            .json(
                Method::POST,
                "/v1.0/me/outlook/masterCategories",
                json!({ "displayName": "investors" }),
            )
            .await,
        409,
        "Conflict",
    );
    let id = made.json["id"].as_str().unwrap();
    let recolored = graph
        .json(
            Method::PATCH,
            &format!("/v1.0/me/outlook/masterCategories/{id}"),
            json!({ "color": "preset7" }),
        )
        .await;
    assert_eq!(recolored.json["color"], "preset7");
    assert_error(
        &graph
            .json(
                Method::PATCH,
                &format!("/v1.0/me/outlook/masterCategories/{id}"),
                json!({ "displayName": "Funds" }),
            )
            .await,
        400,
        "ErrorInvalidRequest",
    );

    let message = graph.first("inbox", "isRead eq true").await;
    let path = format!("/v1.0/me/messages/{}", message["id"].as_str().unwrap());
    graph
        .json(Method::PATCH, &path, json!({ "categories": ["Investors"] }))
        .await;
    assert_eq!(
        graph
            .request(
                Method::DELETE,
                &format!("/v1.0/me/outlook/masterCategories/{id}"),
                &[],
                None
            )
            .await
            .status,
        204
    );
    assert_eq!(
        graph.get(&path).await.json["categories"],
        json!(["Investors"]),
        "messages keep a category taken off the list"
    );
    let list = graph.get("/v1.0/me/outlook/masterCategories").await.json;
    assert!(
        list["value"]
            .as_array()
            .unwrap()
            .iter()
            .all(|c| c["displayName"] != "Investors")
    );
}

#[tokio::test]
async fn batches_run_in_order_and_fail_their_dependents() {
    let graph = Graph::start().await;
    let message = graph.first("inbox", "isRead eq false").await;
    let id = message["id"].as_str().unwrap();
    let batch = graph
        .json(
            Method::POST,
            "/v1.0/$batch",
            json!({ "requests": [
                { "id": "read", "method": "PATCH", "url": format!("/me/messages/{id}"), "headers": { "Content-Type": "application/json" }, "body": { "isRead": true } },
                { "id": "check", "dependsOn": ["read"], "method": "GET", "url": format!("/me/messages/{id}?$select=isRead") },
                { "id": "missing", "method": "GET", "url": "/me/messages/nope" },
                { "id": "after", "dependsOn": ["missing"], "method": "GET", "url": "/me" },
                { "id": "mime", "method": "GET", "url": format!("me/messages/{id}/$value") },
            ]}),
        )
        .await;
    assert_eq!(batch.status, 200, "{}", batch.json);
    let responses = batch.json["responses"].as_array().unwrap();
    let by_id = |name: &str| responses.iter().find(|r| r["id"] == name).unwrap().clone();
    assert_eq!(by_id("read")["status"], 200);
    assert_eq!(by_id("check")["body"]["isRead"], true);
    assert_eq!(by_id("missing")["status"], 400);
    assert_eq!(
        by_id("missing")["body"]["error"]["code"],
        "ErrorInvalidIdMalformed"
    );
    assert_eq!(by_id("after")["status"], 424);
    let mime = by_id("mime");
    assert!(
        mime["body"].is_string(),
        "non-JSON bodies come base64-encoded"
    );
    assert!(
        by_id("check")["headers"]["Content-Type"]
            .as_str()
            .unwrap()
            .starts_with("application/json")
    );

    let too_many: Vec<Value> = (0..21)
        .map(|n| json!({ "id": n.to_string(), "method": "GET", "url": "/me" }))
        .collect();
    assert_error(
        &graph
            .json(
                Method::POST,
                "/v1.0/$batch",
                json!({ "requests": too_many }),
            )
            .await,
        400,
        "BadRequest",
    );
    let no_type = graph.json(Method::POST, "/v1.0/$batch", json!({ "requests": [{ "id": "1", "method": "POST", "url": "/me/messages", "body": {} }] })).await;
    assert_error(&no_type, 400, "BadRequest");

    graph.admin("/__mock/faults", json!({ "path": "/v1.0/me/mailFolders/inbox", "status": 429, "retryAfter": 7, "times": 1 })).await;
    let throttled = graph
        .json(
            Method::POST,
            "/v1.0/$batch",
            json!({ "requests": [
                { "id": "1", "method": "GET", "url": "/me/mailFolders/inbox" },
                { "id": "2", "method": "GET", "url": "/me/mailFolders/inbox" },
            ]}),
        )
        .await;
    let responses = throttled.json["responses"].as_array().unwrap();
    assert_eq!(responses[0]["status"], 429);
    assert_eq!(responses[0]["headers"]["Retry-After"], "7");
    assert_eq!(
        responses[1]["status"], 200,
        "each request in a batch is throttled on its own"
    );
}

#[tokio::test]
async fn throttling_and_faults() {
    let graph = Graph::start().await;
    graph
        .admin(
            "/__mock/faults",
            json!({ "path": "/v1.0/me/messages", "status": 429, "retryAfter": 3 }),
        )
        .await;
    let throttled = graph.get("/v1.0/me/messages").await;
    assert_error(&throttled, 429, "TooManyRequests");
    assert_eq!(throttled.headers["retry-after"], "3");
    assert_eq!(
        graph.get("/v1.0/me/messages").await.status,
        200,
        "the fault fires once"
    );

    let message = graph.first("inbox", "isRead eq false").await;
    let id = message["id"].as_str().unwrap();
    graph.admin("/__mock/faults", json!({ "path": format!("/v1.0/me/messages/{id}"), "method": "PATCH", "status": 503, "after": true })).await;
    let lost = graph
        .json(
            Method::PATCH,
            &format!("/v1.0/me/messages/{id}"),
            json!({ "isRead": true }),
        )
        .await;
    assert_error(&lost, 503, "ServiceUnavailable");
    assert_eq!(
        graph.get(&format!("/v1.0/me/messages/{id}")).await.json["isRead"],
        true,
        "the change was made, only its answer lost"
    );

    // Outlook's limit on concurrent requests: while one request's body is still arriving,
    // another is refused.
    let limited = Graph::with(mock_servers::Config {
        graph_concurrency: Some(1),
        ..mock_servers::Config::default()
    })
    .await;
    let (send, receive) = tokio::sync::mpsc::channel::<Result<Vec<u8>, std::io::Error>>(1);
    let stream = futures_stream(receive);
    let slow = limited
        .http
        .post(limited.url("/v1.0/me/sendMail"))
        .bearer_auth(&limited.token)
        .header("Content-Type", "application/json")
        .body(reqwest::Body::wrap_stream(stream))
        .send();
    let slow = tokio::spawn(slow);
    send.send(Ok(b"{\"message\": ".to_vec())).await.unwrap();
    tokio::time::sleep(Duration::from_millis(150)).await;
    let refused = limited.get("/v1.0/me").await;
    assert_error(&refused, 429, "ApplicationThrottled");
    assert!(refused.headers.contains_key("retry-after"));
    let rest = json!({ "subject": "Slow", "toRecipients": recipients(&["sam@brightmoor.co"]) })
        .to_string()
        + "}";
    send.send(Ok(rest.into_bytes())).await.unwrap();
    drop(send);
    assert_eq!(slow.await.unwrap().unwrap().status(), 202);
    assert_eq!(
        limited.get("/v1.0/me").await.status,
        200,
        "the slot is free again"
    );
}

/// A body stream fed from a channel.
fn futures_stream(
    mut receive: tokio::sync::mpsc::Receiver<Result<Vec<u8>, std::io::Error>>,
) -> impl futures::Stream<Item = Result<Vec<u8>, std::io::Error>> {
    futures::stream::poll_fn(move |context| receive.poll_recv(context))
}

#[tokio::test]
async fn people_users_and_contacts() {
    let graph = Graph::start().await;
    let people = graph.get("/v1.0/me/people?$top=5").await.json;
    let top = people["value"].as_array().unwrap();
    assert_eq!(top.len(), 5);
    let scores: Vec<f64> = top
        .iter()
        .map(|p| {
            p["scoredEmailAddresses"][0]["relevanceScore"]
                .as_f64()
                .unwrap()
        })
        .collect();
    assert!(
        scores.windows(2).all(|pair| pair[0] >= pair[1]),
        "{scores:?}"
    );
    assert!(
        people["@odata.nextLink"]
            .as_str()
            .unwrap()
            .contains("$skip=5")
    );
    let found = graph.get("/v1.0/me/people?$search=%22rach%22").await.json;
    assert_eq!(found["value"][0]["displayName"], "Rachel Kim");
    assert_eq!(
        found["value"][0]["personType"]["subclass"],
        "PersonalContact"
    );

    let users = graph.get("/v1.0/users?$count=true").await.json;
    assert_eq!(
        users["@odata.count"], 15,
        "Maya and her fourteen colleagues"
    );
    let me = graph.get("/v1.0/me").await.json;
    assert!(
        users["value"]
            .as_array()
            .unwrap()
            .iter()
            .any(|u| u["id"] == me["id"]),
        "/me is in the directory"
    );
    let filtered = graph
        .get(&format!(
            "/v1.0/users?$filter={}",
            urlencode("startswith(displayName,'pri')")
        ))
        .await
        .json;
    assert_eq!(filtered["value"][0]["mail"], "priya@brightmoor.co");

    let (all, last) = graph
        .pages("/v1.0/users/delta", &[("Prefer", "odata.maxpagesize=4")])
        .await;
    assert_eq!(all.len(), 15);
    let mut link = last["@odata.deltaLink"].as_str().unwrap().to_owned();
    let noor = all
        .iter()
        .find(|u| u["mail"] == "noor@brightmoor.co")
        .unwrap()["id"]
        .clone();
    graph
        .admin(
            "/__mock/people",
            json!({ "list": "directory", "remove": "noor@brightmoor.co" }),
        )
        .await;
    let (changes, last) = graph.pages(&link, &[]).await;
    link = last["@odata.deltaLink"].as_str().unwrap().to_owned();
    assert_eq!(
        changes,
        vec![json!({ "id": noor, "@removed": { "reason": "changed" } })]
    );
    let latest = graph.get("/v1.0/users/delta?$deltatoken=latest").await.json;
    assert_eq!(latest["value"], json!([]));
    assert!(latest["@odata.deltaLink"].is_string());
    graph
        .admin("/__mock/people", json!({ "expire": true }))
        .await;
    assert_error(&graph.get(&link).await, 410, "SyncStateNotFound");

    let contacts = graph.get("/v1.0/me/contacts?$top=100").await.json;
    let saved = contacts["value"].as_array().unwrap();
    assert!(
        saved
            .iter()
            .any(|c| c["emailAddresses"][0]["address"] == "rachel.kim@callowayfreight.com")
    );
    let (initial, last) = graph.pages("/v1.0/me/contacts/delta", &[]).await;
    assert_eq!(initial.len(), saved.len());
    let link = last["@odata.deltaLink"].as_str().unwrap().to_owned();
    graph
        .admin(
            "/__mock/people",
            json!({ "list": "contacts", "remove": "rachel.kim@callowayfreight.com" }),
        )
        .await;
    let (changes, _) = graph.pages(&link, &[]).await;
    assert_eq!(changes.len(), 1);
    assert_eq!(changes[0]["@removed"]["reason"], "deleted");
    let folders = graph.get("/v1.0/me/contactFolders").await.json;
    assert_eq!(
        folders["value"],
        json!([]),
        "the default Contacts folder has no subfolders"
    );
}

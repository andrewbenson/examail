//! Meeting mail as Outlook's `eventMessage`s: invites from the world's calendar, and requests,
//! responses, counter-proposals and cancellations as they arrive.

mod graph;

use base64::Engine;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use graph::{Graph, Metadata, assert_error, urlencode};
use serde_json::{Value, json};

/// The first invite in the Inbox, with its event expanded.
async fn invite(graph: &Graph) -> Value {
    let reply = graph
        .get(&format!(
            "/v1.0/me/mailFolders/inbox/messages?$search={}&$expand=microsoft.graph.eventMessage/event",
            urlencode("\"subject:Invitation\"")
        ))
        .await;
    assert_eq!(reply.status, 200, "{}", reply.json);
    reply.json["value"][0].clone()
}

/// Delivers a meeting message with an iTIP part for `method`.
async fn deliver(graph: &Graph, from: &str, subject: &str, method: &str, event: &str) -> Value {
    let raw = format!(
        "From: {from}\r\nTo: Maya Okafor <maya@brightmoor.co>\r\nSubject: {subject}\r\nMessage-ID: <{}@example.com>\r\nMIME-Version: 1.0\r\nContent-Type: multipart/alternative; boundary=\"itip\"\r\n\r\n--itip\r\nContent-Type: text/plain; charset=utf-8\r\n\r\n{subject}\r\n--itip\r\nContent-Type: text/calendar; method={method}; charset=utf-8\r\n\r\nBEGIN:VCALENDAR\r\nMETHOD:{method}\r\nBEGIN:VEVENT\r\n{event}\r\nEND:VEVENT\r\nEND:VCALENDAR\r\n--itip--\r\n",
        urlencode(subject).replace('%', "")
    );
    let status = graph
        .admin(
            "/__mock/deliver",
            json!({ "raw": URL_SAFE_NO_PAD.encode(raw) }),
        )
        .await;
    assert_eq!(status, 200);
    let filter = urlencode(&format!("subject eq '{subject}'"));
    let reply = graph
        .get(&format!("/v1.0/me/messages?$filter={filter}"))
        .await;
    reply.json["value"][0].clone()
}

#[tokio::test]
async fn invites_are_meeting_requests() {
    let graph = Graph::start().await;
    let metadata = Metadata::load();
    let message = invite(&graph).await;
    metadata.assert("message", &message);
    assert_eq!(
        message["@odata.type"],
        "#microsoft.graph.eventMessageRequest"
    );
    assert_eq!(message["meetingMessageType"], "meetingRequest");
    assert_eq!(message["meetingRequestType"], "newMeetingRequest");
    assert_eq!(message["type"], "singleInstance");
    assert_eq!(message["responseRequested"], true);
    assert_eq!(message["isOutOfDate"], false);
    let event = &message["event"];
    assert_eq!(
        event["start"], message["startDateTime"],
        "the request's times are the event's"
    );
    assert_eq!(event["end"], message["endDateTime"]);
    assert_eq!(
        event["location"]["displayName"],
        message["location"]["displayName"]
    );
    assert!(
        message["subject"]
            .as_str()
            .unwrap()
            .contains(event["subject"].as_str().unwrap())
    );
    assert_eq!(
        event["responseStatus"]["response"], "none",
        "not answered yet"
    );
    assert_eq!(event["showAs"], "tentative");

    // The same mail through Gmail carries the iCalendar part the request came from.
    let rfc822 = message["internetMessageId"]
        .as_str()
        .unwrap()
        .trim_matches(['<', '>']);
    let found = graph
        .get(&format!(
            "/gmail/v1/users/me/messages?q={}",
            urlencode(&format!("rfc822msgid:{rfc822}"))
        ))
        .await
        .json;
    let id = found["messages"][0]["id"].as_str().unwrap();
    let raw = graph
        .get(&format!("/gmail/v1/users/me/messages/{id}?format=raw"))
        .await
        .json;
    let raw = gmail::wire::decode_base64url(raw["raw"].as_str().unwrap()).unwrap();
    let text = String::from_utf8_lossy(&raw);
    assert!(text.contains("METHOD:REQUEST"));
    assert!(text.contains(event["iCalUId"].as_str().unwrap()));

    // `$select` shows a meeting's own properties only when cast to its type.
    let path = |select: &str| {
        format!(
            "/v1.0/me/messages/{}?$select={select}",
            message["id"].as_str().unwrap()
        )
    };
    let plain = graph.get(&path("subject")).await.json;
    assert_eq!(plain["@odata.type"], "#microsoft.graph.eventMessageRequest");
    assert!(plain.get("meetingMessageType").is_none());
    let cast = graph
        .get(&path("subject,microsoft.graph.eventMessage/meetingMessageType,microsoft.graph.eventMessageRequest/meetingRequestType"))
        .await
        .json;
    assert_eq!(
        (
            cast["meetingMessageType"].as_str(),
            cast["meetingRequestType"].as_str()
        ),
        (Some("meetingRequest"), Some("newMeetingRequest"))
    );
    assert!(cast.get("startDateTime").is_none());
    assert_error(
        &graph
            .get(&path("microsoft.graph.eventMessage/meetingKind"))
            .await,
        400,
        "RequestBroker--ParseUri",
    );

    // Ordinary messages carry neither a meeting type nor its properties.
    let inbox = graph
        .get("/v1.0/me/mailFolders/inbox/messages?$top=50")
        .await
        .json;
    let ordinary: Vec<&Value> = inbox["value"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|m| m.get("@odata.type").is_none())
        .collect();
    assert!(ordinary.len() > 10);
    assert!(
        ordinary
            .iter()
            .all(|m| m.get("meetingMessageType").is_none() && m.get("event").is_none())
    );

    // Outlook's time zone preference applies to a request's times.
    let pacific = graph
        .get_with(
            &format!("/v1.0/me/messages/{}", message["id"].as_str().unwrap()),
            &[("Prefer", "outlook.timezone=\"Pacific Standard Time\"")],
        )
        .await
        .json;
    assert_eq!(
        pacific["startDateTime"]["timeZone"],
        "Pacific Standard Time"
    );
    assert_ne!(
        pacific["startDateTime"]["dateTime"],
        message["startDateTime"]["dateTime"]
    );
}

#[tokio::test]
async fn responses_counters_and_cancellations() {
    let graph = Graph::start().await;
    let metadata = Metadata::load();
    let request = invite(&graph).await;
    let uid = request["event"]["iCalUId"].as_str().unwrap().to_owned();
    let times = "DTSTART:20261125T140000Z\r\nDTEND:20261125T144500Z";

    let accepted = deliver(
        &graph,
        "Grace Liu <grace@juniperpartners.vc>",
        "Accepted: Coffee with Grace",
        "REPLY",
        &format!(
            "UID:{uid}\r\n{times}\r\nATTENDEE;PARTSTAT=ACCEPTED:mailto:grace@juniperpartners.vc"
        ),
    )
    .await;
    metadata.assert("message", &accepted);
    assert_eq!(
        accepted["@odata.type"],
        "#microsoft.graph.eventMessageResponse"
    );
    assert_eq!(
        (
            accepted["meetingMessageType"].as_str(),
            accepted["responseType"].as_str()
        ),
        (Some("meetingAccepted"), Some("accepted"))
    );
    assert_eq!(accepted["proposedNewTime"], Value::Null);

    let declined = deliver(
        &graph,
        "grace@juniperpartners.vc",
        "Declined: Coffee with Grace",
        "REPLY",
        &format!(
            "UID:{uid}\r\n{times}\r\nATTENDEE;PARTSTAT=DECLINED:mailto:grace@juniperpartners.vc"
        ),
    )
    .await;
    assert_eq!(
        (
            declined["meetingMessageType"].as_str(),
            declined["responseType"].as_str()
        ),
        (Some("meetingDeclined"), Some("declined"))
    );

    let counter = deliver(
        &graph,
        "grace@juniperpartners.vc",
        "New Time Proposed: Coffee with Grace",
        "COUNTER",
        &format!("UID:{uid}\r\nDTSTART:20261126T140000Z\r\nDTEND:20261126T144500Z\r\nATTENDEE;PARTSTAT=TENTATIVE:mailto:grace@juniperpartners.vc"),
    )
    .await;
    metadata.assert("message", &counter);
    assert_eq!(counter["responseType"], "tentativelyAccepted");
    assert_eq!(
        counter["proposedNewTime"]["start"]["dateTime"],
        "2026-11-26T14:00:00.0000000"
    );

    let cancelled = deliver(
        &graph,
        "Daniel Reyes <daniel@brightmoor.co>",
        "Canceled: Board prep",
        "CANCEL",
        "UID:board-prep@example.com\r\nDTSTART;TZID=Eastern Standard Time:20261130T090000\r\nDTEND;TZID=Eastern Standard Time:20261130T100000\r\nRRULE:FREQ=WEEKLY;BYDAY=MO",
    )
    .await;
    metadata.assert("message", &cancelled);
    assert_eq!(cancelled["@odata.type"], "#microsoft.graph.eventMessage");
    assert_eq!(cancelled["meetingMessageType"], "meetingCancelled");
    assert_eq!(cancelled["type"], "seriesMaster");
    assert_eq!(
        cancelled["startDateTime"]["dateTime"], "2026-11-30T14:00:00.0000000",
        "Windows zones resolve"
    );
    assert_eq!(
        cancelled["recurrence"]["pattern"]["daysOfWeek"],
        json!(["monday"])
    );
    let expanded = graph
        .get(&format!(
            "/v1.0/me/messages/{}?$expand=microsoft.graph.eventMessage/event",
            cancelled["id"].as_str().unwrap()
        ))
        .await
        .json;
    assert_eq!(
        expanded["event"],
        Value::Null,
        "the calendar has no such event"
    );

    let update = deliver(
        &graph,
        "Grace Liu <grace@juniperpartners.vc>",
        "Updated invitation: Coffee with Grace",
        "REQUEST",
        &format!("UID:{uid}\r\nSEQUENCE:1\r\n{times}\r\nX-MICROSOFT-DISALLOW-COUNTER:TRUE\r\nATTENDEE;RSVP=FALSE:mailto:maya@brightmoor.co"),
    )
    .await;
    assert_eq!(update["meetingRequestType"], "fullUpdate");
    assert_eq!(update["allowNewTimeProposals"], false);
    assert_eq!(update["responseRequested"], false);
}

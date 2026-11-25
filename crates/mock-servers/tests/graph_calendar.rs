//! The calendar through Microsoft Graph: calendars, events, views, series and their instances,
//! writes that Google Calendar's API sees (and the other way round), online meetings, and the
//! calendar view's delta.

mod graph;

use graph::{Graph, Metadata, assert_error, urlencode};
use mock_servers::DEV_ACCESS_TOKEN;
use reqwest::Method;
use serde_json::{Value, json};

/// The week of the mock's "now" (Tuesday 17 November 2026, New York).
const WEEK: &str = "startDateTime=2026-11-16T00:00:00-05:00&endDateTime=2026-11-23T00:00:00-05:00";

async fn all(graph: &Graph, path: &str) -> Vec<Value> {
    graph.pages(path, &[]).await.0
}

fn subjects(items: &[Value]) -> Vec<&str> {
    items
        .iter()
        .map(|item| item["subject"].as_str().unwrap_or_default())
        .collect()
}

fn find<'a>(items: &'a [Value], subject: &str) -> &'a Value {
    items
        .iter()
        .find(|item| item["subject"] == subject)
        .unwrap_or_else(|| panic!("no {subject} in {:?}", subjects(items)))
}

/// A Google Calendar event by its summary, through Google's API.
async fn google(graph: &Graph, calendar: &str, summary: &str) -> Value {
    let reply = graph
        .get(&format!(
            "/calendar/v3/calendars/{calendar}/events?showDeleted=true&q={}",
            urlencode(summary)
        ))
        .await;
    assert_eq!(reply.status, 200, "{}", reply.json);
    reply.json["items"]
        .as_array()
        .unwrap()
        .iter()
        .find(|item| item["summary"] == summary)
        .cloned()
        .unwrap_or_else(|| panic!("Google has no {summary}"))
}

async fn calendar_id(graph: &Graph, name: &str) -> String {
    let calendars = graph.get("/v1.0/me/calendars").await.json;
    calendars["value"]
        .as_array()
        .unwrap()
        .iter()
        .find(|c| c["name"] == name)
        .and_then(|c| c["id"].as_str())
        .unwrap()
        .to_owned()
}

fn lunch() -> Value {
    json!({
        "subject": "Lunch with Ines",
        "body": { "contentType": "HTML", "content": "Does noon work?" },
        "start": { "dateTime": "2026-11-18T12:00:00", "timeZone": "Pacific Standard Time" },
        "end": { "dateTime": "2026-11-18T13:00:00", "timeZone": "Pacific Standard Time" },
        "location": { "displayName": "Harry's Bar" },
        "attendees": [{ "emailAddress": { "address": "ines@brightmoor.co", "name": "Ines" }, "type": "required" }],
        "categories": ["Red category"],
        "transactionId": "7E163156-7762-4BEB-A1C6-729EA81755A7",
    })
}

#[tokio::test]
async fn calendars_are_the_calendar_list() {
    let graph = Graph::start().await;
    let metadata = Metadata::load();
    let list = graph.get("/v1.0/me/calendars").await;
    assert_eq!(list.status, 200, "{}", list.json);
    metadata.assert_collection("calendar", &list.json);
    let calendars = list.json["value"].as_array().unwrap();
    let names: Vec<&str> = calendars
        .iter()
        .map(|c| c["name"].as_str().unwrap())
        .collect();
    assert_eq!(
        names,
        ["Calendar", "Brightmoor Team", "Holidays in United States"],
        "the primary first, the hidden Birthdays left out"
    );
    let primary = &calendars[0];
    assert_eq!(primary["isDefaultCalendar"], true);
    assert_eq!(primary["canEdit"], true);
    assert_eq!(primary["isRemovable"], false);
    assert_eq!(primary["owner"]["address"], "maya@brightmoor.co");
    assert_eq!(
        primary["allowedOnlineMeetingProviders"],
        json!(["teamsForBusiness"])
    );
    assert_eq!(primary["defaultOnlineMeetingProvider"], "teamsForBusiness");
    assert_eq!(primary["hexColor"], "#9fe1e7");
    let team = &calendars[1];
    assert_eq!(
        (team["canEdit"].as_bool(), team["canShare"].as_bool()),
        (Some(true), Some(false))
    );
    assert_eq!(team["owner"]["name"], "Brightmoor Team");
    let holidays = &calendars[2];
    assert_eq!(holidays["canEdit"], false);
    assert_eq!(holidays["allowedOnlineMeetingProviders"], json!([]));

    let default = graph.get("/v1.0/me/calendar").await.json;
    assert_eq!(default["id"], primary["id"]);
    assert!(
        default["@odata.context"]
            .as_str()
            .unwrap()
            .ends_with("/calendar/$entity")
    );
    let by_id = graph
        .get(&format!(
            "/v1.0/me/calendars/{}?$select=name",
            team["id"].as_str().unwrap()
        ))
        .await
        .json;
    assert_eq!(by_id["name"], "Brightmoor Team");
    assert!(by_id.get("canEdit").is_none());
    assert_error(
        &graph.get("/v1.0/me/calendars/nope").await,
        400,
        "ErrorInvalidIdMalformed",
    );
    let filtered = graph
        .get(&format!(
            "/v1.0/me/calendars?$filter={}",
            urlencode("canEdit eq false")
        ))
        .await
        .json;
    assert_eq!(filtered["value"].as_array().unwrap().len(), 1);

    // The signed-in user's own calendar answers under `/users` too; another user's doesn't.
    assert_eq!(
        graph
            .get("/v1.0/users/maya@brightmoor.co/calendar")
            .await
            .status,
        200
    );
    assert_error(
        &graph.get("/v1.0/users/priya@brightmoor.co/calendar").await,
        403,
        "ErrorAccessDenied",
    );
}

#[tokio::test]
async fn views_expand_series() {
    let graph = Graph::start().await;
    let metadata = Metadata::load();
    let week = all(&graph, &format!("/v1.0/me/calendarView?{WEEK}")).await;
    assert_eq!(
        subjects(&week),
        [
            "Weekly product sync",
            "Aiko: design exercise",
            "Priya / Maya 1:1",
            "Design crit",
            "Focus time",
            "Calloway: Atlas feedback call",
            "Dinner with Jess 🍕",
            "Focus time",
        ]
    );
    for event in &week {
        metadata.assert("event", event);
    }
    let sync = find(&week, "Weekly product sync");
    assert_eq!(sync["type"], "occurrence");
    assert!(sync["seriesMasterId"].is_string());
    assert_eq!(
        sync["start"],
        json!({ "dateTime": "2026-11-16T15:00:00.0000000", "timeZone": "UTC" })
    );
    assert_eq!(sync["originalStart"], "2026-11-16T15:00:00Z");
    assert_eq!(sync["originalStartTimeZone"], "Eastern Standard Time");
    assert_eq!(sync["responseStatus"]["response"], "organizer");
    assert_eq!(sync["organizer"]["emailAddress"]["name"], "Maya Okafor");
    assert_eq!(sync["attendees"].as_array().unwrap().len(), 3);
    assert_eq!(sync["location"]["displayName"], "Hudson Room");
    let invite = find(&week, "Calloway: Atlas feedback call");
    assert_eq!(invite["showAs"], "tentative");
    assert_eq!(invite["responseStatus"]["response"], "none");
    assert_eq!(invite["isOrganizer"], false);
    assert_eq!(
        invite["isOnlineMeeting"], true,
        "a Meet link is an online meeting"
    );
    assert_eq!(invite["onlineMeetingProvider"], "unknown");
    assert!(
        invite["onlineMeeting"]["joinUrl"]
            .as_str()
            .unwrap()
            .starts_with("https://meet.google.com/")
    );
    assert!(
        invite["body"]["content"]
            .as_str()
            .unwrap()
            .starts_with("<html><head></head><body>")
    );
    assert_eq!(
        find(&week, "Priya / Maya 1:1")["organizer"]["emailAddress"]["address"],
        "priya@brightmoor.co"
    );

    let error = graph.get("/v1.0/me/calendarView").await;
    assert_error(&error, 400, "ErrorInvalidParameter");
    let pacific = graph
        .get_with(
            &format!("/v1.0/me/calendarView?{WEEK}&$top=1"),
            &[("Prefer", "outlook.timezone=\"Pacific Standard Time\"")],
        )
        .await;
    assert_eq!(
        pacific.json["value"][0]["start"],
        json!({ "dateTime": "2026-11-16T07:00:00.0000000", "timeZone": "Pacific Standard Time" })
    );
    assert!(
        pacific.headers["preference-applied"]
            .to_str()
            .unwrap()
            .contains("outlook.timezone")
    );
    let iana = graph
        .get_with(
            &format!("/v1.0/me/calendarView?{WEEK}&$top=1"),
            &[("Prefer", "outlook.timezone=\"America/New_York\"")],
        )
        .await;
    assert_eq!(
        iana.json["value"][0]["start"]["dateTime"],
        "2026-11-16T10:00:00.0000000"
    );

    let page = graph
        .get(&format!(
            "/v1.0/me/calendarView?{WEEK}&$top=3&$select=subject,start"
        ))
        .await
        .json;
    assert_eq!(page["value"].as_array().unwrap().len(), 3);
    assert!(
        page["@odata.nextLink"]
            .as_str()
            .unwrap()
            .contains("$skip=3")
    );
    assert!(page["value"][0].get("end").is_none());
    let filter = urlencode("subject eq 'Design crit'");
    let crit = graph
        .get(&format!("/v1.0/me/calendarView?{WEEK}&$filter={filter}"))
        .await
        .json;
    assert_eq!(subjects(crit["value"].as_array().unwrap()), ["Design crit"]);
    let latest = graph
        .get(&format!(
            "/v1.0/me/calendarView?{WEEK}&$orderby={}&$top=1",
            urlencode("start/dateTime desc")
        ))
        .await
        .json;
    assert_eq!(latest["value"][0]["subject"], "Focus time");
    assert_eq!(
        subjects(&all(&graph, &format!("/v1.0/me/calendar/calendarView?{WEEK}")).await),
        subjects(&week)
    );
    let team = calendar_id(&graph, "Brightmoor Team").await;
    let shared = all(
        &graph,
        &format!("/v1.0/me/calendars/{team}/calendarView?{WEEK}"),
    )
    .await;
    assert_eq!(subjects(&shared), ["Company all-hands"]);
}

#[tokio::test]
async fn events_are_series_and_single_events() {
    let graph = Graph::start().await;
    let metadata = Metadata::load();
    let events = all(&graph, "/v1.0/me/events?$top=100").await;
    for event in &events {
        metadata.assert("event", event);
    }
    assert!(events.iter().all(|e| e["type"] != "occurrence"));
    let sync = find(&events, "Weekly product sync");
    assert_eq!(sync["type"], "seriesMaster");
    assert_eq!(
        sync["recurrence"],
        json!({
            "pattern": { "type": "weekly", "interval": 1, "month": 0, "dayOfMonth": 0, "firstDayOfWeek": "sunday", "index": "first", "daysOfWeek": ["monday"] },
            "range": { "type": "noEnd", "startDate": "2026-09-07", "endDate": "0001-01-01", "recurrenceTimeZone": "Eastern Standard Time", "numberOfOccurrences": 0 },
        })
    );
    let away = find(&events, "Out of office");
    assert_eq!(
        (away["isAllDay"].as_bool(), away["showAs"].as_str()),
        (Some(true), Some("oof"))
    );
    assert_eq!(
        away["start"]["dateTime"], "2026-11-26T00:00:00.0000000",
        "all-day days float"
    );

    let id = sync["id"].as_str().unwrap();
    let instances = all(&graph, &format!("/v1.0/me/events/{id}/instances?{WEEK}")).await;
    assert_eq!(instances.len(), 1);
    metadata.assert("event", &instances[0]);
    assert_eq!(instances[0]["seriesMasterId"], id);
    let occurrence = graph
        .get(&format!(
            "/v1.0/me/events/{}",
            instances[0]["id"].as_str().unwrap()
        ))
        .await;
    assert_eq!(occurrence.json["type"], "occurrence");
    let cancelled = graph
        .get(&format!(
            "/v1.0/me/events/{id}?$select=cancelledOccurrences"
        ))
        .await
        .json;
    assert_eq!(cancelled["cancelledOccurrences"], json!([]));
    assert_error(
        &graph.get(&format!("/v1.0/me/events/{id}/instances")).await,
        400,
        "ErrorInvalidParameter",
    );
    let team = calendar_id(&graph, "Brightmoor Team").await;
    assert_error(
        &graph
            .get(&format!("/v1.0/me/calendars/{team}/events/{id}"))
            .await,
        404,
        "ErrorItemNotFound",
    );
    assert_eq!(
        graph
            .get(&format!("/v1.0/me/calendar/events/{id}"))
            .await
            .status,
        200
    );
}

#[tokio::test]
async fn events_written_here_are_googles() {
    let graph = Graph::start().await;
    let metadata = Metadata::load();
    let pacific = [("Prefer", "outlook.timezone=\"Pacific Standard Time\"")];
    let created = graph
        .json_with(Method::POST, "/v1.0/me/events", &pacific, lunch())
        .await;
    assert_eq!(created.status, 201, "{}", created.json);
    metadata.assert("event", &created.json);
    let event = &created.json;
    assert_eq!(
        event["start"],
        json!({ "dateTime": "2026-11-18T12:00:00.0000000", "timeZone": "Pacific Standard Time" })
    );
    assert_eq!(event["originalStartTimeZone"], "Pacific Standard Time");
    assert_eq!(event["isOrganizer"], true);
    assert_eq!(event["categories"], json!(["Red category"]));
    assert_eq!(
        event["transactionId"],
        "7E163156-7762-4BEB-A1C6-729EA81755A7"
    );
    assert_eq!(event["attendees"][0]["status"]["response"], "none");
    assert_eq!(
        event["body"]["content"],
        "<html><head></head><body>Does noon work?</body></html>"
    );
    let id = event["id"].as_str().unwrap().to_owned();

    let theirs = google(&graph, "primary", "Lunch with Ines").await;
    assert_eq!(
        theirs["start"],
        json!({ "dateTime": "2026-11-18T12:00:00-08:00", "timeZone": "America/Los_Angeles" })
    );
    assert_eq!(theirs["location"], "Harry's Bar");
    assert_eq!(theirs["attendees"][0]["email"], "ines@brightmoor.co");
    assert_eq!(theirs["organizer"]["email"], "maya@brightmoor.co");

    let again = graph.json(Method::POST, "/v1.0/me/events", lunch()).await;
    assert_eq!(
        again.json["id"],
        id.as_str(),
        "the same transaction makes no second event"
    );
    let week = all(&graph, &format!("/v1.0/me/calendarView?{WEEK}")).await;
    assert_eq!(
        find(&week, "Lunch with Ines")["start"]["dateTime"],
        "2026-11-18T20:00:00.0000000"
    );

    let patched = graph
        .json(
            Method::PATCH,
            &format!("/v1.0/me/events/{id}"),
            json!({ "subject": "Lunch with Ines and Tom", "showAs": "oof", "importance": "high" }),
        )
        .await;
    assert_eq!(patched.status, 200, "{}", patched.json);
    assert_eq!(
        (
            patched.json["showAs"].as_str(),
            patched.json["importance"].as_str()
        ),
        (Some("oof"), Some("high"))
    );
    assert_ne!(patched.json["changeKey"], event["changeKey"]);
    let theirs = google(&graph, "primary", "Lunch with Ines and Tom").await;
    assert_eq!(theirs["transparency"], "opaque");

    let google_id = theirs["id"].as_str().unwrap();
    let renamed = graph
        .json(
            Method::PATCH,
            &format!("/calendar/v3/calendars/primary/events/{google_id}"),
            json!({ "summary": "Lunch, moved" }),
        )
        .await;
    assert_eq!(renamed.status, 200, "{}", renamed.json);
    let read = graph.get(&format!("/v1.0/me/events/{id}")).await.json;
    assert_eq!(
        read["subject"], "Lunch, moved",
        "Google's changes show in Outlook"
    );
    assert_eq!(read["showAs"], "oof", "what Outlook keeps stays");

    assert_eq!(
        graph
            .request(Method::DELETE, &format!("/v1.0/me/events/{id}"), &[], None)
            .await
            .status,
        204
    );
    assert_error(
        &graph.get(&format!("/v1.0/me/events/{id}")).await,
        404,
        "ErrorItemNotFound",
    );
    assert_eq!(
        google(&graph, "primary", "Lunch, moved").await["status"],
        "cancelled"
    );

    let holidays = calendar_id(&graph, "Holidays in United States").await;
    assert_error(
        &graph
            .json(
                Method::POST,
                &format!("/v1.0/me/calendars/{holidays}/events"),
                lunch(),
            )
            .await,
        403,
        "ErrorAccessDenied",
    );
    let mut nowhere = lunch();
    nowhere["start"]["timeZone"] = "Atlantis Standard Time".into();
    assert_error(
        &graph.json(Method::POST, "/v1.0/me/events", nowhere).await,
        400,
        "TimeZoneNotSupportedException",
    );
    let mut unknown = lunch();
    unknown["mood"] = "sunny".into();
    assert_error(
        &graph.json(Method::POST, "/v1.0/me/events", unknown).await,
        400,
        "RequestBodyRead",
    );

    let team = calendar_id(&graph, "Brightmoor Team").await;
    let day = json!({
        "subject": "Hack day",
        "isAllDay": true,
        "start": { "dateTime": "2026-11-21T00:00:00", "timeZone": "Eastern Standard Time" },
        "end": { "dateTime": "2026-11-22T00:00:00", "timeZone": "Eastern Standard Time" },
    });
    let all_day = graph
        .json(
            Method::POST,
            &format!("/v1.0/me/calendars/{team}/events"),
            day.clone(),
        )
        .await;
    assert_eq!(all_day.status, 201, "{}", all_day.json);
    assert!(
        all_day.json["@odata.context"]
            .as_str()
            .unwrap()
            .contains(&format!("calendars('{team}')/events/$entity"))
    );
    assert_eq!(all_day.json["isAllDay"], true);
    let theirs = google(
        &graph,
        "c_8f3b2a1d9e7c4b6a5f01@group.calendar.google.com",
        "Hack day",
    )
    .await;
    assert_eq!(
        (
            theirs["start"]["date"].as_str(),
            theirs["end"]["date"].as_str()
        ),
        (Some("2026-11-21"), Some("2026-11-22"))
    );
    let mut late = day;
    late["start"]["dateTime"] = "2026-11-21T09:00:00".into();
    assert_error(
        &graph.json(Method::POST, "/v1.0/me/events", late).await,
        400,
        "ErrorInvalidRequest",
    );
}

#[tokio::test]
async fn series_written_here() {
    let graph = Graph::start().await;
    let created = graph
        .json(
            Method::POST,
            "/v1.0/me/events",
            json!({
                "subject": "Planning",
                "start": { "dateTime": "2026-11-18T10:00:00", "timeZone": "Eastern Standard Time" },
                "end": { "dateTime": "2026-11-18T10:30:00", "timeZone": "Eastern Standard Time" },
                "recurrence": {
                    "pattern": { "type": "weekly", "interval": 1, "daysOfWeek": ["Monday"] },
                    "range": { "type": "endDate", "startDate": "2026-11-18", "endDate": "2026-12-31" },
                },
            }),
        )
        .await;
    assert_eq!(created.status, 201, "{}", created.json);
    let master = &created.json;
    assert_eq!(master["type"], "seriesMaster");
    assert_eq!(
        master["start"]["dateTime"], "2026-11-23T15:00:00.0000000",
        "the series starts on its first Monday"
    );
    assert_eq!(master["recurrence"]["range"]["endDate"], "2026-12-31");
    assert_eq!(
        google(&graph, "primary", "Planning").await["recurrence"],
        json!(["RRULE:FREQ=WEEKLY;BYDAY=MO;UNTIL=20270101T045959Z"])
    );
    let id = master["id"].as_str().unwrap();
    let span = "startDateTime=2026-11-16T00:00:00Z&endDateTime=2027-01-31T00:00:00Z";
    let instances = all(&graph, &format!("/v1.0/me/events/{id}/instances?{span}")).await;
    assert_eq!(instances.len(), 6);

    let second = instances[1]["id"].as_str().unwrap();
    let moved = graph
        .json(
            Method::PATCH,
            &format!("/v1.0/me/events/{second}"),
            json!({ "subject": "Planning (long)" }),
        )
        .await;
    assert_eq!(moved.status, 200, "{}", moved.json);
    assert_eq!(moved.json["type"], "exception");
    assert_eq!(moved.json["seriesMasterId"], id);
    let third = instances[2]["id"].as_str().unwrap();
    assert_eq!(
        graph
            .request(
                Method::DELETE,
                &format!("/v1.0/me/events/{third}"),
                &[],
                None
            )
            .await
            .status,
        204
    );
    let instances = all(&graph, &format!("/v1.0/me/events/{id}/instances?{span}")).await;
    assert_eq!(instances.len(), 5);
    assert_eq!(subjects(&instances)[1], "Planning (long)");
    let cancelled = graph
        .get(&format!(
            "/v1.0/me/events/{id}?$select=cancelledOccurrences"
        ))
        .await
        .json;
    assert_eq!(
        cancelled["cancelledOccurrences"],
        json!([format!("OID.{id}.2026-12-07")])
    );
    assert_error(
        &graph
            .json(
                Method::PATCH,
                &format!("/v1.0/me/events/{second}"),
                json!({ "recurrence": null }),
            )
            .await,
        400,
        "ErrorInvalidRequest",
    );

    let numbered = graph
        .json(
            Method::POST,
            "/v1.0/me/events",
            json!({
                "subject": "Standup",
                "start": { "dateTime": "2026-11-17T09:00:00", "timeZone": "America/New_York" },
                "end": { "dateTime": "2026-11-17T09:15:00", "timeZone": "America/New_York" },
                "recurrence": {
                    "pattern": { "type": "daily", "interval": 2 },
                    "range": { "type": "numbered", "startDate": "2026-11-17", "numberOfOccurrences": 3 },
                },
            }),
        )
        .await;
    assert_eq!(numbered.status, 201, "{}", numbered.json);
    let id = numbered.json["id"].as_str().unwrap();
    let days: Vec<String> = all(&graph, &format!("/v1.0/me/events/{id}/instances?{span}"))
        .await
        .iter()
        .map(|e| e["start"]["dateTime"].as_str().unwrap()[..10].to_owned())
        .collect();
    assert_eq!(days, ["2026-11-17", "2026-11-19", "2026-11-21"]);
    let bad = graph
        .json(Method::PATCH, &format!("/v1.0/me/events/{id}"), json!({ "recurrence": { "pattern": { "type": "weekly", "interval": 1 }, "range": { "type": "noEnd", "startDate": "2026-11-17" } } }))
        .await;
    assert_error(&bad, 400, "ErrorInvalidRequest");
}

#[tokio::test]
async fn online_meetings_are_teams_meetings() {
    let graph = Graph::start().await;
    let mut body = lunch();
    body.as_object_mut().unwrap().remove("transactionId");
    body["isOnlineMeeting"] = true.into();
    body["onlineMeetingProvider"] = "teamsForBusiness".into();
    let created = graph
        .json(Method::POST, "/v1.0/me/events", body.clone())
        .await;
    assert_eq!(created.status, 201, "{}", created.json);
    assert_eq!(created.json["isOnlineMeeting"], true);
    assert_eq!(created.json["onlineMeetingProvider"], "teamsForBusiness");
    let join = created.json["onlineMeeting"]["joinUrl"].as_str().unwrap();
    assert!(
        join.starts_with(&format!("{}l/meetup-join/", graph.server.url())),
        "{join}"
    );
    let id = created.json["id"].as_str().unwrap();
    let off = graph
        .json(
            Method::PATCH,
            &format!("/v1.0/me/events/{id}"),
            json!({ "isOnlineMeeting": false }),
        )
        .await;
    assert_eq!(
        off.json["onlineMeeting"]["joinUrl"], join,
        "a meeting stays online"
    );

    let mut default = body.clone();
    default
        .as_object_mut()
        .unwrap()
        .remove("onlineMeetingProvider");
    let made = graph.json(Method::POST, "/v1.0/me/events", default).await;
    assert_eq!(made.json["onlineMeetingProvider"], "teamsForBusiness");
    let mut skype = body;
    skype["onlineMeetingProvider"] = "skypeForConsumer".into();
    assert_error(
        &graph.json(Method::POST, "/v1.0/me/events", skype).await,
        400,
        "ErrorInvalidRequest",
    );

    // A personal account has calendars of its own, where online meetings are Skype's.
    let mut personal = Graph::start().await;
    personal.token = format!("{DEV_ACCESS_TOKEN}:maya.okafor@outlook.com");
    let calendars = personal.get("/v1.0/me/calendars").await.json;
    assert_eq!(
        calendars["value"][0]["owner"]["address"],
        "maya.okafor@outlook.com"
    );
    assert_eq!(
        calendars["value"][0]["allowedOnlineMeetingProviders"],
        json!(["skypeForConsumer"])
    );
    let week = all(&personal, &format!("/v1.0/me/calendarView?{WEEK}")).await;
    assert_eq!(subjects(&week), ["Dentist", "Yoga", "Pick up dry cleaning"]);
}

#[tokio::test]
async fn calendar_view_delta() {
    let graph = Graph::start().await;
    let metadata = Metadata::load();
    let small = [("Prefer", "odata.maxpagesize=3")];
    let (first, last) = graph
        .pages(&format!("/v1.0/me/calendarView/delta?{WEEK}"), &small)
        .await;
    assert_eq!(first.len(), 8);
    for event in &first {
        assert_eq!(event["@odata.type"], "#microsoft.graph.event");
        metadata.assert("event", event);
    }
    let link = last["@odata.deltaLink"].as_str().unwrap().to_owned();
    assert!(link.contains("/v1.0/me/calendarView/delta?$deltatoken="));
    let quiet = graph.get(&link).await.json;
    assert_eq!(quiet["value"], json!([]), "nothing changed");

    let created = graph
        .json(Method::POST, "/v1.0/me/events", lunch())
        .await
        .json;
    let aiko = find(&first, "Aiko: design exercise")["id"]
        .as_str()
        .unwrap()
        .to_owned();
    graph
        .request(
            Method::DELETE,
            &format!("/v1.0/me/events/{aiko}"),
            &[],
            None,
        )
        .await;
    let call = find(&first, "Calloway: Atlas feedback call")["id"]
        .as_str()
        .unwrap()
        .to_owned();
    let later = json!({
        "start": { "dateTime": "2026-12-10T14:00:00", "timeZone": "Eastern Standard Time" },
        "end": { "dateTime": "2026-12-10T15:00:00", "timeZone": "Eastern Standard Time" },
    });
    assert_eq!(
        graph
            .json(Method::PATCH, &format!("/v1.0/me/events/{call}"), later)
            .await
            .status,
        200
    );
    let dinner = google(&graph, "primary", "Dinner with Jess 🍕").await;
    graph
        .json(
            Method::PATCH,
            &format!(
                "/calendar/v3/calendars/primary/events/{}",
                dinner["id"].as_str().unwrap()
            ),
            json!({ "summary": "Dinner with Jess" }),
        )
        .await;
    let board = all(&graph, "/v1.0/me/events?$top=100").await;
    let board = find(&board, "Board prep")["id"]
        .as_str()
        .unwrap()
        .to_owned();
    graph
        .json(
            Method::PATCH,
            &format!("/v1.0/me/events/{board}"),
            json!({ "importance": "high" }),
        )
        .await;

    let (changes, last) = graph.pages(&link, &[]).await;
    for change in &changes {
        metadata.assert("event", change);
    }
    let removed: Vec<&str> = changes
        .iter()
        .filter(|c| c.get("@removed").is_some())
        .map(|c| c["id"].as_str().unwrap())
        .collect();
    let mut expected = vec![aiko.as_str(), call.as_str(), board.as_str()];
    expected.sort_unstable();
    let mut removed = removed;
    removed.sort_unstable();
    assert_eq!(
        removed, expected,
        "deleted, moved out, and changed outside the window"
    );
    let updated: Vec<&str> = changes
        .iter()
        .filter(|c| c.get("@removed").is_none())
        .map(|c| c["subject"].as_str().unwrap())
        .collect();
    assert_eq!(updated, ["Lunch with Ines", "Dinner with Jess"]);
    assert_eq!(
        changes
            .iter()
            .find(|c| c["subject"] == "Lunch with Ines")
            .unwrap()["id"],
        created["id"]
    );
    assert!(last["@odata.deltaLink"].is_string());

    assert_error(
        &graph
            .get(&format!(
                "/v1.0/me/calendarView/delta?{WEEK}&$select=subject"
            ))
            .await,
        400,
        "ErrorInvalidUrlQuery",
    );
    assert_error(
        &graph
            .get("/v1.0/me/calendarView/delta?$deltatoken=bm9wZQ")
            .await,
        410,
        "SyncStateNotFound",
    );
    assert_error(
        &graph.get("/v1.0/me/calendarView/delta").await,
        400,
        "ErrorInvalidParameter",
    );
    let team = calendar_id(&graph, "Brightmoor Team").await;
    let (shared, last) = graph
        .pages(
            &format!("/v1.0/me/calendars/{team}/calendarView/delta?{WEEK}"),
            &[],
        )
        .await;
    assert_eq!(subjects(&shared), ["Company all-hands"]);
    let token = last["@odata.deltaLink"]
        .as_str()
        .unwrap()
        .split("$deltatoken=")
        .nth(1)
        .unwrap()
        .to_owned();
    assert_error(
        &graph
            .get(&format!("/v1.0/me/calendarView/delta?$deltatoken={token}"))
            .await,
        400,
        "BadRequest",
    );
}

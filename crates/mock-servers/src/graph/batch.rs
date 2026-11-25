//! `POST /v1.0/$batch`: up to 20 requests in one JSON body, each run as if it had come on its
//! own (with the batch's credentials), in parallel dependency waves ordered by `dependsOn`. A
//! request whose dependency failed gets a 424; one that's throttled gets its own 429.

use std::collections::BTreeSet;
use std::pin::Pin;
use std::sync::Arc;
use std::task::Poll;

use axum::Router;
use axum::body::{Body, Bytes, to_bytes};
use axum::extract::{Request, State};
use axum::http::{HeaderMap, HeaderName, HeaderValue, Method, header};
use axum::response::Response;
use base64::Engine;
use base64::engine::general_purpose::STANDARD;
use serde_json::{Map, Value, json};
use tower::ServiceExt;

use super::error::{GraphError, Result};
use super::reply;
use crate::world::App;

/// Graph's limit on requests per batch.
const MAX_REQUESTS: usize = 20;

pub async fn handle(
    State((app, api)): State<(Arc<App>, Router)>,
    headers: HeaderMap,
    body: Bytes,
) -> Result<Response> {
    let batch: Value = serde_json::from_slice(&body).map_err(GraphError::unreadable_body)?;
    let requests = batch["requests"].as_array().ok_or_else(|| {
        GraphError::bad_request("The batch request must contain a 'requests' array.")
    })?;
    if requests.len() > MAX_REQUESTS {
        return Err(GraphError::bad_request(format!(
            "The number of requests in a batch is limited to {MAX_REQUESTS}."
        )));
    }
    let mut ids: Vec<String> = Vec::new();
    for request in requests {
        let id = json_id(&request["id"])
            .ok_or_else(|| GraphError::bad_request("Each request in a batch needs an id."))?;
        if ids.iter().any(|known| known.eq_ignore_ascii_case(&id)) {
            return Err(GraphError::bad_request(format!(
                "Batch request ids must be unique: '{id}' appears twice."
            )));
        }
        if request.get("body").is_some_and(|b| !b.is_null()) && content_type(request).is_none() {
            return Err(GraphError::bad_request(format!(
                "Write request id : {id} does not contain Content-Type header or body."
            )));
        }
        ids.push(id);
    }
    let waves = dependency_waves(requests, &ids)?;
    if waves.is_empty() {
        let latency = app.lock().config.latency;
        if !latency.is_zero() {
            tokio::time::sleep(latency).await;
        }
    }
    let mut statuses: Vec<Option<u16>> = vec![None; requests.len()];
    let mut responses: Vec<Option<Value>> = vec![None; requests.len()];
    for wave in waves {
        let futures: Vec<_> = wave
            .iter()
            .map(|&index| {
                let request = &requests[index];
                let failed_dependency = depends_on(request).iter().any(|dependency| {
                    ids.iter()
                        .position(|id| id.eq_ignore_ascii_case(dependency))
                        .and_then(|at| statuses[at])
                        .is_some_and(|status| status >= 400)
                });
                let id = ids[index].as_str();
                let (app, api, headers) = (&app, &api, &headers);
                async move {
                    if failed_dependency {
                        Ok(json!({
                            "id": id,
                            "status": 424,
                            "body": { "error": { "code": "FailedDependency", "message": "The request depends on a request in the batch that failed." } },
                        }))
                    } else {
                        run(app, api, headers, id, request).await
                    }
                }
            })
            .collect();
        let batch_wave = join_all(futures).await;
        for (index, result) in wave.into_iter().zip(batch_wave) {
            let response = result?;
            statuses[index] = response["status"]
                .as_u64()
                .and_then(|s| u16::try_from(s).ok());
            responses[index] = Some(response);
        }
    }
    Ok(reply(
        200,
        json!({ "responses": responses.into_iter().flatten().collect::<Vec<_>>() }),
    ))
}

fn json_id(value: &Value) -> Option<String> {
    value
        .as_str()
        .map(str::to_owned)
        .or_else(|| value.as_u64().map(|n| n.to_string()))
        .or_else(|| value.as_i64().map(|n| n.to_string()))
}

fn depends_on(request: &Value) -> Vec<String> {
    request["dependsOn"]
        .as_array()
        .map(|ids| ids.iter().filter_map(json_id).collect())
        .unwrap_or_default()
}

fn content_type(request: &Value) -> Option<String> {
    request["headers"]
        .as_object()?
        .iter()
        .find(|(name, _)| name.eq_ignore_ascii_case("content-type"))
        .and_then(|(_, value)| value.as_str().map(str::to_owned))
}

/// Groups requests into topological waves so independent requests run concurrently while every
/// request still waits for its `dependsOn` predecessors.
fn dependency_waves(requests: &[Value], ids: &[String]) -> Result<Vec<Vec<usize>>> {
    let mut done: BTreeSet<usize> = BTreeSet::new();
    let mut waves = Vec::new();
    while done.len() < requests.len() {
        let wave: Vec<usize> = (0..requests.len())
            .filter(|index| {
                !done.contains(index)
                    && depends_on(&requests[*index]).iter().all(|dependency| {
                        ids.iter()
                            .position(|id| id.eq_ignore_ascii_case(dependency))
                            .is_some_and(|at| done.contains(&at))
                    })
            })
            .collect();
        if wave.is_empty() {
            let unknown = requests
                .iter()
                .flat_map(depends_on)
                .find(|d| !ids.iter().any(|id| id.eq_ignore_ascii_case(d)));
            return Err(GraphError::bad_request(match unknown {
                Some(id) => format!("The request depends on '{id}', which isn't in the batch."),
                None => "The batch's dependsOn form a cycle.".to_owned(),
            }));
        }
        done.extend(wave.iter().copied());
        waves.push(wave);
    }
    Ok(waves)
}

enum Slot<F, T> {
    Running(Pin<Box<F>>),
    Done(T),
    Taken,
}

/// Polls `futures` concurrently in index order and collects their outputs once all finish.
async fn join_all<F, T>(futures: Vec<F>) -> Vec<T>
where
    F: Future<Output = T>,
{
    let mut slots: Vec<Slot<F, T>> = futures
        .into_iter()
        .map(|future| Slot::Running(Box::pin(future)))
        .collect();
    std::future::poll_fn(|cx| {
        let mut all_done = true;
        for slot in &mut slots {
            if let Slot::Running(pinned) = slot {
                match pinned.as_mut().poll(cx) {
                    Poll::Ready(value) => *slot = Slot::Done(value),
                    Poll::Pending => all_done = false,
                }
            }
        }
        if all_done {
            Poll::Ready(
                slots
                    .iter_mut()
                    .map(|slot| match std::mem::replace(slot, Slot::Taken) {
                        Slot::Done(value) => value,
                        Slot::Running(_) | Slot::Taken => unreachable!("every slot completed"),
                    })
                    .collect(),
            )
        } else {
            Poll::Pending
        }
    })
    .await
}

/// Runs one request through the API and renders its response for the batch.
async fn run(
    app: &App,
    api: &Router,
    outer: &HeaderMap,
    id: &str,
    request: &Value,
) -> Result<Value> {
    let method = request["method"]
        .as_str()
        .and_then(|method| Method::from_bytes(method.to_ascii_uppercase().as_bytes()).ok())
        .ok_or_else(|| {
            GraphError::bad_request(format!("Request id {id} has an invalid method."))
        })?;
    let url = request["url"].as_str().unwrap_or_default();
    if url.contains("://") {
        return Err(GraphError::bad_request(format!(
            "Request id {id}'s url must be relative."
        )));
    }
    let target = format!("/v1.0/{}", url.trim_start_matches('/'));
    let mut builder = Request::builder().method(method).uri(super::own_mailbox(
        app,
        outer,
        &super::canonical_target(&target),
    ));
    if let Some(auth) = outer.get(header::AUTHORIZATION) {
        builder = builder.header(header::AUTHORIZATION, auth);
    }
    for (name, value) in request["headers"].as_object().into_iter().flatten() {
        if let (Ok(name), Some(Ok(value))) = (
            HeaderName::from_bytes(name.as_bytes()),
            value.as_str().map(HeaderValue::from_str),
        ) {
            builder = builder.header(name, value);
        }
    }
    let body = match &request["body"] {
        Value::Null => Body::empty(),
        Value::String(text) => Body::from(text.clone()),
        other => Body::from(serde_json::to_vec(other).expect("JSON values serialize")),
    };
    let inner = builder.body(body).map_err(GraphError::unreadable_body)?;
    let response = api
        .clone()
        .oneshot(inner)
        .await
        .unwrap_or_else(|never| match never {});
    let (parts, body) = response.into_parts();
    let bytes = to_bytes(body, usize::MAX).await.unwrap_or_default();
    let mut headers = Map::new();
    for (name, value) in &parts.headers {
        if name == header::CONTENT_LENGTH {
            continue;
        }
        if let Ok(value) = value.to_str() {
            headers.insert(crate::http::title_case(name.as_str()), value.into());
        }
    }
    let is_json = parts
        .headers
        .get(header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .is_some_and(|v| v.starts_with("application/json"));
    let mut rendered = json!({ "id": id, "status": parts.status.as_u16(), "headers": headers });
    if !bytes.is_empty() {
        rendered["body"] = if is_json {
            serde_json::from_slice(&bytes).unwrap_or(Value::Null)
        } else {
            STANDARD.encode(&bytes).into()
        };
    }
    Ok(rendered)
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use super::*;
    use crate::{Config, DEV_ACCESS_TOKEN};

    #[test]
    fn numeric_and_string_depends_on_group_into_waves() {
        let requests = vec![
            json!({ "id": 1, "method": "GET", "url": "/me" }),
            json!({ "id": 2, "dependsOn": [1], "method": "GET", "url": "/me" }),
            json!({ "id": "3", "method": "GET", "url": "/me" }),
            json!({ "id": -4, "dependsOn": [2, "3"], "method": "GET", "url": "/me" }),
        ];
        let ids: Vec<String> = requests
            .iter()
            .map(|r| json_id(&r["id"]).unwrap())
            .collect();
        assert_eq!(ids, ["1", "2", "3", "-4"]);
        assert_eq!(depends_on(&requests[1]), ["1"]);
        assert_eq!(depends_on(&requests[3]), ["2", "3"]);
        let waves = dependency_waves(&requests, &ids).unwrap();
        assert_eq!(waves, vec![vec![0, 2], vec![1], vec![3]]);
    }

    #[tokio::test]
    async fn unchained_subrequests_exercise_concurrency_while_chained_waves_wait() {
        let router = crate::router(&Config {
            latency: Duration::from_millis(25),
            graph_concurrency: Some(2),
            ..Config::default()
        });
        let send = |body: Value| {
            let router = router.clone();
            async move {
                let request = Request::builder()
                    .method(Method::POST)
                    .uri("/v1.0/$batch")
                    .header(header::AUTHORIZATION, format!("Bearer {DEV_ACCESS_TOKEN}"))
                    .header(header::CONTENT_TYPE, "application/json")
                    .body(Body::from(body.to_string()))
                    .unwrap();
                let response = router.oneshot(request).await.unwrap();
                let bytes = to_bytes(response.into_body(), usize::MAX).await.unwrap();
                serde_json::from_slice::<Value>(&bytes).unwrap()
            }
        };

        // Three unchained subrequests run in one wave against a limit of 2, so the third 429s.
        let unchained = send(json!({
            "requests": [
                { "id": 1, "method": "GET", "url": "/me" },
                { "id": 2, "method": "GET", "url": "/me" },
                { "id": 3, "method": "GET", "url": "/me" },
            ]
        }))
        .await;
        let statuses: Vec<u64> = unchained["responses"]
            .as_array()
            .unwrap()
            .iter()
            .map(|r| r["status"].as_u64().unwrap())
            .collect();
        assert_eq!(statuses, [200, 200, 429]);

        // Chaining request 3 onto numeric id 1 puts it in the second wave, so all three fit.
        let chained = send(json!({
            "requests": [
                { "id": 1, "method": "GET", "url": "/me" },
                { "id": 2, "method": "GET", "url": "/me" },
                { "id": 3, "dependsOn": [1], "method": "GET", "url": "/me" },
            ]
        }))
        .await;
        let statuses: Vec<u64> = chained["responses"]
            .as_array()
            .unwrap()
            .iter()
            .map(|r| r["status"].as_u64().unwrap())
            .collect();
        assert_eq!(statuses, [200, 200, 200]);
    }
}

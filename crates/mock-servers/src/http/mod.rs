//! Router assembly and the cross-cutting HTTP behavior of Google's front end: JSON formatting
//! (`prettyPrint`, `fields`), auth, the per-user quota, fault injection, CORS and request
//! logging.

mod batch;
pub mod extract;
mod fields;
pub mod quota;

use std::sync::Arc;
use std::time::Instant;

use axum::Router;
use axum::body::{Body, to_bytes};
use axum::extract::{DefaultBodyLimit, Request, State};
use axum::http::{HeaderValue, Method, StatusCode, Uri, header};
use axum::middleware::{Next, from_fn, from_fn_with_state};
use axum::response::{Html, IntoResponse, Response};
use axum::routing::post;
use tower_http::cors::CorsLayer;

use crate::error::Failure;
use crate::world::{App, Fault, World};
use extract::Params;

/// Gmail accepts uploads up to 35 MB. The limit leaves headroom for base64 in JSON bodies.
const BODY_LIMIT: usize = 64 * 1024 * 1024;

/// The whole mock: every Google API on one origin, plus `/__mock/` admin endpoints.
pub fn router(app: Arc<App>) -> Router {
    let api = Router::new()
        .merge(crate::mail::routes::router(app.clone()))
        .merge(crate::calendar::routes::router())
        .merge(crate::people::routes::router())
        .merge(crate::oauth::api_router())
        .layer(from_fn_with_state(app.clone(), inject_faults))
        .layer(from_fn_with_state(app.clone(), quota::enforce))
        .layer(from_fn_with_state(app.clone(), crate::oauth::authenticate))
        .layer(from_fn(format_json))
        .with_state(app.clone());

    let batch = post(batch::handle)
        .with_state(api.clone())
        .layer(from_fn_with_state(app.clone(), inject_faults))
        .layer(from_fn(format_json));

    let oauth = crate::oauth::router()
        .layer(from_fn_with_state(app.clone(), inject_faults))
        .with_state(app.clone());

    Router::new()
        .merge(api)
        .route("/batch/gmail/v1", batch)
        .merge(oauth)
        .merge(crate::graph::router(app.clone()))
        .merge(crate::admin::router().with_state(app.clone()))
        .fallback(not_found)
        .layer(DefaultBodyLimit::max(BODY_LIMIT))
        .layer(CorsLayer::very_permissive())
        .layer(from_fn_with_state(app, delay))
        .layer(from_fn(log_requests))
}

/// Holds every non-Graph API request for the configured latency (admin requests answer at once,
/// and `/v1.0/` requests hold their latency inside Graph's concurrency limit). A Gmail batch is
/// one round trip, as with Google.
async fn delay(State(app): State<Arc<App>>, request: Request, next: Next) -> Response {
    let latency = app.lock().config.latency;
    let path = request.uri().path();
    if !latency.is_zero() && !path.starts_with("/__mock/") && !path.starts_with("/v1.0/") {
        tokio::time::sleep(latency).await;
    }
    next.run(request).await
}

/// Google pretty-prints JSON unless `prettyPrint=false`, applies `fields` to successful
/// responses, and always labels JSON as UTF-8.
async fn format_json(request: Request, next: Next) -> Response {
    let params = Params::parse(request.uri().query().unwrap_or_default());
    let response = next.run(request).await;
    let is_json = response
        .headers()
        .get(header::CONTENT_TYPE)
        .and_then(|value| value.to_str().ok())
        .is_some_and(|value| value.starts_with("application/json"));
    if !is_json {
        return response;
    }
    let (mut parts, body) = response.into_parts();
    let Ok(bytes) = to_bytes(body, usize::MAX).await else {
        return StatusCode::INTERNAL_SERVER_ERROR.into_response();
    };
    let Ok(mut value) = serde_json::from_slice::<serde_json::Value>(&bytes) else {
        return Response::from_parts(parts, Body::from(bytes));
    };
    if parts.status.is_success()
        && let Some(mask) = params.text("fields")
    {
        value = fields::Mask::parse(mask).apply(value);
    }
    let pretty = params.get("prettyPrint").is_none_or(|v| v != "false");
    let mut text = if pretty {
        serde_json::to_string_pretty(&value)
    } else {
        serde_json::to_string(&value)
    }
    .expect("JSON values serialize");
    text.push('\n');
    parts.headers.insert(
        header::CONTENT_TYPE,
        HeaderValue::from_static("application/json; charset=UTF-8"),
    );
    parts.headers.remove(header::CONTENT_LENGTH);
    for vary in ["Origin", "X-Origin", "Referer"] {
        parts
            .headers
            .append(header::VARY, HeaderValue::from_static(vary));
    }
    Response::from_parts(parts, Body::from(text))
}

/// Takes the first armed fault matching `method` and `uri` (path prefix and any required query
/// parameters).
pub(crate) fn take_fault(world: &mut World, method: &Method, uri: &Uri) -> Option<Fault> {
    let path = uri.path();
    let method = method.as_str();
    let query: Vec<(String, String)> =
        url::form_urlencoded::parse(uri.query().unwrap_or_default().as_bytes())
            .into_owned()
            .collect();
    world
        .faults
        .iter_mut()
        .find(|fault| {
            fault.times > 0
                && path.starts_with(&fault.path)
                && fault
                    .method
                    .as_deref()
                    .is_none_or(|wanted| wanted.eq_ignore_ascii_case(method))
                && fault.query.as_deref().is_none_or(|wanted| {
                    url::form_urlencoded::parse(wanted.as_bytes())
                        .into_owned()
                        .all(|pair| query.contains(&pair))
                })
        })
        .map(|fault| {
            fault.times -= 1;
            fault.clone()
        })
}

/// `content-type` → `Content-Type`, as Google's multipart framing and Graph's `$batch` write
/// header names.
pub(crate) fn title_case(name: &str) -> String {
    name.split('-')
        .map(|word| {
            let mut chars = word.chars();
            chars.next().map_or_else(String::new, |first| {
                first.to_ascii_uppercase().to_string() + chars.as_str()
            })
        })
        .collect::<Vec<_>>()
        .join("-")
}

/// Answers matching requests with a canned error, as configured through `/__mock/faults`. The
/// error comes instead of handling them, or after it (`after`), so the change is made but the
/// answer is lost.
async fn inject_faults(State(app): State<Arc<App>>, request: Request, next: Next) -> Response {
    let fault = take_fault(&mut app.lock(), request.method(), request.uri());
    let Some(fault) = fault else {
        return next.run(request).await;
    };
    let path = request.uri().path().to_owned();
    let query = request.uri().query().unwrap_or_default().to_owned();
    if fault.after {
        let _ = next.run(request).await;
    }
    if path == "/o/oauth2/v2/auth" && fault.status == StatusCode::FOUND.as_u16() {
        let params = Params::parse(&query);
        if let Some(Ok(mut redirect)) = params.text("redirect_uri").map(url::Url::parse) {
            {
                let mut pairs = redirect.query_pairs_mut();
                pairs.append_pair("error", fault.reason.as_deref().unwrap_or("access_denied"));
                if let Some(message) = &fault.message {
                    pairs.append_pair("error_description", message);
                }
                if let Some(state) = params.get("state") {
                    pairs.append_pair("state", state);
                }
            }
            return (
                StatusCode::FOUND,
                [(header::LOCATION, redirect.to_string())],
            )
                .into_response();
        }
    }
    if matches!(path.as_str(), "/token" | "/revoke") && fault.status < 500 && fault.status != 429 {
        let default_error = if path == "/revoke" {
            "invalid_token"
        } else if fault.status == 401 {
            "invalid_client"
        } else {
            "invalid_grant"
        };
        let body = gmail::oauth::OAuthError {
            error: fault.reason.unwrap_or_else(|| default_error.to_owned()),
            error_description: fault.message,
        };
        let status = StatusCode::from_u16(fault.status).unwrap_or(StatusCode::BAD_REQUEST);
        return (
            status,
            [(header::CONTENT_TYPE, "application/json; charset=utf-8")],
            serde_json::to_string_pretty(&body).expect("OAuthError serializes"),
        )
            .into_response();
    }
    let (message, reason) = canned(fault.status);
    let mut failure = Failure::new(
        fault.status,
        fault.message.as_deref().unwrap_or(message),
        fault.reason.as_deref().unwrap_or(reason),
    );
    if path.starts_with("/calendar/") {
        failure = failure.legacy();
    }
    failure.into_response()
}

/// The message and reason Google typically sends with each status.
fn canned(status: u16) -> (&'static str, &'static str) {
    match status {
        400 => ("Precondition check failed.", "failedPrecondition"),
        401 => ("Invalid Credentials", "authError"),
        403 => ("User-rate limit exceeded.", "rateLimitExceeded"),
        404 => ("Requested entity was not found.", "notFound"),
        410 => ("Resource has been deleted", "deleted"),
        429 => (
            "Too many concurrent requests for user.",
            "rateLimitExceeded",
        ),
        503 => ("The service is currently unavailable.", "backendError"),
        _ => ("Backend Error", "backendError"),
    }
}

/// Google's HTML 404 for paths no API serves.
async fn not_found(request: Request) -> Response {
    let path = request.uri().path().to_owned();
    (
        StatusCode::NOT_FOUND,
        Html(format!(
            "<!DOCTYPE html>\n<html lang=en>\n<title>Error 404 (Not Found)!!1</title>\n\
             <p><b>404.</b> <ins>That’s an error.</ins>\n<p>The requested URL <code>{path}</code> \
             was not found on this server.  <ins>That’s all we know.</ins>\n"
        )),
    )
        .into_response()
}

async fn log_requests(request: Request, next: Next) -> Response {
    let started = Instant::now();
    let method = request.method().clone();
    let target = request
        .uri()
        .path_and_query()
        .map_or_else(|| "/".to_owned(), ToString::to_string);
    let response = next.run(request).await;
    log::info!(
        "{method} {target} -> {} ({:.1?})",
        response.status().as_u16(),
        started.elapsed()
    );
    response
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use serde_json::{Value, json};
    use tower::ServiceExt;

    use super::*;
    use crate::{Config, DEV_ACCESS_TOKEN};

    #[test]
    fn title_case_formats_header_names() {
        assert_eq!(title_case("content-type"), "Content-Type");
        assert_eq!(title_case("www-authenticate"), "Www-Authenticate");
        assert_eq!(title_case("etag"), "Etag");
        assert_eq!(title_case(""), "");
    }

    #[test]
    fn take_fault_checks_path_method_and_query() {
        let mut world = crate::fixtures::world(&Config::default());
        world.faults.push(Fault {
            method: Some("post".into()),
            path: "/common/oauth2/v2.0/token".into(),
            query: Some("slice=test".into()),
            status: 400,
            message: None,
            reason: Some("AADSTS70008".into()),
            times: 1,
            after: false,
            retry_after: None,
        });
        let wrong_method: Uri = "/common/oauth2/v2.0/token?slice=test".parse().unwrap();
        assert!(take_fault(&mut world, &Method::GET, &wrong_method).is_none());
        let missing_query: Uri = "/common/oauth2/v2.0/token".parse().unwrap();
        assert!(take_fault(&mut world, &Method::POST, &missing_query).is_none());
        let matching: Uri = "/common/oauth2/v2.0/token?other=1&slice=test"
            .parse()
            .unwrap();
        assert!(take_fault(&mut world, &Method::POST, &matching).is_some());
        assert!(take_fault(&mut world, &Method::POST, &matching).is_none());
    }

    #[tokio::test]
    async fn faults_cover_google_batch_oauth_and_graph_login_filters() {
        let router = crate::router(&Config::default());
        let arm = |fault: Value| {
            let router = router.clone();
            async move {
                let req = Request::builder()
                    .method(Method::POST)
                    .uri("/__mock/faults")
                    .header(header::CONTENT_TYPE, "application/json")
                    .body(Body::from(fault.to_string()))
                    .unwrap();
                assert_eq!(router.oneshot(req).await.unwrap().status(), 204);
            }
        };

        arm(json!({ "path": "/batch/gmail/v1", "status": 503 })).await;
        let batch_req = Request::builder()
            .method(Method::POST)
            .uri("/batch/gmail/v1")
            .body(Body::empty())
            .unwrap();
        assert_eq!(
            router.clone().oneshot(batch_req).await.unwrap().status(),
            503
        );

        arm(
            json!({ "path": "/token", "method": "POST", "status": 400, "reason": "invalid_grant" }),
        )
        .await;
        let token_req = Request::builder()
            .method(Method::POST)
            .uri("/token")
            .header(header::CONTENT_TYPE, "application/x-www-form-urlencoded")
            .body(Body::from("grant_type=refresh_token"))
            .unwrap();
        let token_res = router.clone().oneshot(token_req).await.unwrap();
        assert_eq!(token_res.status(), 400);
        let bytes = to_bytes(token_res.into_body(), usize::MAX).await.unwrap();
        let err: gmail::oauth::OAuthError = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(err.error, "invalid_grant");

        // Graph login checks method and query on armed faults too.
        arm(json!({
            "path": "/common/oauth2/v2.0/authorize",
            "query": "login_hint=blocked%40brightmoor.co",
            "status": 302,
            "reason": "AADSTS65004",
        }))
        .await;
        let allowed_req = Request::builder()
            .uri("/common/oauth2/v2.0/authorize?client_id=c&response_type=code&redirect_uri=http%3A%2F%2F127.0.0.1%3A9%2Fcb&scope=openid")
            .body(Body::empty())
            .unwrap();
        let allowed_res = router.clone().oneshot(allowed_req).await.unwrap();
        assert!(
            allowed_res.headers()[header::LOCATION]
                .to_str()
                .unwrap()
                .contains("code=")
        );
        let blocked_req = Request::builder()
            .uri("/common/oauth2/v2.0/authorize?client_id=c&response_type=code&redirect_uri=http%3A%2F%2F127.0.0.1%3A9%2Fcb&scope=openid&login_hint=blocked%40brightmoor.co")
            .body(Body::empty())
            .unwrap();
        let blocked_res = router.clone().oneshot(blocked_req).await.unwrap();
        assert!(
            blocked_res.headers()[header::LOCATION]
                .to_str()
                .unwrap()
                .contains("error=access_denied")
        );
    }

    #[tokio::test]
    async fn graph_latency_is_held_inside_concurrency_limit() {
        let router = crate::router(&Config {
            latency: Duration::from_millis(30),
            graph_concurrency: Some(1),
            ..Config::default()
        });
        let call = || {
            let router = router.clone();
            async move {
                let req = Request::builder()
                    .uri("/v1.0/me")
                    .header(header::AUTHORIZATION, format!("Bearer {DEV_ACCESS_TOKEN}"))
                    .body(Body::empty())
                    .unwrap();
                router.oneshot(req).await.unwrap().status().as_u16()
            }
        };
        let (a, b) = tokio::join!(call(), call());
        let mut codes = [a, b];
        codes.sort_unstable();
        assert_eq!(codes, [200, 429]);
    }
}

//! What every Graph request goes through: the bearer check, injected faults, Outlook's
//! concurrency limit, and Graph's response conventions (`request-id`, `innerError`, OData's
//! content type).

use std::sync::Arc;

use axum::body::{Body, to_bytes};
use axum::extract::{Request, State};
use axum::http::{HeaderValue, header};
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};
use serde_json::{Value, json};

use super::error::GraphError;
use super::odata::Prefer;
use crate::world::App;

/// OData's JSON, as Graph labels it.
const ODATA_JSON: &str = "application/json;odata.metadata=minimal;odata.streaming=true;IEEE754Compatible=false;charset=utf-8";

/// Rejects requests without a valid `Authorization: Bearer`, and tells handlers who the token
/// signs in ([`super::Account`]).
pub async fn authenticate(
    State(app): State<Arc<App>>,
    mut request: Request,
    next: Next,
) -> Response {
    let who = crate::oauth::bearer(request.headers()).map(|token| {
        let world = app.lock();
        super::login::who(&world, token)
    });
    match who {
        Some(Ok(who)) => {
            request.extensions_mut().insert(who);
            next.run(request).await
        }
        Some(Err(expired)) => unauthenticated(if expired {
            "Lifetime validation failed, the token is expired."
        } else {
            "Access token validation failure. Invalid audience."
        }),
        None => unauthenticated("Access token is empty."),
    }
}

fn unauthenticated(message: &str) -> Response {
    let mut response = GraphError::new(401, "InvalidAuthenticationToken", message).into_response();
    response.headers_mut().insert(
        header::WWW_AUTHENTICATE,
        HeaderValue::from_static(
            "Bearer realm=\"\", authorization_uri=\"https://login.microsoftonline.com/common/oauth2/authorize\", client_id=\"00000003-0000-0000-c000-000000000000\"",
        ),
    );
    response
}

/// Answers matching requests with an armed fault (`/__mock/faults`), in Graph's error body.
/// With `after`, the request takes effect and only its answer is lost.
pub async fn inject_faults(State(app): State<Arc<App>>, request: Request, next: Next) -> Response {
    let fault = crate::http::take_fault(&mut app.lock(), request.method(), request.uri());
    let Some(fault) = fault else {
        return next.run(request).await;
    };
    if fault.after {
        let _ = next.run(request).await;
    }
    let (code, message) = canned(fault.status);
    let mut error = GraphError::new(
        fault.status,
        fault.reason.as_deref().unwrap_or(code),
        fault.message.as_deref().unwrap_or(message),
    );
    if fault.status == 429 || fault.status == 503 {
        error.retry_after = Some(fault.retry_after.unwrap_or(10));
    }
    error.into_response()
}

/// The code and message Graph's mail endpoints typically send with each status.
fn canned(status: u16) -> (&'static str, &'static str) {
    match status {
        400 => ("ErrorInvalidRequest", "The request is invalid."),
        401 => (
            "InvalidAuthenticationToken",
            "Access token validation failure.",
        ),
        403 => (
            "ErrorAccessDenied",
            "Access is denied. Check credentials and try again.",
        ),
        404 => (
            "ErrorItemNotFound",
            "The specified object was not found in the store.",
        ),
        409 => (
            "ErrorIrresolvableConflict",
            "The change key passed in the request does not match the current change key for the item.",
        ),
        410 => (
            "SyncStateNotFound",
            "The sync state is no longer available. Restart the sync with a new delta request.",
        ),
        429 => ("TooManyRequests", "Please retry again later."),
        503 => (
            "ServiceUnavailable",
            "The service is temporarily unavailable. Please retry later.",
        ),
        504 => ("GatewayTimeout", "The server did not respond in time."),
        _ => (
            "ErrorInternalServerError",
            "An internal server error occurred. The operation failed.",
        ),
    }
}

/// Refuses requests beyond the configured number at once (`Config::graph_concurrency`), as
/// Outlook does past four per app and mailbox, and holds each request for `Config::latency` while
/// its concurrency slot is occupied. A batch isn't an Outlook request itself; its subrequests
/// count as they run.
pub async fn limit_concurrency(
    State(app): State<Arc<App>>,
    request: Request,
    next: Next,
) -> Response {
    if request.uri().path() == "/v1.0/$batch" {
        return next.run(request).await;
    }
    let (tracked, latency) = {
        let mut world = app.lock();
        if let Some(limit) = world.config.graph_concurrency {
            if world.graph.in_flight >= limit {
                return GraphError::throttled(
                    "ApplicationThrottled",
                    "Application is over its MailboxConcurrency limit.",
                    1,
                )
                .into_response();
            }
            world.graph.in_flight += 1;
        }
        (
            world.config.graph_concurrency.is_some(),
            world.config.latency,
        )
    };
    let _release = tracked.then(|| Release(app));
    if !latency.is_zero() {
        tokio::time::sleep(latency).await;
    }
    next.run(request).await
}

/// Ends a request's turn under the concurrency limit, however the handler finishes.
struct Release(Arc<App>);

impl Drop for Release {
    fn drop(&mut self) {
        let mut world = self.0.lock();
        world.graph.in_flight = world.graph.in_flight.saturating_sub(1);
    }
}

/// Graph's response conventions: `request-id` and `client-request-id` headers, `innerError` in
/// error bodies, OData's content type, `OData-Version` and `Preference-Applied`.
pub async fn finish(State(app): State<Arc<App>>, request: Request, next: Next) -> Response {
    let (request_id, date) = {
        let mut world = app.lock();
        let n = world.graph.next_request();
        let date = world.clock.now().strftime("%Y-%m-%dT%H:%M:%S").to_string();
        (super::ids::guid(&format!("request {n}")), date)
    };
    let client_id = request
        .headers()
        .get("client-request-id")
        .and_then(|value| value.to_str().ok())
        .map_or_else(|| request_id.clone(), str::to_owned);
    let prefer = Prefer::from_headers(request.headers());
    let response = next.run(request).await;
    let (mut parts, body) = response.into_parts();
    let is_json = parts
        .headers
        .get(header::CONTENT_TYPE)
        .and_then(|value| value.to_str().ok())
        .is_some_and(|value| value.starts_with("application/json"));
    let body = if is_json {
        let bytes = to_bytes(body, usize::MAX).await.unwrap_or_default();
        let mut value: Value = serde_json::from_slice(&bytes).unwrap_or(Value::Null);
        if parts.status.is_success() {
            parts
                .headers
                .insert(header::CONTENT_TYPE, HeaderValue::from_static(ODATA_JSON));
            if let Some(applied) = prefer
                .applied()
                .and_then(|a| HeaderValue::from_str(&a).ok())
            {
                parts.headers.insert("Preference-Applied", applied);
            }
        } else if let Some(error) = value.get_mut("error").and_then(Value::as_object_mut) {
            let inner = error.entry("innerError").or_insert_with(|| json!({}));
            if let Some(inner) = inner.as_object_mut() {
                inner.insert("date".into(), date.into());
                inner.insert("request-id".into(), request_id.clone().into());
                inner.insert("client-request-id".into(), client_id.clone().into());
            }
        }
        parts.headers.remove(header::CONTENT_LENGTH);
        parts
            .headers
            .insert("OData-Version", HeaderValue::from_static("4.0"));
        Body::from(serde_json::to_vec(&value).expect("JSON values serialize"))
    } else {
        body
    };
    for (name, value) in [
        ("request-id", &request_id),
        ("client-request-id", &client_id),
    ] {
        if let Ok(value) = HeaderValue::from_str(value) {
            parts.headers.insert(name, value);
        }
    }
    parts
        .headers
        .insert(header::CACHE_CONTROL, HeaderValue::from_static("no-cache"));
    Response::from_parts(parts, body)
}

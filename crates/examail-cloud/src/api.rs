//! `relay-api`: what the app and the user's browser call, behind IAP.
//!
//! | Route | Who | What |
//! | --- | --- | --- |
//! | `GET /v1/capabilities` | app | What this service can do, so an older app and a newer service agree |
//! | `GET /v1/account` | app | Whether the IAP account has connected, and as whom |
//! | `GET /v1/connect` | browser | Starts the service's own Google consent |
//! | `GET /v1/connect/callback` | browser | Google's redirect back: keeps the sealed refresh token |
//! | `POST /v1/devices` | app | Gives this Mac a device token |
//! | `DELETE /v1/devices/self` | app | Forgets this Mac |
//! | `GET /v1/check` | app | Proves the grant still works by refreshing it |
//! | `DELETE /v1/account` | app | Disconnect: revokes the grant and forgets the account |
//! | `GET /v1/jobs` | app | The account's reminders and scheduled sends, and how each stands |
//! | `PUT /v1/jobs/{key}` | app | Records a reminder or scheduled send, or replaces it ([`crate::jobs`]) |
//! | `DELETE /v1/jobs/{key}` | app | Calls one off |

use std::sync::Arc;

use anyhow::{Context, anyhow};
use axum::Json;
use axum::extract::{Query, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::{Html, IntoResponse, Redirect, Response};
use axum::extract::Path;
use axum::routing::{delete, get, post, put};
use base64::Engine as _;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use serde::Deserialize;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};

use crate::App;
use crate::google::SCOPES;
use crate::iap::Identity;
use crate::store::{Account, Connect, Device, Grant};

/// How long a consent may take between `/v1/connect` and Google's redirect.
const CONNECT_FOR: i64 = 10 * 60;
/// The header a device token travels in. IAP takes `Authorization` for the Google ID token.
const DEVICE_HEADER: &str = "x-examail-device";
/// The API's version, which `GET /v1/capabilities` reports.
const API_VERSION: u32 = 1;

pub fn router(app: Arc<App>) -> axum::Router {
    axum::Router::new()
        .route("/health", get(|| async { "ok" }))
        .route("/v1/capabilities", get(capabilities))
        .route("/v1/account", get(account).delete(disconnect))
        .route("/v1/connect", get(connect))
        .route("/v1/connect/callback", get(callback))
        .route("/v1/devices", post(add_device))
        .route("/v1/devices/self", delete(remove_device))
        .route("/v1/check", get(check))
        .route("/v1/jobs", get(list_jobs))
        .route("/v1/jobs/{key}", put(put_job).delete(delete_job))
        .with_state(app)
}

/// An error as a response: its status, and its message for the app's log.
pub struct Error(StatusCode, anyhow::Error);

impl Error {
    fn new(status: StatusCode, error: impl Into<anyhow::Error>) -> Self {
        Self(status, error.into())
    }
}

impl<E: Into<anyhow::Error>> From<E> for Error {
    fn from(error: E) -> Self {
        Self(StatusCode::INTERNAL_SERVER_ERROR, error.into())
    }
}

impl IntoResponse for Error {
    fn into_response(self) -> Response {
        // A server error's detail can quote Firestore, KMS or Google, so it stays in the log. A
        // client error's message is one of this file's own.
        let message = if self.0.is_server_error() {
            log::error!("{:#}", self.1);
            "internal error".to_owned()
        } else {
            log::info!("{}: {:#}", self.0, self.1);
            format!("{:#}", self.1)
        };
        (self.0, Json(json!({ "error": message }))).into_response()
    }
}

type Result<T> = std::result::Result<T, Error>;

async fn identify(app: &App, headers: &HeaderMap) -> Result<Identity> {
    app.iap
        .identify(headers)
        .await
        .map_err(|error| {
            // Why it failed is for the log; the caller only learns it isn't signed in.
            log::info!("refused an IAP assertion: {error:#}");
            Error::new(StatusCode::UNAUTHORIZED, anyhow!("not signed in"))
        })
}

/// The caller's identity, after checking the device token is theirs.
async fn identify_device(app: &App, headers: &HeaderMap) -> Result<(Identity, String)> {
    let identity = identify(app, headers).await?;
    let token = headers
        .get(DEVICE_HEADER)
        .and_then(|value| value.to_str().ok())
        .ok_or_else(|| Error::new(StatusCode::UNAUTHORIZED, anyhow!("no device token")))?;
    let hash = token_hash(token);
    match app.store.device(&hash).await? {
        Some(device) if device.sub == identity.sub => Ok((identity, hash)),
        _ => Err(Error::new(StatusCode::UNAUTHORIZED, anyhow!("unknown device token"))),
    }
}

fn token_hash(token: &str) -> String {
    Sha256::digest(token.as_bytes())
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

fn random_token() -> String {
    let mut bytes = [0u8; 32];
    getrandom::fill(&mut bytes).expect("the OS random source is available");
    URL_SAFE_NO_PAD.encode(bytes)
}

async fn capabilities() -> Json<Value> {
    Json(json!({
        "version": API_VERSION,
        "service": env!("CARGO_PKG_VERSION"),
        "features": ["connect", "devices", "jobs"],
    }))
}

async fn account(State(app): State<Arc<App>>, headers: HeaderMap) -> Result<Json<Value>> {
    let identity = identify(&app, &headers).await?;
    let account = app.store.account(&identity.sub).await?;
    Ok(Json(json!({
        "email": identity.email,
        "connected": account.as_ref().is_some_and(|account| account.status == "active"),
        "status": account.map(|account| account.status),
    })))
}

/// Starts the consent: Google asks the IAP account to let the service act on its mail.
async fn connect(State(app): State<Arc<App>>, headers: HeaderMap) -> Result<Redirect> {
    let identity = identify(&app, &headers).await?;
    let state = random_token();
    let pkce = gmail::oauth::Pkce::generate();
    let expires_at = jiff::Timestamp::now()
        .checked_add(jiff::Span::new().seconds(CONNECT_FOR))
        .context("a consent's expiry")?;
    app.store
        .put_connect(
            &state,
            &Connect {
                sub: identity.sub.clone(),
                verifier: pkce.verifier.clone(),
                expires_at: expires_at.to_string(),
            },
        )
        .await?;
    let mut url = app
        .google
        .oauth
        .authorization_url(&redirect_uri(&app), SCOPES, &state, &pkce);
    url.query_pairs_mut()
        .append_pair("login_hint", &identity.email);
    Ok(Redirect::to(url.as_str()))
}

fn redirect_uri(app: &App) -> String {
    format!("{}/v1/connect/callback", app.config.public_url)
}

#[derive(Deserialize)]
struct Callback {
    code: Option<String>,
    state: Option<String>,
    error: Option<String>,
}

async fn callback(
    State(app): State<Arc<App>>,
    headers: HeaderMap,
    Query(callback): Query<Callback>,
) -> Result<Html<String>> {
    let identity = identify(&app, &headers).await?;
    if let Some(error) = callback.error {
        return Ok(page("Examail Cloud isn't connected", &format!("Google said: {error}. You can close this tab and try again from Examail.")));
    }
    let (Some(code), Some(state)) = (callback.code, callback.state) else {
        return Err(Error::new(StatusCode::BAD_REQUEST, anyhow!("no code or state")));
    };
    let connect = app
        .store
        .take_connect(&state)
        .await?
        .ok_or_else(|| Error::new(StatusCode::BAD_REQUEST, anyhow!("unknown or used consent")))?;
    let expired = connect
        .expires_at
        .parse::<jiff::Timestamp>()
        .map_or(true, |at| at < jiff::Timestamp::now());
    if connect.sub != identity.sub || expired {
        return Err(Error::new(StatusCode::BAD_REQUEST, anyhow!("the consent isn't this account's, or expired")));
    }
    let tokens = app
        .google
        .oauth
        .exchange_code(&code, &connect.verifier, &redirect_uri(&app))
        .await
        .context("exchanging the consent's code")?;
    // The ID token came straight from Google's token endpoint over TLS, so its claims can be
    // read without checking its signature (OpenID Connect Core 3.1.3.7).
    let granted = tokens
        .id_token
        .as_deref()
        .and_then(id_token_sub)
        .context("Google sent no ID token")?;
    if granted != identity.sub {
        return Ok(page(
            "That's a different Google account",
            &format!("You're signed in to Examail Cloud as {}, but gave access to another account. Close this tab and connect again, choosing {}.", identity.email, identity.email),
        ));
    }
    let refresh = tokens
        .refresh_token
        .context("Google sent no refresh token")?;
    app.store
        .put_grant(
            &identity.sub,
            &Grant {
                sealed_refresh_token: app.google.seal(refresh.as_bytes()).await?,
                scopes: tokens.scope.unwrap_or_default(),
            },
        )
        .await?;
    app.store
        .put_account(
            &identity.sub,
            &Account {
                email: identity.email.clone(),
                status: "active".into(),
                connected_at: jiff::Timestamp::now().to_string(),
            },
        )
        .await?;
    log::info!("connected {}", identity.sub);
    Ok(page(
        "Examail Cloud is connected",
        &format!("{} is connected. You can close this tab and go back to Examail.", identity.email),
    ))
}

/// The `sub` claim of an ID token.
fn id_token_sub(token: &str) -> Option<String> {
    let body = token.split('.').nth(1)?;
    let claims: Value = serde_json::from_slice(&URL_SAFE_NO_PAD.decode(body).ok()?).ok()?;
    claims["sub"].as_str().map(str::to_owned)
}

fn page(title: &str, text: &str) -> Html<String> {
    let escape = |text: &str| {
        text.replace('&', "&amp;")
            .replace('<', "&lt;")
            .replace('>', "&gt;")
    };
    Html(format!(
        "<!doctype html><meta charset=utf-8><meta name=viewport content='width=device-width'>\
         <title>{title}</title><body style='font:16px -apple-system,system-ui,sans-serif;max-width:32rem;margin:15vh auto;padding:0 16px'>\
         <h1 style='font-size:22px'>{title}</h1><p>{text}</p>",
        title = escape(title),
        text = escape(text),
    ))
}

#[derive(Deserialize)]
struct NewDevice {
    name: String,
}

/// Gives a Mac its device token. The account must have connected first.
async fn add_device(
    State(app): State<Arc<App>>,
    headers: HeaderMap,
    Json(device): Json<NewDevice>,
) -> Result<(StatusCode, Json<Value>)> {
    let identity = identify(&app, &headers).await?;
    let connected = app
        .store
        .account(&identity.sub)
        .await?
        .is_some_and(|account| account.status == "active");
    if !connected {
        return Err(Error::new(StatusCode::CONFLICT, anyhow!("connect the account first")));
    }
    let token = random_token();
    app.store
        .put_device(
            &token_hash(&token),
            &Device {
                sub: identity.sub,
                name: device.name.chars().take(100).collect(),
                created_at: jiff::Timestamp::now().to_string(),
            },
        )
        .await?;
    Ok((StatusCode::CREATED, Json(json!({ "token": token }))))
}

async fn remove_device(State(app): State<Arc<App>>, headers: HeaderMap) -> Result<StatusCode> {
    let (_, hash) = identify_device(&app, &headers).await?;
    app.store.delete_device(&hash).await?;
    Ok(StatusCode::NO_CONTENT)
}

/// Refreshes the account's grant, proving the service can still act on its mail.
async fn check(State(app): State<Arc<App>>, headers: HeaderMap) -> Result<Json<Value>> {
    let (identity, _) = identify_device(&app, &headers).await?;
    let grant = app
        .store
        .grant(&identity.sub)
        .await?
        .ok_or_else(|| Error::new(StatusCode::CONFLICT, anyhow!("not connected")))?;
    let refresh = String::from_utf8(app.google.open(&grant.sealed_refresh_token).await?)?;
    match app.google.oauth.refresh(&refresh).await {
        Ok(_) => Ok(Json(json!({ "ok": true, "email": identity.email }))),
        Err(gmail::Error::OAuth(error)) if error.error == "invalid_grant" => {
            if let Some(mut account) = app.store.account(&identity.sub).await? {
                account.status = "revoked".into();
                app.store.put_account(&identity.sub, &account).await?;
            }
            Ok(Json(json!({ "ok": false, "status": "revoked" })))
        }
        Err(error) => Err(error.into()),
    }
}

/// Disconnect: revokes the service's grant at Google and forgets the account.
async fn disconnect(State(app): State<Arc<App>>, headers: HeaderMap) -> Result<StatusCode> {
    let identity = identify(&app, &headers).await?;
    if let Some(grant) = app.store.grant(&identity.sub).await? {
        let refresh = String::from_utf8(app.google.open(&grant.sealed_refresh_token).await?)?;
        match app.google.oauth.revoke(&refresh).await {
            Ok(()) => {}
            // Already revoked, as when the user removed access in their Google account.
            Err(gmail::Error::OAuth(error)) if error.error == "invalid_token" => {}
            Err(error) => return Err(error.into()),
        }
    }
    app.store.forget(&identity.sub).await?;
    log::info!("disconnected {}", identity.sub);
    Ok(StatusCode::NO_CONTENT)
}

/// The account's jobs, so the app can learn what fired while it was closed.
async fn list_jobs(State(app): State<Arc<App>>, headers: HeaderMap) -> Result<Json<Value>> {
    let (identity, _) = identify_device(&app, &headers).await?;
    let jobs = app.store.jobs(&identity.sub).await?;
    Ok(Json(json!({ "jobs": jobs })))
}

/// Records `key`'s reminder or scheduled send, replacing what was there.
async fn put_job(
    State(app): State<Arc<App>>,
    headers: HeaderMap,
    Path(key): Path<String>,
    Json(request): Json<crate::jobs::Request>,
) -> Result<Json<Value>> {
    let (identity, _) = identify_device(&app, &headers).await?;
    if !crate::jobs::valid_key(&key) {
        return Err(Error::new(StatusCode::BAD_REQUEST, anyhow!("a key is 1–200 of A–Z, a–z, 0–9, -, _, . and :")));
    }
    if let Some(why) = crate::jobs::refuse(&request, jiff::Timestamp::now()) {
        return Err(Error::new(StatusCode::UNPROCESSABLE_ENTITY, anyhow!("{why}")));
    }
    let id = crate::jobs::document_id(&identity.sub, &key);
    let mut job = crate::jobs::Job::new(&identity.sub, &key, request);
    match app.store.job(&id).await? {
        // A job that already finished isn't brought back by the app resending it.
        Some(existing) if existing.status != crate::jobs::Status::Pending && existing.at == job.at => {
            return Ok(Json(json!({ "job": existing })));
        }
        // Resending a waiting job keeps its attempts, so it can't be retried forever.
        Some(existing) if existing.status == crate::jobs::Status::Pending => {
            job.attempts = existing.attempts;
        }
        // A new job, or a finished one set for a new time: either adds one waiting.
        Some(_) | None => {
            let pending = app
                .store
                .jobs(&identity.sub)
                .await?
                .into_iter()
                .filter(|job| job.status == crate::jobs::Status::Pending)
                .count();
            // Counted, not enforced atomically: two requests at once may both pass at the limit,
            // which only an account's own Macs, behind IAP, can do.
            if pending >= crate::jobs::MOST_PENDING {
                return Err(Error::new(
                    StatusCode::TOO_MANY_REQUESTS,
                    anyhow!("an account may have {} jobs waiting", crate::jobs::MOST_PENDING),
                ));
            }
        }
    }
    app.store.put_job(&id, &job).await?;
    Ok(Json(json!({ "job": job })))
}

async fn delete_job(
    State(app): State<Arc<App>>,
    headers: HeaderMap,
    Path(key): Path<String>,
) -> Result<StatusCode> {
    let (identity, _) = identify_device(&app, &headers).await?;
    if !crate::jobs::valid_key(&key) {
        return Err(Error::new(StatusCode::BAD_REQUEST, anyhow!("not a job key")));
    }
    app.store
        .delete_job(&crate::jobs::document_id(&identity.sub, &key))
        .await?;
    Ok(StatusCode::NO_CONTENT)
}

#[cfg(test)]
mod tests {
    use base64::Engine as _;
    use base64::engine::general_purpose::URL_SAFE_NO_PAD;

    use super::{id_token_sub, token_hash};

    #[test]
    fn an_id_tokens_sub_is_read() {
        let body = URL_SAFE_NO_PAD.encode(br#"{"sub":"1234","email":"me@example.com"}"#);
        assert_eq!(id_token_sub(&format!("h.{body}.s")).as_deref(), Some("1234"));
        assert_eq!(id_token_sub("not a token"), None);
    }

    #[test]
    fn tokens_hash_to_hex() {
        assert_eq!(token_hash("abc").len(), 64);
        assert_ne!(token_hash("abc"), token_hash("abd"));
    }
}

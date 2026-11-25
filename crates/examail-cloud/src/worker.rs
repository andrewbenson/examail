//! `relay-worker`: what Google's own services call. Cloud Run lets only the Cloud Scheduler
//! and Pub/Sub service accounts invoke it, so these routes check nothing themselves.
//!
//! - `POST /internal/tick`, every minute from Cloud Scheduler: fires due jobs ([`crate::jobs`]).
//! - `POST /hooks/gmail`, from the Gmail push subscription: new mail ([`crate::watch`]).

use std::sync::Arc;

use axum::http::StatusCode;
use axum::routing::{get, post};

use crate::App;

pub fn router(app: Arc<App>) -> axum::Router {
    axum::Router::new()
        .route("/health", get(|| async { "ok" }))
        .route("/internal/tick", post(tick))
        .route("/hooks/gmail", post(gmail))
        .with_state(app)
}

async fn tick(axum::extract::State(app): axum::extract::State<Arc<App>>) -> StatusCode {
    // Jobs first, so mail reading never makes a send late; the watcher and split labels have
    // their own troubles, which don't fail the tick.
    let jobs = crate::jobs::tick(&app).await;
    if let Err(error) = crate::watch::sweep(&app).await {
        log::error!("watching mail: {error:#}");
    }
    if let Err(error) = crate::splits::tick(&app).await {
        log::error!("split labels: {error:#}");
    }
    match jobs {
        Ok(0) => StatusCode::NO_CONTENT,
        Ok(finished) => {
            log::info!("tick finished {finished} jobs");
            StatusCode::NO_CONTENT
        }
        // Cloud Scheduler logs the failure; the next tick tries again.
        Err(error) => {
            log::error!("tick: {error:#}");
            StatusCode::INTERNAL_SERVER_ERROR
        }
    }
}

/// A push always answers success: what it couldn't do, the tick's sweep does within a minute,
/// and a failure would only have Pub/Sub send it again and again.
async fn gmail(
    axum::extract::State(app): axum::extract::State<Arc<App>>,
    body: axum::body::Bytes,
) -> StatusCode {
    let Some(email) = crate::watch::pushed_email(&body) else {
        log::warn!("a push that doesn't read ({} bytes)", body.len());
        return StatusCode::NO_CONTENT;
    };
    if let Err(error) = crate::watch::pushed(&app, &email).await {
        log::warn!("acting on a push: {error:#}");
    }
    StatusCode::NO_CONTENT
}

//! `relay-worker`: what Google's own services call. Cloud Run lets only the Cloud Scheduler
//! and Pub/Sub service accounts invoke it, so these routes check nothing themselves.
//!
//! - `POST /internal/tick`, every minute from Cloud Scheduler: fires due jobs (phase 2).
//! - `POST /hooks/gmail`, from the Gmail push subscription: new mail (phase 4).

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

async fn tick() -> StatusCode {
    log::debug!("tick");
    StatusCode::NO_CONTENT
}

async fn gmail(body: axum::body::Bytes) -> StatusCode {
    log::info!("gmail push ({} bytes)", body.len());
    StatusCode::NO_CONTENT
}

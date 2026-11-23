//! `relay-worker`: what Google's own services call. Cloud Run lets only the Cloud Scheduler
//! and Pub/Sub service accounts invoke it, so these routes check nothing themselves.
//!
//! - `POST /internal/tick`, every minute from Cloud Scheduler: fires due jobs ([`crate::jobs`]).
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

async fn tick(axum::extract::State(app): axum::extract::State<Arc<App>>) -> StatusCode {
    match crate::jobs::tick(&app).await {
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

async fn gmail(body: axum::body::Bytes) -> StatusCode {
    log::info!("gmail push ({} bytes)", body.len());
    StatusCode::NO_CONTENT
}

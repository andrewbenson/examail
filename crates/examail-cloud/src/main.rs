//! Examail Cloud: the service that does what the app can only do while it runs (bringing back
//! snoozed threads, sending scheduled mail, applying mail rules) when the Mac is closed.
//!
//! One binary runs as either of two Cloud Run services, chosen by `ROLE`:
//!
//! - `api` (`relay-api`), behind IAP: what the app and the user's browser call. IAP admits
//!   only the listed Google accounts, and every request carries IAP's signed assertion of which
//!   one ([`iap`]).
//! - `worker` (`relay-worker`), callable only by the Pub/Sub and Cloud Scheduler service
//!   accounts (Cloud Run's own sign-in): Gmail push and the per-minute tick.
//!
//! The design is in the "Examail Cloud" design doc.

mod api;
mod config;
mod firestore;
mod google;
mod iap;
mod jobs;
mod store;
mod worker;

use std::sync::Arc;

use anyhow::{Context, Result};

use crate::config::{Config, Role};

/// What every request handler shares.
pub struct App {
    pub config: Config,
    pub http: reqwest::Client,
    pub google: google::Google,
    pub store: store::Store,
    pub iap: iap::Verifier,
}

#[tokio::main]
async fn main() -> Result<()> {
    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("info")).init();
    let config = Config::from_env()?;
    let http = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(30))
        .build()
        .context("building the HTTP client")?;
    let google = google::Google::new(http.clone(), &config);
    let store = store::Store::new(firestore::Firestore::new(
        http.clone(),
        google.clone(),
        &config.project_id,
        &config.database,
    ));
    let iap = iap::Verifier::new(http.clone(), config.iap_audience.clone());
    let role = config.role;
    let port = config.port;
    let app = Arc::new(App {
        config,
        http,
        google,
        store,
        iap,
    });
    let router = match role {
        Role::Api => api::router(app),
        Role::Worker => worker::router(app),
    };
    let listener = tokio::net::TcpListener::bind(("0.0.0.0", port))
        .await
        .with_context(|| format!("listening on port {port}"))?;
    log::info!("examail-cloud {} ({role:?}) on port {port}", env!("CARGO_PKG_VERSION"));
    axum::serve(listener, router).await?;
    Ok(())
}

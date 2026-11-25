//! The service's settings, from its environment (set by `cloud/deploy`).

use anyhow::{Context, Result, bail};

/// Which of the two services this instance is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Role {
    Api,
    Worker,
}

pub struct Config {
    pub role: Role,
    pub port: u16,
    pub project_id: String,
    /// The Firestore database (`relay`), apart from any other app's in the project.
    pub database: String,
    /// The Cloud KMS key that seals refresh tokens
    /// (`projects/…/locations/…/keyRings/…/cryptoKeys/…`).
    pub kms_key: String,
    /// The audience IAP signs its assertions for. Unset only for local runs, which then trust
    /// `X-Examail-Dev-User` instead.
    pub iap_audience: Option<String>,
    /// The `Examail Cloud` web OAuth client the service's own Google consent runs through.
    pub oauth_client_id: String,
    pub oauth_client_secret: String,
    /// The API's public URL, which the consent's redirect comes back to.
    pub public_url: String,
    /// The Pub/Sub topic Gmail publishes mailbox changes to (`projects/…/topics/…`). Unset, the
    /// mail watcher is off ([`crate::watch`]).
    pub gmail_topic: Option<String>,
}

impl Config {
    pub fn from_env() -> Result<Self> {
        let role = match var("ROLE")?.as_str() {
            "api" => Role::Api,
            "worker" => Role::Worker,
            other => bail!("ROLE is {other:?}, not api or worker"),
        };
        let port = std::env::var("PORT")
            .ok()
            .map(|port| port.parse().context("PORT isn't a port"))
            .transpose()?
            .unwrap_or(8080);
        let iap_audience = std::env::var("IAP_AUDIENCE")
            .ok()
            .filter(|aud| !aud.is_empty());
        if iap_audience.is_none() && role == Role::Api && std::env::var("K_SERVICE").is_ok() {
            bail!("IAP_AUDIENCE must be set on Cloud Run");
        }
        Ok(Self {
            role,
            port,
            project_id: var("PROJECT_ID")?,
            database: std::env::var("FIRESTORE_DATABASE").unwrap_or_else(|_| "relay".into()),
            kms_key: var("KMS_KEY")?,
            iap_audience,
            oauth_client_id: std::env::var("OAUTH_CLIENT_ID").unwrap_or_default(),
            oauth_client_secret: std::env::var("OAUTH_CLIENT_SECRET").unwrap_or_default(),
            public_url: std::env::var("PUBLIC_URL")
                .unwrap_or_default()
                .trim_end_matches('/')
                .to_owned(),
            gmail_topic: std::env::var("GMAIL_TOPIC")
                .ok()
                .filter(|topic| !topic.is_empty()),
        })
    }
}

fn var(name: &str) -> Result<String> {
    std::env::var(name).with_context(|| format!("{name} isn't set"))
}

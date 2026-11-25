//! The service's own Google credentials (its service account's access token, from the metadata
//! server), Cloud KMS, and the OAuth client its users' grants come from.

use std::sync::Arc;
use std::time::{Duration, Instant};

use anyhow::{Context, Result, bail};
use base64::Engine as _;
use base64::engine::general_purpose::STANDARD;
use serde::Deserialize;
use tokio::sync::Mutex;

use crate::config::Config;

/// Scopes the service's own consent asks for: Gmail to bring threads back and send drafts, and
/// who the account is. Less than the app asks for, since the service never reads calendars,
/// contacts or settings.
pub const SCOPES: &[&str] = &[
    gmail::oauth::scopes::GMAIL_MODIFY,
    gmail::oauth::scopes::OPENID,
    gmail::oauth::scopes::USERINFO_EMAIL,
];

#[derive(Clone)]
pub struct Google {
    http: reqwest::Client,
    token: Arc<Mutex<Option<(String, Instant)>>>,
    kms_key: String,
    pub oauth: gmail::oauth::OAuthClient,
}

impl Google {
    pub fn new(http: reqwest::Client, config: &Config) -> Self {
        Self {
            http,
            token: Arc::default(),
            kms_key: config.kms_key.clone(),
            oauth: gmail::oauth::OAuthClient::new(
                gmail::Endpoints::google().oauth,
                config.oauth_client_id.clone(),
                Some(config.oauth_client_secret.clone()),
            ),
        }
    }

    /// The service account's access token for Google Cloud APIs, cached until a minute before it
    /// expires. Off Cloud Run (local runs) it's `GCP_ACCESS_TOKEN`.
    pub async fn access_token(&self) -> Result<String> {
        let mut cached = self.token.lock().await;
        if let Some((token, until)) = cached.as_ref()
            && Instant::now() < *until
        {
            return Ok(token.clone());
        }
        if let Ok(token) = std::env::var("GCP_ACCESS_TOKEN") {
            return Ok(token);
        }
        #[derive(Deserialize)]
        struct Token {
            access_token: String,
            expires_in: u64,
        }
        let token: Token = self
            .http
            .get("http://metadata.google.internal/computeMetadata/v1/instance/service-accounts/default/token")
            .header("Metadata-Flavor", "Google")
            .send()
            .await
            .context("asking the metadata server for a token")?
            .error_for_status()?
            .json()
            .await?;
        let until = Instant::now() + Duration::from_secs(token.expires_in.saturating_sub(60));
        *cached = Some((token.access_token.clone(), until));
        Ok(token.access_token)
    }

    /// Seals `plaintext` with the service's KMS key.
    pub async fn seal(&self, plaintext: &[u8]) -> Result<String> {
        let body = serde_json::json!({ "plaintext": STANDARD.encode(plaintext) });
        let answer = self.kms("encrypt", body).await?;
        answer["ciphertext"]
            .as_str()
            .map(str::to_owned)
            .context("KMS answered without a ciphertext")
    }

    /// Opens what [`Self::seal`] sealed.
    pub async fn open(&self, ciphertext: &str) -> Result<Vec<u8>> {
        let answer = self
            .kms("decrypt", serde_json::json!({ "ciphertext": ciphertext }))
            .await?;
        let plaintext = answer["plaintext"]
            .as_str()
            .context("KMS answered without a plaintext")?;
        Ok(STANDARD.decode(plaintext)?)
    }

    async fn kms(&self, verb: &str, body: serde_json::Value) -> Result<serde_json::Value> {
        let response = self
            .http
            .post(format!(
                "https://cloudkms.googleapis.com/v1/{}:{verb}",
                self.kms_key
            ))
            .bearer_auth(self.access_token().await?)
            .json(&body)
            .send()
            .await
            .with_context(|| format!("calling KMS {verb}"))?;
        let status = response.status();
        if !status.is_success() {
            bail!(
                "KMS {verb} failed ({status}): {}",
                response.text().await.unwrap_or_default()
            );
        }
        Ok(response.json().await?)
    }
}

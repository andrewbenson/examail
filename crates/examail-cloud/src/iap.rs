//! Who's calling the API, from IAP's signed assertion (`x-goog-iap-jwt-assertion`). IAP only
//! lets listed Google accounts through, so a valid assertion is the allowlist; checking its
//! signature and audience here means a request that somehow reached the service around IAP is
//! still refused.

use std::sync::Arc;
use std::time::{Duration, Instant};

use anyhow::{Context, Result, bail, ensure};
use base64::Engine as _;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use serde::Deserialize;
use tokio::sync::RwLock;

const KEYS_URL: &str = "https://www.gstatic.com/iap/verify/public_key-jwk";
const ISSUER: &str = "https://cloud.google.com/iap";
/// How long fetched keys are trusted before they're fetched again. IAP rotates them rarely, and
/// an unknown key id fetches them at once.
const KEYS_FOR: Duration = Duration::from_secs(6 * 60 * 60);
/// Clock skew allowed on `iat` and `exp`.
const LEEWAY: i64 = 60;

/// The Google account IAP vouched for.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Identity {
    /// Google's permanent account id (the `sub` claim without IAP's `accounts.google.com:`).
    pub sub: String,
    pub email: String,
}

pub struct Verifier {
    http: reqwest::Client,
    /// `None` for local runs, which trust `X-Examail-Dev-User` instead.
    audience: Option<String>,
    keys: RwLock<Option<(Arc<Vec<Key>>, Instant)>>,
}

#[derive(Debug, Clone, Deserialize)]
struct Key {
    kid: String,
    x: String,
    y: String,
}

#[derive(Deserialize)]
struct Keys {
    keys: Vec<Key>,
}

#[derive(Deserialize)]
struct Header {
    alg: String,
    kid: String,
}

#[derive(Deserialize)]
struct Claims {
    iss: String,
    aud: String,
    sub: String,
    email: String,
    exp: i64,
    iat: i64,
}

impl Verifier {
    pub fn new(http: reqwest::Client, audience: Option<String>) -> Self {
        Self {
            http,
            audience,
            keys: RwLock::new(None),
        }
    }

    /// The caller of a request with these headers.
    pub async fn identify(&self, headers: &axum::http::HeaderMap) -> Result<Identity> {
        let Some(audience) = &self.audience else {
            // A local run: no IAP in front, so the caller names themselves.
            let user = header(headers, "x-examail-dev-user").context("no X-Examail-Dev-User")?;
            let (sub, email) = user.split_once(':').unwrap_or((user, user));
            return Ok(Identity {
                sub: sub.to_owned(),
                email: email.to_owned(),
            });
        };
        let assertion = header(headers, "x-goog-iap-jwt-assertion").context("no IAP assertion")?;
        self.verify(assertion, audience, jiff::Timestamp::now().as_second())
            .await
    }

    async fn verify(&self, assertion: &str, audience: &str, now: i64) -> Result<Identity> {
        let mut parts = assertion.split('.');
        let (Some(head), Some(body), Some(signature), None) =
            (parts.next(), parts.next(), parts.next(), parts.next())
        else {
            bail!("the assertion isn't a JWT");
        };
        let header: Header = serde_json::from_slice(&URL_SAFE_NO_PAD.decode(head)?)?;
        ensure!(
            header.alg == "ES256",
            "the assertion is signed {}",
            header.alg
        );
        let key = self.key(&header.kid).await?;
        let point = [
            &[4u8][..],
            &URL_SAFE_NO_PAD.decode(&key.x)?,
            &URL_SAFE_NO_PAD.decode(&key.y)?,
        ]
        .concat();
        ring::signature::UnparsedPublicKey::new(&ring::signature::ECDSA_P256_SHA256_FIXED, point)
            .verify(
                format!("{head}.{body}").as_bytes(),
                &URL_SAFE_NO_PAD.decode(signature)?,
            )
            .map_err(|_| anyhow::anyhow!("the assertion's signature doesn't verify"))?;
        let claims: Claims = serde_json::from_slice(&URL_SAFE_NO_PAD.decode(body)?)?;
        check(&claims, audience, now)
    }

    /// IAP's key `kid`, fetching the keys when they're stale or don't have it.
    async fn key(&self, kid: &str) -> Result<Key> {
        if let Some((keys, at)) = self.keys.read().await.as_ref()
            && at.elapsed() < KEYS_FOR
            && let Some(key) = keys.iter().find(|key| key.kid == kid)
        {
            return Ok(key.clone());
        }
        let keys: Keys = self
            .http
            .get(KEYS_URL)
            .send()
            .await
            .context("fetching IAP's keys")?
            .error_for_status()?
            .json()
            .await?;
        let keys = Arc::new(keys.keys);
        *self.keys.write().await = Some((keys.clone(), Instant::now()));
        keys.iter()
            .find(|key| key.kid == kid)
            .cloned()
            .with_context(|| format!("IAP has no key {kid}"))
    }
}

fn check(claims: &Claims, audience: &str, now: i64) -> Result<Identity> {
    ensure!(claims.iss == ISSUER, "the assertion is from {}", claims.iss);
    ensure!(
        claims.aud == audience,
        "the assertion is for {}",
        claims.aud
    );
    ensure!(claims.exp + LEEWAY > now, "the assertion expired");
    ensure!(
        claims.iat - LEEWAY <= now,
        "the assertion is from the future"
    );
    let sub = claims
        .sub
        .strip_prefix("accounts.google.com:")
        .unwrap_or(&claims.sub);
    ensure!(!sub.is_empty(), "the assertion names no account");
    Ok(Identity {
        sub: sub.to_owned(),
        email: claims.email.clone(),
    })
}

fn header<'a>(headers: &'a axum::http::HeaderMap, name: &str) -> Option<&'a str> {
    headers.get(name).and_then(|value| value.to_str().ok())
}

#[cfg(test)]
mod tests {
    use super::{Claims, check};

    fn claims() -> Claims {
        Claims {
            iss: "https://cloud.google.com/iap".into(),
            aud: "/projects/1/locations/us-central1/services/examail-api".into(),
            sub: "accounts.google.com:1234".into(),
            email: "me@example.com".into(),
            exp: 1_000,
            iat: 400,
        }
    }

    #[test]
    fn a_good_assertion_names_its_account() {
        let identity = check(
            &claims(),
            "/projects/1/locations/us-central1/services/examail-api",
            500,
        )
        .unwrap();
        assert_eq!(identity.sub, "1234");
        assert_eq!(identity.email, "me@example.com");
    }

    #[test]
    fn another_audience_issuer_or_time_is_refused() {
        let audience = "/projects/1/locations/us-central1/services/examail-api";
        assert!(
            check(
                &claims(),
                "/projects/1/locations/us-central1/services/other",
                500
            )
            .is_err()
        );
        assert!(
            check(
                &Claims {
                    iss: "https://evil".into(),
                    ..claims()
                },
                audience,
                500
            )
            .is_err()
        );
        assert!(check(&claims(), audience, 2_000).is_err(), "expired");
        assert!(
            check(&claims(), audience, 100).is_err(),
            "issued in the future"
        );
    }
}

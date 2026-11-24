//! Access tokens for one account, minted from its refresh token.

use std::sync::{Arc, Mutex, MutexGuard};

use gmail::oauth::OAuthClient;
use gmail::{TokenFuture, TokenProvider};
use jiff::{SignedDuration, Timestamp};

use crate::Error;

/// How long before expiry a token is refreshed.
const MARGIN: SignedDuration = SignedDuration::from_mins(5);

/// One account's access tokens: a [`TokenProvider`] for its Gmail, Calendar and People clients.
/// Cheap to clone, and clones share the cached token.
///
/// The token is cached until five minutes before it expires, then refreshed on the next request.
/// Concurrent requests wait for one refresh. A 401 for the token
/// ([`invalidate`](TokenProvider::invalidate)) drops it, but a 401 for a token it already
/// replaced changes nothing. A refresh that finishes after the account was removed hands out
/// nothing.
/// When Google says the grant was revoked or expired, every request fails with
/// [`Error::SignInAgain`] (inside [`gmail::Error::Token`]) without calling Google, and
/// [`needs_sign_in`](Self::needs_sign_in) turns true, until the account signs in again.
#[derive(Clone)]
pub struct Tokens(Arc<Inner>);

struct Inner {
    email: String,
    oauth: OAuthClient,
    state: Mutex<State>,
    /// Held while refreshing, so concurrent requests share one refresh.
    refreshing: tokio::sync::Mutex<()>,
}

struct State {
    /// The refresh token, or `None` once Google rejected it (or when there never was one).
    grant: Option<String>,
    access: Option<Access>,
    /// The OpenID Connect ID token the last refresh came with, and when to stop using it. Only a
    /// grant with `openid` gets one.
    id: Option<Access>,
}

/// An access token and when to stop using it.
pub(crate) struct Access {
    token: String,
    refresh_at: Timestamp,
}

impl Access {
    /// A token Google issued at (or after) `issued` that lives `expires_in` seconds.
    pub(crate) fn new(token: String, issued: Timestamp, expires_in: u64) -> Self {
        let lifetime = SignedDuration::from_secs(i64::try_from(expires_in).unwrap_or(i64::MAX));
        let refresh_at = issued
            .checked_add(lifetime.saturating_sub(MARGIN))
            .unwrap_or(Timestamp::MAX);
        Self { token, refresh_at }
    }

    fn usable_at(&self, now: Timestamp) -> bool {
        now < self.refresh_at
    }
}

/// What a request does next.
enum Next {
    Use(String),
    Refresh(String),
}

impl std::fmt::Debug for Tokens {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Tokens")
            .field("email", &self.0.email)
            .finish_non_exhaustive()
    }
}

impl Tokens {
    /// Tokens for `email` from `grant`, its refresh token. `None` means it has to sign in again.
    pub(crate) fn new(email: &str, oauth: OAuthClient, grant: Option<String>) -> Self {
        Self(Arc::new(Inner {
            email: email.to_owned(),
            oauth,
            state: Mutex::new(State {
                grant,
                access: None,
                id: None,
            }),
            refreshing: tokio::sync::Mutex::new(()),
        }))
    }

    /// Google rejected the refresh token, so the account has to sign in again.
    #[must_use]
    pub fn needs_sign_in(&self) -> bool {
        self.state().grant.is_none()
    }

    /// The account signed in again (or just signed in), so use its new grant from now on.
    pub(crate) fn replace(&self, grant: String, access: Access) {
        let mut state = self.state();
        state.grant = Some(grant);
        state.access = Some(access);
        // The old grant's ID token; the next one comes from the new grant.
        state.id = None;
    }

    /// An OpenID Connect ID token for the account, whose audience is Examail's OAuth client:
    /// what Examail Cloud's gateway (IAP) admits it by. `None` when the account's grant has no
    /// `openid`, which signing in again adds.
    ///
    /// # Errors
    /// As for an access token: [`Error::SignInAgain`] inside [`gmail::Error::Token`], or the
    /// refresh's own.
    pub async fn id_token(&self) -> Result<Option<String>, gmail::Error> {
        if let Some(id) = self.usable_id() {
            return Ok(Some(id));
        }
        {
            // The next request refreshes, and a refresh brings a new ID token.
            let mut state = self.state();
            state.access = None;
        }
        self.access().await?;
        Ok(self.usable_id())
    }

    fn usable_id(&self) -> Option<String> {
        self.state()
            .id
            .as_ref()
            .filter(|id| id.usable_at(Timestamp::now()))
            .map(|id| id.token.clone())
    }

    /// The account was removed, so stop handing out tokens.
    pub(crate) fn revoke(&self) {
        let mut state = self.state();
        state.grant = None;
        state.access = None;
        state.id = None;
    }

    fn state(&self) -> MutexGuard<'_, State> {
        self.0.state.lock().expect("token state is never poisoned")
    }

    fn sign_in_again(&self) -> gmail::Error {
        gmail::Error::Token(Box::new(Error::SignInAgain {
            email: self.0.email.clone(),
        }))
    }

    fn next(&self) -> Result<Next, gmail::Error> {
        let state = self.state();
        let Some(grant) = &state.grant else {
            return Err(self.sign_in_again());
        };
        Ok(match &state.access {
            Some(access) if access.usable_at(Timestamp::now()) => Next::Use(access.token.clone()),
            _ => Next::Refresh(grant.clone()),
        })
    }

    async fn access(&self) -> Result<String, gmail::Error> {
        if let Next::Use(token) = self.next()? {
            return Ok(token);
        }
        let _refreshing = self.0.refreshing.lock().await;
        loop {
            // Whoever held the lock may have refreshed already.
            let grant = match self.next()? {
                Next::Use(token) => return Ok(token),
                Next::Refresh(grant) => grant,
            };
            let issued = Timestamp::now();
            let result = self.0.oauth.refresh(&grant).await;
            let mut state = self.state();
            // The account was removed or signed in again meanwhile, so Google's answer is about
            // a grant it no longer has. Start over from what it has now.
            if state.grant.as_deref() != Some(grant.as_str()) {
                continue;
            }
            return match result {
                Ok(response) => {
                    state.access = Some(Access::new(
                        response.access_token.clone(),
                        issued,
                        response.expires_in,
                    ));
                    if let Some(id) = &response.id_token {
                        let lifetime = expires_in(id, issued).unwrap_or(response.expires_in);
                        state.id = Some(Access::new(id.clone(), issued, lifetime));
                    }
                    Ok(response.access_token)
                }
                Err(gmail::Error::OAuth(error)) if error.error == "invalid_grant" => {
                    log::warn!("Google rejected an account's refresh token: {error}");
                    state.grant = None;
                    state.access = None;
                    Err(self.sign_in_again())
                }
                Err(error) => Err(error),
            };
        }
    }
}

/// How many seconds after `issued` ID token `token` expires, from its `exp` claim.
fn expires_in(token: &str, issued: Timestamp) -> Option<u64> {
    use base64::Engine as _;
    let payload = token.split('.').nth(1)?;
    let claims: serde_json::Value = serde_json::from_slice(
        &base64::engine::general_purpose::URL_SAFE_NO_PAD
            .decode(payload.trim_end_matches('='))
            .ok()?,
    )
    .ok()?;
    let exp = claims["exp"].as_i64()?;
    u64::try_from(exp - issued.as_second()).ok()
}

impl TokenProvider for Tokens {
    fn access_token(&self) -> TokenFuture<'_> {
        Box::pin(self.access())
    }

    fn invalidate(&self, token: &str) {
        let mut state = self.state();
        if state
            .access
            .as_ref()
            .is_some_and(|access| access.token == token)
        {
            state.access = None;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_id_tokens_lifetime_comes_from_its_exp() {
        use base64::Engine as _;
        let issued: Timestamp = "2026-11-17T14:00:00Z".parse().unwrap();
        let claims = format!(r#"{{"exp":{}}}"#, issued.as_second() + 3600);
        let token = format!(
            "e30.{}.sig",
            base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(claims)
        );
        assert_eq!(expires_in(&token, issued), Some(3600));
        assert_eq!(expires_in("not-a-jwt", issued), None);
    }

    #[test]
    fn tokens_are_refreshed_five_minutes_before_they_expire() {
        let issued: Timestamp = "2026-11-17T14:00:00Z".parse().unwrap();
        let access = Access::new("ya29.x".into(), issued, 3599);
        let at = |offset: i64| {
            issued
                .checked_add(SignedDuration::from_secs(offset))
                .unwrap()
        };
        assert!(access.usable_at(issued));
        assert!(access.usable_at(at(3599 - 301)));
        assert!(!access.usable_at(at(3599 - 300)));
        assert!(!access.usable_at(at(4000)));
        // A token shorter-lived than the margin is never reused.
        assert!(!Access::new("ya29.y".into(), issued, 60).usable_at(issued));
    }
}

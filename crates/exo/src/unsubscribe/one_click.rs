//! One-click unsubscribes (RFC 8058), posted from the store's queue
//! ([`exo_store::Store::queue_unsubscribe`]).
//!
//! The link is someone else's, so the POST is only `List-Unsubscribe=One-Click`: no cookies,
//! no credentials, no proxy, no redirects followed, and only to an address the account may
//! reach ([`Reach`]), checked on the addresses the name resolves to, which are the ones
//! connected to. A redirect still counts as done, because the sender got the POST, which is all
//! RFC 8058 asks of it. A failed POST shows nothing. A failure that retrying could fix is retried
//! for a week, and any other is logged and dropped.

use std::net::IpAddr;
use std::sync::LazyLock;
use std::time::Duration;

use exo_store::Store;
use jiff::{SignedDuration, Timestamp};
use url::Url;

use crate::remote_content::{self, Scope};

/// What RFC 8058 posts.
const BODY: &str = "List-Unsubscribe=One-Click";
/// How long a POST may take.
const TIMEOUT: Duration = Duration::from_secs(30);
/// The first retry's wait, doubled after each failure up to [`MAX_BACKOFF`].
const BACKOFF: SignedDuration = SignedDuration::from_secs(5);
const MAX_BACKOFF: SignedDuration = SignedDuration::from_mins(5);
/// How long an unsubscribe keeps being retried.
const GIVE_UP: SignedDuration = SignedDuration::from_hours(7 * 24);

static PUBLIC_CLIENT: LazyLock<remote_content::Client> =
    LazyLock::new(|| remote_content::Client::no_redirects(TIMEOUT, Scope::Public, true));
static LOOPBACK_CLIENT: LazyLock<remote_content::Client> =
    LazyLock::new(|| remote_content::Client::no_redirects(TIMEOUT, Scope::LoopbackOnly, false));

/// Which addresses an account's one-click unsubscribes may reach.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Reach {
    /// The public internet, over HTTPS (RFC 8058's rule). Never this machine or a private
    /// network, which a crafted link could otherwise make Exo post to.
    Public,
    /// This machine only, over HTTP too. It's for an account on mock-servers, whose mail points at
    /// local test servers, never at the internet.
    Loopback,
}

impl Reach {
    /// The reach of an account whose Gmail is at `endpoints`: [`Reach::Loopback`] on this
    /// machine (mock-servers), [`Reach::Public`] anywhere else.
    pub fn of(endpoints: &gmail::Endpoints) -> Self {
        if remote_content::serves_gmail_locally(endpoints) {
            Self::Loopback
        } else {
            Self::Public
        }
    }

    fn scope(self) -> Scope {
        match self {
            Self::Public => Scope::Public,
            Self::Loopback => Scope::LoopbackOnly,
        }
    }

    /// Whether a POST may connect to `ip`.
    pub fn allows(self, ip: IpAddr) -> bool {
        self.scope().allows(ip)
    }

    /// Whether `url` may be posted to, as far as can be told before resolving its host: its
    /// scheme, no credentials, and an address literal this reach allows.
    pub fn permits(self, url: &Url) -> bool {
        remote_content::permits(url, self.scope(), self == Self::Public)
    }
}

/// How a POST went.
#[derive(Debug, PartialEq, Eq)]
pub enum Outcome {
    /// The sender has it.
    Done,
    /// It failed for a reason trying again could fix: the network, a timeout, a 5xx, 408, 429.
    Retry(String),
    /// It can't succeed: a link or address this reach doesn't allow, or the sender refused it.
    Refused(String),
}

/// POSTs RFC 8058's unsubscribe to `url`, connecting only to addresses `reach` allows.
pub async fn post(url: &Url, reach: Reach) -> Outcome {
    if !reach.permits(url) {
        return Outcome::Refused("not a link Examail posts to".into());
    }
    let client = match reach {
        Reach::Public => &*PUBLIC_CLIENT,
        Reach::Loopback => &*LOOPBACK_CLIENT,
    };
    match client.post_form(url, BODY).await {
        Ok(response) => {
            let status = response.status();
            if status.is_success() || status.is_redirection() {
                Outcome::Done
            } else if status.is_server_error() || matches!(status.as_u16(), 408 | 429) {
                Outcome::Retry(format!("HTTP {status}"))
            } else {
                Outcome::Refused(format!("HTTP {status}"))
            }
        }
        Err(error) if error.is_refused() => Outcome::Refused(error.to_string()),
        Err(error) => Outcome::Retry(error.to_string()),
    }
}

/// Posts the store's waiting unsubscribes that are due, as far as `reach` allows. Returns when
/// the earliest one left should be tried again.
pub async fn push(store: &Store, reach: Reach) -> Option<Timestamp> {
    let pending = match store.pending_unsubscribes().await {
        Ok(pending) => pending,
        Err(error) => {
            log::error!("reading unsubscribes: {error}");
            return None;
        }
    };
    let now = Timestamp::now();
    let mut retry: Option<Timestamp> = None;
    for pending in pending {
        if let Some(at) = pending.not_before.filter(|at| *at > now) {
            retry = Some(retry.map_or(at, |retry| retry.min(at)));
            continue;
        }
        let outcome = match Url::parse(&pending.url) {
            Ok(url) => post(&url, reach).await,
            Err(error) => Outcome::Refused(error.to_string()),
        };
        let stale = now.duration_since(pending.queued) > GIVE_UP;
        let written = match outcome {
            Outcome::Done => store.finish_unsubscribe(&pending.url).await,
            Outcome::Retry(why) if !stale => {
                let wait = BACKOFF
                    .checked_mul(1 << pending.failures.min(10))
                    .unwrap_or(MAX_BACKOFF)
                    .min(MAX_BACKOFF);
                let at = now.checked_add(wait).unwrap_or(now);
                log::info!("unsubscribing: {why}; trying again at {at}");
                retry = Some(retry.map_or(at, |retry| retry.min(at)));
                store.postpone_unsubscribe(&pending.url, at).await
            }
            Outcome::Retry(why) | Outcome::Refused(why) => {
                log::warn!("giving up unsubscribing: {why}");
                store.finish_unsubscribe(&pending.url).await
            }
        };
        if let Err(error) = written {
            log::error!("updating unsubscribes: {error}");
        }
    }
    retry
}

#[cfg(test)]
mod tests {
    use super::*;

    fn url(text: &str) -> Url {
        Url::parse(text).expect("a URL")
    }

    #[test]
    fn the_public_reach_is_the_internet_only() {
        for public in [
            "8.8.8.8",
            "93.184.215.14",
            "2606:4700::1111",
            "64:ff9b::808:808",
            "2002:808:808::1",
            "::ffff:8.8.8.8",
        ] {
            assert!(
                Reach::Public.allows(public.parse().expect("an address")),
                "{public}"
            );
        }
        for private in [
            "127.0.0.1",
            "10.0.0.1",
            "172.16.5.4",
            "192.168.1.1",
            "169.254.169.254",
            "100.64.0.1",
            "0.0.0.0",
            "0.1.2.3",
            "192.0.0.8",
            "198.18.0.1",
            "224.0.0.1",
            "255.255.255.255",
            "240.0.0.1",
            "192.0.2.1",
            "::1",
            "::",
            "fc00::1",
            "fd12:3456::1",
            "fe80::1",
            "fec0::1",
            "ff02::1",
            "2001:db8::1",
            "::ffff:127.0.0.1",
            "::ffff:10.0.0.1",
            "64:ff9b::a00:1",
            "2002:c0a8:101::1",
            "::7f00:1",
            "100::1",
        ] {
            assert!(
                !Reach::Public.allows(private.parse().expect("an address")),
                "{private}"
            );
        }
        assert!(Reach::Loopback.allows("127.0.0.1".parse().expect("an address")));
        assert!(Reach::Loopback.allows("::ffff:127.0.0.1".parse().expect("an address")));
        assert!(!Reach::Loopback.allows("8.8.8.8".parse().expect("an address")));
    }

    #[test]
    fn only_https_without_credentials_to_an_allowed_literal() {
        assert!(Reach::Public.permits(&url("https://list.example/u?id=1")));
        for refused in [
            "http://list.example/u",
            "https://user:pass@list.example/u",
            "https://user@list.example/u",
            "https://127.0.0.1/u",
            "https://[::1]/u",
            "https://10.1.2.3/u",
            "https://0x7f.1/u",
            "https://2130706433/u",
        ] {
            assert!(!Reach::Public.permits(&url(refused)), "{refused}");
        }
        assert!(Reach::Loopback.permits(&url("http://127.0.0.1:9060/u")));
        assert!(!Reach::Loopback.permits(&url("https://8.8.8.8/u")));
    }

    #[test]
    fn mock_accounts_reach_loopback_and_google_ones_the_internet() {
        let mock = gmail::Endpoints::mock(&url("http://127.0.0.1:9060/"));
        assert_eq!(Reach::of(&mock), Reach::Loopback);
        assert_eq!(Reach::of(&gmail::Endpoints::google()), Reach::Public);
    }

    /// What the mock's one-click endpoint took.
    async fn taken(server: &mock_servers::MockServer) -> Vec<serde_json::Value> {
        let url = server.url().join("__mock/unsubscribes").expect("a URL");
        reqwest::get(url)
            .await
            .expect("the mock answers")
            .json()
            .await
            .expect("JSON")
    }

    #[tokio::test]
    async fn the_answer_says_whether_it_went() {
        let server = mock_servers::MockServer::start(mock_servers::Config::default())
            .await
            .expect("the mock starts");
        let link = |query: &str| {
            server
                .url()
                .join(&format!("__mock/unsubscribe/list.example/u{query}"))
                .expect("a URL")
        };
        assert_eq!(post(&link(""), Reach::Loopback).await, Outcome::Done);
        assert_eq!(
            post(&link("?answer=302"), Reach::Loopback).await,
            Outcome::Done,
            "a redirect counts, unfollowed"
        );
        assert!(matches!(
            post(&link("?answer=503"), Reach::Loopback).await,
            Outcome::Retry(_)
        ));
        assert!(matches!(
            post(&link("?answer=429"), Reach::Loopback).await,
            Outcome::Retry(_)
        ));
        assert!(matches!(
            post(&link("?answer=404"), Reach::Loopback).await,
            Outcome::Refused(_)
        ));
        assert!(
            matches!(post(&link(""), Reach::Public).await, Outcome::Refused(_)),
            "a real account never posts here"
        );
        let taken = taken(&server).await;
        assert_eq!(taken.len(), 5, "the public reach never connected");
        assert!(
            taken
                .iter()
                .all(|post| post["body"] == BODY && post["credentials"] == serde_json::json!([]))
        );
    }

    #[tokio::test]
    async fn the_queue_keeps_what_can_be_retried_until_it_goes() {
        let server = mock_servers::MockServer::start(mock_servers::Config::default())
            .await
            .expect("the mock starts");
        let dir = std::env::temp_dir().join(format!("exo-one-click-queue-{}", std::process::id()));
        let store = Store::open(
            &dir,
            "maya@brightmoor.co",
            server.endpoints(),
            gmail::StaticToken(mock_servers::DEV_ACCESS_TOKEN.into()),
        )
        .await
        .expect("the store opens");
        let link = |path: &str| {
            server
                .url()
                .join(&format!("__mock/unsubscribe/{path}"))
                .expect("a URL")
                .to_string()
        };
        for path in [
            "list.example/ok",
            "list.example/busy?answer=503",
            "list.example/gone?answer=410",
        ] {
            store.queue_unsubscribe(&link(path)).await.expect("queued");
        }
        store
            .queue_unsubscribe(&link("list.example/ok"))
            .await
            .expect("queued");
        let retry = push(&store, Reach::Loopback).await;
        let left = store.pending_unsubscribes().await.expect("read");
        assert_eq!(
            left.iter()
                .map(|pending| (pending.url.as_str(), pending.failures))
                .collect::<Vec<_>>(),
            [(link("list.example/busy?answer=503").as_str(), 1)]
        );
        assert_eq!(retry, left[0].not_before, "it says when to try again");
        // The retry isn't due yet, so nothing goes again.
        push(&store, Reach::Loopback).await;
        assert_eq!(
            taken(&server).await.len(),
            3,
            "each posted once, the duplicate dropped"
        );
        drop(store);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn a_name_that_resolves_to_this_machine_is_never_posted_to() {
        // `localhost` resolves without a network, to addresses the public reach refuses.
        let outcome = post(&url("https://localhost:9/u"), Reach::Public).await;
        assert!(matches!(outcome, Outcome::Refused(_)), "{outcome:?}");
        let outcome = post(&url("https://127.0.0.1:9/u"), Reach::Public).await;
        assert!(matches!(outcome, Outcome::Refused(_)), "{outcome:?}");
    }
}

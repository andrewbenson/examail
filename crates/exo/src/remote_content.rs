//! The HTTP client message bodies load remote content with (images, and the CSS they import).
//! Senders pick those URLs, so it only reaches the public internet, never this Mac, its local
//! network or link-local services. Otherwise an `<img src="http://192.168.1.1/…">` has Exo ask
//! a router or printer on your network for something each time the message shows, from inside
//! it. Names are resolved before they're checked and the connection goes to the addresses that
//! passed, so DNS can't sneak a private address in. Addresses in URLs, redirects included, are
//! checked the same way.

use std::{
    net::{IpAddr, SocketAddr},
    sync::Arc,
    time::Duration,
};

use reqwest::dns::{Addrs, Name, Resolve, Resolving};

/// How many redirects a load follows (reqwest's default).
const MAX_REDIRECTS: usize = 10;

/// A load refused because it would have reached this Mac or its network.
#[derive(Debug, thiserror::Error)]
#[error("{0} is not on the public internet")]
pub(crate) struct Private(String);

/// Why a load failed.
#[derive(Debug, thiserror::Error)]
pub(crate) enum Error {
    /// The URL doesn't parse.
    #[error(transparent)]
    Url(#[from] url::ParseError),
    /// It names a local address.
    #[error(transparent)]
    Private(#[from] Private),
    /// The request failed (a name resolving only to local addresses, or a redirect to one,
    /// fails here too).
    #[error(transparent)]
    Http(#[from] reqwest::Error),
    /// The client couldn't be built, so nothing loads rather than loading unchecked.
    #[error("the remote content client couldn't be built")]
    Unavailable,
}

/// Which addresses a [`Client`] may connect to.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Scope {
    /// Only globally routable public internet addresses.
    Public,
    /// Public internet addresses, plus this machine's loopback (mock-servers).
    PublicOrLoopback,
    /// Only this machine's loopback (mock-servers' one-click unsubscribes).
    LoopbackOnly,
}

impl Scope {
    /// Whether `ip` is within this scope.
    pub(crate) fn allows(self, ip: IpAddr) -> bool {
        match self {
            Self::Public => is_public(ip),
            Self::PublicOrLoopback => ip.to_canonical().is_loopback() || is_public(ip),
            Self::LoopbackOnly => ip.to_canonical().is_loopback(),
        }
    }
}

impl Error {
    /// Whether the request was refused before connecting because its URL or resolved addresses
    /// weren't allowed.
    pub(crate) fn is_refused(&self) -> bool {
        match self {
            Self::Url(_) | Self::Private(_) => true,
            Self::Http(error) => {
                let mut source = std::error::Error::source(error);
                while let Some(cause) = source {
                    if cause.is::<Private>() {
                        return true;
                    }
                    source = cause.source();
                }
                false
            }
            Self::Unavailable => false,
        }
    }
}

/// The client. Every URL is checked before it's asked for, and names are checked as they resolve.
#[derive(Clone, Debug)]
pub(crate) struct Client {
    http: Option<reqwest::Client>,
    scope: Scope,
    https_only: bool,
}

impl Client {
    /// Gives up on a load after `timeout`. With `loopback` it may also load from this Mac, which
    /// only a Gmail served from it (mock-servers) asks for.
    pub(crate) fn new(timeout: Duration, loopback: bool) -> Self {
        let scope = if loopback {
            Scope::PublicOrLoopback
        } else {
            Scope::Public
        };
        Self {
            http: http(timeout, scope, false, true),
            scope,
            https_only: false,
        }
    }

    /// [`new`](Self::new) on the public internet, over HTTPS only, redirects included.
    pub(crate) fn https_only(timeout: Duration) -> Self {
        Self {
            http: http(timeout, Scope::Public, true, true),
            scope: Scope::Public,
            https_only: true,
        }
    }

    /// A client that never follows redirects, restricted to `scope` and optionally HTTPS only.
    pub(crate) fn no_redirects(timeout: Duration, scope: Scope, https_only: bool) -> Self {
        Self {
            http: http(timeout, scope, https_only, false),
            scope,
            https_only,
        }
    }

    /// Asks for `url`, refusing without connecting when it names a local address or carries
    /// credentials.
    pub(crate) async fn get(&self, url: &str) -> Result<reqwest::Response, Error> {
        let url = url::Url::parse(url)?;
        check(&url, self.scope, self.https_only)?;
        let http = self.http.as_ref().ok_or(Error::Unavailable)?;
        Ok(http.get(url).send().await?)
    }

    /// POSTs `body` (`application/x-www-form-urlencoded`) to `url`, refusing without connecting
    /// when `url` isn't allowed by this client's scope.
    pub(crate) async fn post_form(
        &self,
        url: &url::Url,
        body: &'static str,
    ) -> Result<reqwest::Response, Error> {
        check(url, self.scope, self.https_only)?;
        let http = self.http.as_ref().ok_or(Error::Unavailable)?;
        Ok(http
            .post(url.clone())
            .header(
                reqwest::header::CONTENT_TYPE,
                "application/x-www-form-urlencoded",
            )
            .body(body)
            .send()
            .await?)
    }

    /// The body at `url` ([`get`](Self::get)), or `None` when it's over `limit` bytes.
    pub(crate) async fn bytes(&self, url: &str, limit: usize) -> Result<Option<Vec<u8>>, Error> {
        let mut response = self.get(url).await?.error_for_status()?;
        let mut bytes = Vec::new();
        while let Some(chunk) = response.chunk().await? {
            if bytes.len() + chunk.len() > limit {
                return Ok(None);
            }
            bytes.extend_from_slice(&chunk);
        }
        Ok(Some(bytes))
    }
}

/// Whether Gmail is served from this Mac (mock-servers), the only case in which remote content may
/// come from it too.
pub(crate) fn serves_gmail_locally(endpoints: &gmail::Endpoints) -> bool {
    endpoints.gmail.host().is_some_and(|host| match host {
        url::Host::Domain(name) => name.eq_ignore_ascii_case("localhost"),
        url::Host::Ipv4(ip) => ip.is_loopback(),
        url::Host::Ipv6(ip) => ip.is_loopback(),
    })
}

/// Whether `url` is allowed before resolving its host: allowed scheme, no userinfo credentials,
/// and an IP literal (if any) permitted by `scope`.
pub(crate) fn permits(url: &url::Url, scope: Scope, https_only: bool) -> bool {
    check(url, scope, https_only).is_ok()
}

/// The HTTP client, whose resolver and redirects only reach addresses in `scope`, and with
/// `https_only`, only over HTTPS.
fn http(
    timeout: Duration,
    scope: Scope,
    https_only: bool,
    follow_redirects: bool,
) -> Option<reqwest::Client> {
    let policy = if follow_redirects {
        reqwest::redirect::Policy::custom(move |attempt| {
            if attempt.previous().len() >= MAX_REDIRECTS {
                attempt.error("too many redirects")
            } else if let Err(refused) = check(attempt.url(), scope, https_only) {
                attempt.error(refused)
            } else {
                attempt.follow()
            }
        })
    } else {
        reqwest::redirect::Policy::none()
    };
    reqwest::Client::builder()
        .timeout(timeout)
        .redirect(policy)
        .dns_resolver(Arc::new(PublicResolver { scope }))
        .https_only(https_only)
        // A proxy would resolve the name itself, past the resolver's check.
        .no_proxy()
        .build()
        .inspect_err(|error| log::error!("building the remote content client: {error}"))
        .ok()
}

/// Refuses `url` before anything connects when its scheme isn't HTTP(S), it carries credentials,
/// or its host is an IP literal outside `scope`. Names are left to [`PublicResolver`].
fn check(url: &url::Url, scope: Scope, https_only: bool) -> Result<(), Private> {
    let scheme_ok = if https_only {
        url.scheme() == "https"
    } else {
        matches!(url.scheme(), "http" | "https")
    };
    if !scheme_ok || !url.username().is_empty() || url.password().is_some() {
        return Err(Private(url.to_string()));
    }
    let Some(host) = url.host() else {
        return Err(Private(url.to_string()));
    };
    if scope == Scope::Public {
        return if exo_mail::is_public_host(host.clone()) {
            Ok(())
        } else {
            Err(Private(host.to_string()))
        };
    }
    let ip = match host {
        url::Host::Ipv4(ip) => IpAddr::V4(ip),
        url::Host::Ipv6(ip) => IpAddr::V6(ip),
        url::Host::Domain(_) => return Ok(()),
    };
    if scope.allows(ip) {
        Ok(())
    } else {
        Err(Private(ip.to_string()))
    }
}

/// Resolves names with the system's resolver, keeping only the addresses [`Scope::allows`] lets
/// through. A name with none left fails to resolve.
struct PublicResolver {
    scope: Scope,
}

impl Resolve for PublicResolver {
    fn resolve(&self, name: Name) -> Resolving {
        let scope = self.scope;
        Box::pin(async move {
            let host = name.as_str().to_owned();
            let addresses: Vec<SocketAddr> = tokio::net::lookup_host((host.as_str(), 0))
                .await?
                .filter(|address| scope.allows(address.ip()))
                .collect();
            if addresses.is_empty() {
                return Err(Box::new(Private(host)) as _);
            }
            Ok(Box::new(addresses.into_iter()) as Addrs)
        })
    }
}

pub(crate) use exo_mail::is_public_ip as is_public;

#[cfg(test)]
mod tests {
    use std::{
        io::{Read as _, Write as _},
        sync::atomic::{AtomicUsize, Ordering},
    };

    use super::*;

    #[test]
    fn only_public_addresses_pass() {
        let public = [
            "8.8.8.8",
            "93.184.216.34",
            "100.128.0.1",
            "2606:4700::1111",
            "::ffff:8.8.8.8",
            "64:ff9b::808:808",
            "2002:808:808::1",
        ];
        for ip in public {
            assert!(is_public(ip.parse().expect("an address")), "{ip} is public");
        }
        let private = [
            "127.0.0.1",
            "127.8.9.10",
            "10.1.2.3",
            "172.16.0.1",
            "172.31.255.255",
            "192.168.1.1",
            "169.254.169.254",
            "0.0.0.0",
            "0.1.2.3",
            "100.64.0.1",
            "100.127.255.254",
            "192.0.0.8",
            "192.0.2.1",
            "198.18.0.1",
            "198.19.255.254",
            "198.51.100.1",
            "203.0.113.1",
            "224.0.0.1",
            "255.255.255.255",
            "240.0.0.1",
            "::1",
            "::",
            "::7f00:1",
            "::a00:1",
            "fe80::1",
            "fc00::1",
            "fd12:3456::1",
            "fec0::1",
            "ff02::1",
            "100::1",
            "2001:db8::1",
            "2001::1",
            "::ffff:192.168.1.1",
            "::ffff:127.0.0.1",
            "::ffff:192.0.0.1",
            "::ffff:198.18.0.1",
            "64:ff9b::a00:1",
            "64:ff9b:1::808:808",
            "64:ff9b:1:ffff::1",
            "2002:c0a8:101::1",
            "2002:7f00:1::1",
        ];
        for ip in private {
            assert!(!is_public(ip.parse().expect("an address")), "{ip} is local");
        }
        assert!(
            Scope::PublicOrLoopback.allows("127.0.0.1".parse().expect("an address")),
            "mock-servers' own machine"
        );
        assert!(
            Scope::PublicOrLoopback.allows("::ffff:127.0.0.1".parse().expect("an address")),
            "canonicalized IPv4-mapped loopback"
        );
        assert!(
            !Scope::PublicOrLoopback.allows("192.168.1.1".parse().expect("an address")),
            "but never the network"
        );
        assert!(Scope::LoopbackOnly.allows("127.0.0.1".parse().expect("an address")));
        assert!(!Scope::LoopbackOnly.allows("8.8.8.8".parse().expect("an address")));
    }

    #[test]
    fn urls_naming_local_addresses_or_credentials_are_refused_before_connecting() {
        let check = |url: &str, loopback| {
            let scope = if loopback {
                Scope::PublicOrLoopback
            } else {
                Scope::Public
            };
            check(&url.parse().expect("a URL"), scope, false).is_ok()
        };
        assert!(
            check("https://example.com/a.png", false),
            "names are the resolver's"
        );
        assert!(!check("http://192.168.0.1/admin", false));
        assert!(!check("http://[::ffff:10.0.0.1]/", false));
        assert!(!check("http://[::7f00:1]/", false));
        assert!(!check("http://[64:ff9b:1::808:808]/", false));
        assert!(!check("http://127.0.0.1:8080/", false));
        assert!(!check("http://localhost:8080/", false));
        assert!(!check("http://sub.localhost/a.png", false));
        assert!(!check("https://user:pass@example.com/a.png", false));
        assert!(!check("https://user@example.com/a.png", false));
        assert!(!check("file:///etc/passwd", false));
        assert!(check("http://127.0.0.1:8080/", true));
        assert!(!check("http://user@127.0.0.1:8080/", true));
    }

    /// A server on this Mac that counts the connections it takes and answers each request with
    /// `answer`.
    struct Server {
        address: SocketAddr,
        connections: Arc<AtomicUsize>,
    }

    impl Server {
        fn start(answer: String) -> Self {
            let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("a port");
            let address = listener.local_addr().expect("an address");
            let connections = Arc::new(AtomicUsize::new(0));
            let counted = connections.clone();
            std::thread::spawn(move || {
                for mut stream in listener.incoming().map_while(Result::ok) {
                    counted.fetch_add(1, Ordering::SeqCst);
                    let mut request = [0; 1024];
                    let _ = stream.read(&mut request);
                    let _ = stream.write_all(answer.as_bytes());
                }
            });
            Self {
                address,
                connections,
            }
        }
    }

    #[tokio::test]
    async fn loads_never_reach_this_mac_by_address_or_by_name() {
        let server = Server::start(
            "HTTP/1.1 200 OK\r\nContent-Length: 2\r\nConnection: close\r\n\r\nok".to_owned(),
        );
        let port = server.address.port();
        let client = Client::new(Duration::from_secs(5), false);
        for url in [
            format!("http://127.0.0.1:{port}/a.png"),
            format!("http://localhost:{port}/a.png"),
            format!("http://[::ffff:127.0.0.1]:{port}/a.png"),
        ] {
            assert!(client.get(&url).await.is_err(), "{url} is refused");
        }
        assert_eq!(
            server.connections.load(Ordering::SeqCst),
            0,
            "nothing connected"
        );

        let mock = client_for_mock()
            .get(&format!("http://127.0.0.1:{port}/a.png"))
            .await
            .expect("mock-servers' machine is allowed");
        assert_eq!(mock.text().await.expect("a body"), "ok");
    }

    #[tokio::test]
    async fn redirects_to_local_addresses_are_refused() {
        for location in [
            "http://169.254.169.254/latest/meta-data/",
            "http://192.168.1.1/",
            "http://[fe80::1]/",
        ] {
            let answer = format!(
                "HTTP/1.1 302 Found\r\nLocation: {location}\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
            );
            let server = Server::start(answer);
            let response = client_for_mock()
                .get(&format!("http://{}/a.png", server.address))
                .await;
            let error = response.expect_err("the redirect is refused");
            assert!(
                matches!(&error, Error::Http(error) if error.is_redirect()),
                "{location}: {error}"
            );
        }
    }

    fn client_for_mock() -> Client {
        Client::new(Duration::from_secs(5), true)
    }
}

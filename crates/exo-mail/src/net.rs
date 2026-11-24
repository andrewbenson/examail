//! How a body gets the resources it references, without this crate doing any I/O.

use std::{
    net::{IpAddr, Ipv4Addr, Ipv6Addr},
    sync::{
        Arc, Mutex,
        atomic::{AtomicUsize, Ordering},
    },
};

use base64::Engine as _;
use blitz_traits::net::{Bytes, NetHandler, NetProvider, Request};

use crate::{animation, view::BASE_URL};

/// Images that may be animated, by URL, waiting for the view to find the elements that show
/// them.
pub(crate) type MayAnimate = Arc<Mutex<Vec<(String, Arc<[u8]>)>>>;

/// Loads what a message body references, which is only ever an `http`/`https` URL (remote
/// images, and stylesheets or fonts the sender's CSS imports, loaded directly with no proxy)
/// or a `cid:<content-id>` (inline attachments). Recognized tracking pixels never get here
/// unless [`load_read_receipts`](crate::load_read_receipts) sends them, and with
/// [`ViewOptions::block_remote_content`](crate::ViewOptions::block_remote_content) only `cid:`
/// URLs do.
///
/// Implementations start the load and return. Call [`Done::finish`] with the bytes from any
/// thread when they arrive (drop it on failure), then [`BodyView::poll`](crate::BodyView::poll)
/// on the UI thread to lay out with them.
pub trait Fetch: Send + Sync + 'static {
    /// Starts loading `url`.
    fn fetch(&self, url: &str, done: Done);
}

/// Completes one [`Fetch::fetch`].
pub struct Done {
    url: String,
    handler: Box<dyn NetHandler>,
    arrived: Arc<AtomicUsize>,
    may_animate: MayAnimate,
}

impl Done {
    /// A completion nobody waits for, so what arrives is dropped.
    pub(crate) fn ignored(url: &str) -> Self {
        Self {
            url: url.to_owned(),
            handler: Box::new(Ignore),
            arrived: Arc::default(),
            may_animate: MayAnimate::default(),
        }
    }

    /// Hands the loaded bytes to the body that asked for them. Images are decoded here, on the
    /// calling thread.
    pub fn finish(self, bytes: Vec<u8>) {
        note_animation(&self.may_animate, &self.url, &bytes);
        self.handler.bytes(self.url, Bytes::from(bytes));
        self.arrived.fetch_add(1, Ordering::Release);
    }
}

/// Drops a response (a read receipt's).
struct Ignore;

impl NetHandler for Ignore {
    fn bytes(self: Box<Self>, _url: String, _bytes: Bytes) {}
}

/// Keeps `bytes` aside for the view when they may be an animated image (Blitz shows only an
/// image's first frame).
fn note_animation(may_animate: &MayAnimate, url: &str, bytes: &[u8]) {
    if animation::may_animate(bytes)
        && let Ok(mut pending) = may_animate.lock()
        && !pending.iter().any(|(existing, _)| existing == url)
    {
        pending.push((url.to_owned(), Arc::from(bytes)));
    }
}

/// Adapts a [`Fetch`] to Blitz, answering `data:` URLs itself.
pub(crate) struct FetchProvider {
    fetch: Arc<dyn Fetch>,
    /// Remote URLs go to `fetch`. Without this, only inline attachments do (remote content is
    /// blocked).
    remote: bool,
    /// How many responses have been handed over, for [`BodyView::poll`](crate::BodyView::poll).
    pub(crate) arrived: Arc<AtomicUsize>,
    /// How many `data:` URLs have been answered, which happens as they're asked for.
    pub(crate) answered_inline: Arc<AtomicUsize>,
    pub(crate) may_animate: MayAnimate,
}

impl FetchProvider {
    pub(crate) fn new(fetch: Arc<dyn Fetch>, remote: bool) -> Self {
        Self {
            fetch,
            remote,
            arrived: Arc::default(),
            answered_inline: Arc::default(),
            may_animate: MayAnimate::default(),
        }
    }
}

impl NetProvider for FetchProvider {
    fn fetch(&self, _doc_id: usize, request: Request, handler: Box<dyn NetHandler>) {
        let url = request.url.as_str();
        match request.url.scheme() {
            "data" => {
                if let Some(bytes) = decode_data_url(url) {
                    note_animation(&self.may_animate, url, &bytes);
                    handler.bytes(url.to_owned(), Bytes::from(bytes));
                    self.answered_inline.fetch_add(1, Ordering::Release);
                }
            }
            "http" | "https" if !self.remote => {}
            "http" | "https" | "cid" if !url.starts_with(BASE_URL) => self.fetch.fetch(
                url,
                Done {
                    url: url.to_owned(),
                    handler,
                    arrived: self.arrived.clone(),
                    may_animate: self.may_animate.clone(),
                },
            ),
            // Unresolvable relative URLs, and local files or other schemes a sender names.
            _ => {}
        }
    }
}

/// The payload of a `data:` URL (RFC 2397), its base64 decoded as browsers decode it.
pub fn decode_data_url(url: &str) -> Option<Vec<u8>> {
    let (header, payload) = crate::sanitize::strip_ascii_prefix(url, "data:")?.split_once(',')?;
    if header.to_ascii_lowercase().ends_with(";base64") {
        let compact: String = payload
            .chars()
            .filter(|c| !c.is_ascii_whitespace())
            .collect();
        // Forgiving base64, as browsers decode `data:` URLs, so padding may be left off.
        let engine = base64::engine::GeneralPurpose::new(
            &base64::alphabet::STANDARD,
            base64::engine::GeneralPurposeConfig::new()
                .with_decode_padding_mode(base64::engine::DecodePaddingMode::Indifferent),
        );
        engine.decode(compact).ok()
    } else {
        Some(percent_decode_bytes(payload))
    }
}

/// Whether `ip` is on the public internet: not this machine (loopback, `0.0.0.0`), a private or
/// carrier-grade NAT network, link-local, multicast, broadcast, documentation, benchmarking,
/// protocol-assignment or reserved space, nor an IPv6 transition/translation address standing for
/// one of those.
pub fn is_public_ip(ip: IpAddr) -> bool {
    match ip.to_canonical() {
        IpAddr::V4(ip) => is_public_ipv4(ip),
        IpAddr::V6(ip) => is_public_ipv6(ip),
    }
}

/// Whether an IPv4 address is on the public internet.
pub fn is_public_ipv4(ip: Ipv4Addr) -> bool {
    let [a, b, c, _] = ip.octets();
    !(a == 0
        || ip.is_private()
        || ip.is_loopback()
        || ip.is_link_local()
        || ip.is_unspecified()
        || ip.is_broadcast()
        || ip.is_multicast()
        || ip.is_documentation()
        // 100.64.0.0/10, carrier-grade NAT.
        || (a == 100 && b & 0xc0 == 64)
        // 192.0.0.0/24, IETF protocol assignments.
        || (a == 192 && b == 0 && c == 0)
        // 198.18.0.0/15, benchmarking.
        || (a == 198 && b & 0xfe == 18)
        // 240.0.0.0/4, reserved.
        || a >= 240)
}

/// Whether an IPv6 address is on the public internet.
pub fn is_public_ipv6(ip: Ipv6Addr) -> bool {
    // IPv4-mapped (`::ffff:0:0/96`) reaches the IPv4 address inside it.
    if let Some(v4) = ip.to_ipv4_mapped() {
        return is_public_ipv4(v4);
    }
    let segments = ip.segments();
    let embedded = |high: u16, low: u16| Ipv4Addr::from((u32::from(high) << 16) | u32::from(low));
    // Well-known NAT64 (`64:ff9b::/96`) and 6to4 (`2002::/16`) reach the IPv4 address inside them.
    if segments[..6] == [0x64, 0xff9b, 0, 0, 0, 0] {
        return is_public_ipv4(embedded(segments[6], segments[7]));
    }
    if segments[0] == 0x2002 {
        return is_public_ipv4(embedded(segments[1], segments[2]));
    }
    !(ip.is_loopback()
        || ip.is_unspecified()
        || ip.is_multicast()
        || ip.is_unique_local()
        || ip.is_unicast_link_local()
        // Deprecated IPv4-compatible (`::/96`).
        || segments[..6] == [0; 6]
        // Local-use NAT64 (`64:ff9b:1::/48`, RFC 8215).
        || segments[..3] == [0x64, 0xff9b, 1]
        // Site-local (`fec0::/10`), deprecated but still routed by some networks.
        || segments[0] & 0xffc0 == 0xfec0
        // Documentation (`2001:db8::/32`) and Teredo (`2001:0::/32`).
        || segments[..2] == [0x2001, 0x0db8]
        || segments[..2] == [0x2001, 0]
        // Discard-only (`100::/64`).
        || segments[..4] == [0x100, 0, 0, 0])
}

/// Whether a URL host is on the public internet (not `localhost`, `*.localhost`, or a non-public
/// IP literal).
pub fn is_public_host(host: url::Host<&str>) -> bool {
    match host {
        url::Host::Domain(domain) => {
            let domain = domain.trim_end_matches('.');
            !domain.is_empty()
                && !domain.eq_ignore_ascii_case("localhost")
                && !domain
                    .get(domain.len().saturating_sub(".localhost".len())..)
                    .is_some_and(|suffix| suffix.eq_ignore_ascii_case(".localhost"))
        }
        url::Host::Ipv4(ip) => is_public_ipv4(ip),
        url::Host::Ipv6(ip) => is_public_ipv6(ip),
    }
}

fn hex_nibble(byte: u8) -> Option<u8> {
    match byte {
        b'0'..=b'9' => Some(byte - b'0'),
        b'a'..=b'f' => Some(byte - b'a' + 10),
        b'A'..=b'F' => Some(byte - b'A' + 10),
        _ => None,
    }
}

/// Decodes `%XX` escapes (requiring two ASCII hex digits), leaving malformed ones as they are
/// (the URL spec's percent-decode).
pub fn percent_decode_bytes(text: &str) -> Vec<u8> {
    let bytes = text.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%'
            && let (Some(hi), Some(lo)) = (
                bytes.get(i + 1).copied().and_then(hex_nibble),
                bytes.get(i + 2).copied().and_then(hex_nibble),
            )
        {
            out.push((hi << 4) | lo);
            i += 3;
        } else {
            out.push(bytes[i]);
            i += 1;
        }
    }
    out
}

/// Percent-decodes `text` into UTF-8 lossily, leaving malformed `%` sequences as they are.
pub fn percent_decode_lossy(text: &str) -> String {
    if !text.contains('%') {
        return text.to_owned();
    }
    String::from_utf8_lossy(&percent_decode_bytes(text)).into_owned()
}

/// JavaScript's `decodeURIComponent`, giving `text` back unchanged where that would throw.
pub fn decode_uri_component(text: &str) -> String {
    if !text.contains('%') {
        return text.to_owned();
    }
    let well_formed = text.match_indices('%').all(|(i, _)| {
        text.get(i + 1..i + 3)
            .is_some_and(|hex| hex.bytes().all(|b| b.is_ascii_hexdigit()))
    });
    well_formed
        .then(|| String::from_utf8(percent_decode_bytes(text)).ok())
        .flatten()
        .unwrap_or_else(|| text.to_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decodes_data_urls() {
        assert_eq!(
            decode_data_url("data:image/gif;base64,R0lG").unwrap(),
            b"GIF"
        );
        assert_eq!(decode_data_url("data:text/plain,a%20b").unwrap(), b"a b");
        assert_eq!(
            decode_data_url("DATA:image/png;base64,iVBORw==").unwrap(),
            b"\x89PNG"
        );
        assert_eq!(
            decode_data_url("Data:text/plain,hello%20world").unwrap(),
            b"hello world"
        );
        assert!(decode_data_url("https://example.com").is_none());
    }

    #[test]
    fn classifies_public_and_non_public_ips_and_hosts() {
        for ip in [
            "8.8.8.8",
            "93.184.216.34",
            "100.128.0.1",
            "2606:4700::1111",
            "::ffff:8.8.8.8",
            "64:ff9b::808:808",
            "2002:808:808::1",
        ] {
            let parsed: IpAddr = ip.parse().expect("valid ip");
            assert!(is_public_ip(parsed), "{ip} should be public");
            if let IpAddr::V6(v6) = parsed {
                assert!(is_public_ipv6(v6), "{ip} should be public ipv6");
            }
        }
        for ip in [
            "127.0.0.1",
            "10.1.2.3",
            "172.16.0.1",
            "192.168.1.1",
            "169.254.169.254",
            "0.0.0.0",
            "0.1.2.3",
            "100.64.0.1",
            "192.0.0.8",
            "192.0.2.1",
            "198.18.0.1",
            "224.0.0.1",
            "240.0.0.1",
            "255.255.255.255",
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
            "::ffff:100.64.0.1",
            "64:ff9b::a00:1",
            "64:ff9b::7f00:1",
            "64:ff9b:1::808:808",
            "64:ff9b:1:ffff::1",
            "2002:c0a8:101::1",
            "2002:7f00:1::1",
        ] {
            let parsed: IpAddr = ip.parse().expect("valid ip");
            assert!(!is_public_ip(parsed), "{ip} should be non-public");
            if let IpAddr::V6(v6) = parsed {
                assert!(!is_public_ipv6(v6), "{ip} should be non-public ipv6");
            }
        }
        assert!(is_public_host(url::Host::Domain("example.com")));
        assert!(!is_public_host(url::Host::Domain("localhost")));
        assert!(!is_public_host(url::Host::Domain("LOCALHOST.")));
        assert!(!is_public_host(url::Host::Domain("sub.localhost")));
        assert!(!is_public_host(url::Host::Ipv6(
            "64:ff9b::a00:1".parse().unwrap()
        )));
        assert!(is_public_host(url::Host::Ipv6(
            "64:ff9b::808:808".parse().unwrap()
        )));
    }
}

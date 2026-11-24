//! Links in a body, and which one unsubscribes.

use std::sync::LazyLock;

use regex::Regex;

use crate::{
    Link,
    content::Content,
    dom::{self, NodeRef},
    quotes::FORWARDED,
};

/// Marks the chosen unsubscribe link, which signature detection treats as a footer.
pub(crate) const UNSUBSCRIBE_CLASS: &str = "exo-unsubscribe-link";

static UNSUBSCRIBE_WORDS: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new("(?i)unsubscribe|unsub\\b|avregistrera|désinscription|se désinscrire|me désabonner|se désabonner|gérer abonnement|解除|メール配信を希望されな")
        .expect("valid regex")
});
static APPLE_NEWS: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"mynews\.apple\.com/subscriptions").expect("valid regex"));
static ITALIAN_NEWSLETTER: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?i)non.*interessato.*newsletter").expect("valid regex"));
static CONTEXT_UNSUBSCRIBE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?i)unsubscribe|no longer.*receive.*emails").expect("valid regex")
});
static SETTINGS_TEXT: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?i)((email|communication)\s*(settings|preference))|(disable.*notifications)|((update|manage).*subscription)|(^turn off$)|opt(-|\s*)out")
        .expect("valid regex")
});
static OPT_OUT_CONTEXT: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?i)opt(-|\s*)out|(not|no longer|stop|do not want|don't want).*((receiv|get|interested).*(e-?mails?|updates))|(hear from (me|us|our))|(not |ir)relevant to you")
        .expect("valid regex")
});

/// Every link under `root`, and the best unsubscribe candidate (marked with
/// [`UNSUBSCRIBE_CLASS`]). `user_domains` are the account's own domains, and a `mailto:` to them
/// is never an unsubscribe link.
pub(crate) fn find_links(
    root: &NodeRef,
    content: &Content,
    user_domains: &[String],
) -> (Vec<Link>, Option<Link>) {
    let forwarded = FORWARDED.is_match(&content.to_formatted_string());
    let mut links = Vec::new();
    let mut unsubscribe = None;
    let mut best_anchor = None;
    let mut best_rank = u8::MAX;
    for anchor in dom::select(root, |n| dom::is(n, "a")) {
        let Some(url) = dom::attr(&anchor, "href").filter(|href| url::Url::parse(href).is_ok())
        else {
            continue;
        };
        let mut text = dom::text_content(&anchor).trim().to_owned();
        if text.is_empty() {
            text = dom::select_first(&anchor, |n| dom::is(n, "img"))
                .and_then(|image| dom::attr(&image, "alt"))
                .unwrap_or_default()
                .trim()
                .to_owned();
        }
        let link = Link {
            text,
            context: content.context_around(&anchor, 256).unwrap_or_default(),
            url,
        };
        let quoted = dom::closest(&anchor, |n| dom::is(n, "blockquote")).is_some();
        if let Some(rank) = unsubscribe_rank(&link, user_domains, quoted, forwarded)
            && rank <= best_rank
        {
            best_anchor = Some(anchor);
            unsubscribe = Some(link.clone());
            best_rank = rank;
        }
        links.push(link);
    }
    if let Some(anchor) = best_anchor {
        dom::add_class(&anchor, &[UNSUBSCRIBE_CLASS]);
    }
    (links, unsubscribe)
}

/// How likely a link is to unsubscribe, 1 (surely) to 4, or `None` when it isn't one.
fn unsubscribe_rank(
    link: &Link,
    user_domains: &[String],
    quoted: bool,
    forwarded: bool,
) -> Option<u8> {
    let Link { text, context, url } = link;
    if forwarded || quoted || url.starts_with("https://mail.superhuman.com") {
        return None;
    }
    if url
        .get(..7)
        .is_some_and(|scheme| scheme.eq_ignore_ascii_case("mailto:"))
    {
        let recipients = valid_unsubscribe_mailto_recipients(url)?;
        let to_own_domain = recipients.iter().any(|recipient| {
            let Some((_, recipient_domain)) = address_of(recipient).rsplit_once('@') else {
                return false;
            };
            user_domains
                .iter()
                .any(|domain| domain_matches(recipient_domain, domain))
        });
        if to_own_domain {
            return None;
        }
    }
    if UNSUBSCRIBE_WORDS.is_match(text)
        || UNSUBSCRIBE_WORDS.is_match(url)
        || APPLE_NEWS.is_match(url)
        || ITALIAN_NEWSLETTER.is_match(context)
    {
        Some(1)
    } else if CONTEXT_UNSUBSCRIBE.is_match(context) {
        Some(2)
    } else if SETTINGS_TEXT.is_match(text) {
        Some(3)
    } else if OPT_OUT_CONTEXT.is_match(context) {
        Some(4)
    } else {
        None
    }
}

fn domain_matches(recipient_domain: &str, user_domain: &str) -> bool {
    let user_domain = user_domain.trim_start_matches('.').trim_end_matches('.');
    if user_domain.is_empty() {
        return false;
    }
    let recipient_domain = recipient_domain.trim_end_matches('.');
    recipient_domain.eq_ignore_ascii_case(user_domain)
        || (recipient_domain.len() > user_domain.len()
            && recipient_domain.as_bytes()[recipient_domain.len() - user_domain.len() - 1] == b'.'
            && recipient_domain[recipient_domain.len() - user_domain.len()..]
                .eq_ignore_ascii_case(user_domain))
}

/// Splits an RFC 6068 `mailto:` address list on commas outside double quotes *before*
/// percent-decoding, so `%2C` inside display names (`%22Doe%2C%20Jane%22`) never splits a
/// recipient in half.
pub fn split_mailto_addresses(raw: &str) -> Vec<String> {
    if raw.is_empty() {
        return Vec::new();
    }
    let bytes = raw.as_bytes();
    let mut out = Vec::new();
    let mut start = 0;
    let mut at = 0;
    let mut quoted = false;
    while at < bytes.len() {
        match bytes[at] {
            b'\\' if quoted => {
                // Skip the escaped character (or `%XX` percent-encoded character).
                at += if bytes.get(at + 1) == Some(&b'%')
                    && raw
                        .get(at + 2..at + 4)
                        .is_some_and(|hex| hex.bytes().all(|b| b.is_ascii_hexdigit()))
                {
                    4
                } else {
                    2
                };
                continue;
            }
            b'"' => quoted = !quoted,
            b'%' if quoted
                && raw
                    .get(at + 1..at + 3)
                    .is_some_and(|hex| hex.eq_ignore_ascii_case("5c")) =>
            {
                at += if bytes.get(at + 3) == Some(&b'%')
                    && raw
                        .get(at + 4..at + 6)
                        .is_some_and(|hex| hex.bytes().all(|b| b.is_ascii_hexdigit()))
                {
                    6
                } else {
                    4
                };
                continue;
            }
            b'%' if raw
                .get(at + 1..at + 3)
                .is_some_and(|hex| hex.eq_ignore_ascii_case("22")) =>
            {
                quoted = !quoted;
                at += 3;
                continue;
            }
            b',' if !quoted => {
                out.push(crate::net::percent_decode_lossy(&raw[start..at]));
                start = at + 1;
            }
            _ => {}
        }
        at += 1;
    }
    out.push(crate::net::percent_decode_lossy(&raw[start..]));
    out
}

/// The recipients of a `mailto:` URL when there is at least one and every one is valid.
fn valid_unsubscribe_mailto_recipients(url: &str) -> Option<Vec<String>> {
    let rest = crate::sanitize::strip_ascii_prefix(url, "mailto:")?;
    let (to, query) = rest.split_once('?').unwrap_or((rest, ""));
    let mut recipients = split_mailto_addresses(to);
    for pair in query.split('&').filter(|pair| !pair.is_empty()) {
        let (key, value) = pair.split_once('=').unwrap_or((pair, ""));
        let key = crate::net::percent_decode_lossy(key);
        if matches!(key.to_ascii_lowercase().as_str(), "to" | "cc" | "bcc") {
            recipients.extend(split_mailto_addresses(value));
        }
    }
    let recipients: Vec<String> = recipients
        .into_iter()
        .map(|r| r.trim().to_owned())
        .filter(|r| !r.is_empty())
        .collect();
    (!recipients.is_empty()
        && recipients
            .iter()
            .all(|r| crate::sanitize::is_valid_email(address_of(r))))
    .then_some(recipients)
}

/// The address in `Name <address>`.
fn address_of(contact: &str) -> &str {
    match (contact.rfind('<'), contact.rfind('>')) {
        (Some(open), Some(close)) if open < close => &contact[open + 1..close],
        _ => contact,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn picks_the_unsubscribe_link() {
        let document = dom::parse(
            "<p>Read <a href=\"https://n.co/post\">more</a>.</p>\
             <p>Don't want these emails? <a href=\"https://n.co/prefs\">update email preferences</a> or \
             <a href=\"https://n.co/u?id=1\">Unsubscribe</a>.</p>",
        );
        let body = dom::body(&document).unwrap();
        let content = Content::new(&body);
        let (links, unsubscribe) = find_links(&body, &content, &[]);
        assert_eq!(links.len(), 3);
        assert_eq!(unsubscribe.unwrap().url, "https://n.co/u?id=1");
        let marked = dom::select(&body, |n| dom::has_class(n, UNSUBSCRIBE_CLASS));
        assert_eq!(
            marked.len(),
            1,
            "only the final winning anchor gets the unsubscribe class"
        );
        assert_eq!(
            dom::attr(&marked[0], "href").unwrap(),
            "https://n.co/u?id=1"
        );
    }

    #[test]
    fn validates_mailto_unsubscribe_recipients_and_own_domains() {
        let pick = |href: &str, domains: &[String]| {
            let document = dom::parse(&format!("<p><a href=\"{href}\">Unsubscribe</a></p>"));
            let body = dom::body(&document).unwrap();
            let content = Content::new(&body);
            find_links(&body, &content, domains).1
        };
        // Invalid mailto (no recipient) is rejected even when `user_domains` is empty.
        assert!(pick("mailto:?subject=unsubscribe", &[]).is_none());
        assert!(pick("MAILTO:?subject=unsubscribe", &[]).is_none());
        // `+` in query `to=` is preserved via RFC 3986 percent-decoding.
        assert_eq!(
            pick("MAILTO:?to=unsub+tok@example.com&subject=unsubscribe", &[])
                .unwrap()
                .url,
            "MAILTO:?to=unsub+tok@example.com&subject=unsubscribe"
        );
        let own = vec!["acme.com".to_owned()];
        // Own domain (exact or subdomain) is rejected even when `?subject=...` follows.
        assert!(pick("mailto:team@acme.com?subject=unsubscribe", &own).is_none());
        assert!(pick("MAILTO:team@lists.acme.com?subject=unsubscribe", &own).is_none());
        assert!(pick("mailto:?to=team@acme.com&subject=unsubscribe", &own).is_none());
        // Suffix lookalike (`notacme.com`) is not own domain and is accepted.
        assert!(pick("mailto:unsub@notacme.com?subject=unsubscribe", &own).is_some());
        // Commas (literal or `%2C`) inside quoted display names must not split recipients.
        assert_eq!(
            split_mailto_addresses(
                "\"Doe%2C Jane\" <jane@example.com>,%22Lee%2C%20Bob%22%20%3Cbob@example.org%3E"
            ),
            vec![
                "\"Doe, Jane\" <jane@example.com>",
                "\"Lee, Bob\" <bob@example.org>",
            ]
        );
        assert!(
            valid_unsubscribe_mailto_recipients(
                "mailto:\"Doe%2C Jane\" <jane@example.com>?subject=unsubscribe"
            )
            .is_some()
        );
        assert!(
            valid_unsubscribe_mailto_recipients(
                "mailto:?to=%22Doe%2C%20Jane%22%20%3Cjane@example.com%3E&subject=unsubscribe"
            )
            .is_some()
        );
        assert!(
            pick(
                "mailto:%22Doe%2C%20Jane%22%20%3Cjane@example.com%3E?subject=unsubscribe",
                &own
            )
            .is_some()
        );
        assert!(
            pick(
                "mailto:?to=%22Doe%2C%20Jane%22%20%3Cjane@example.com%3E&subject=unsubscribe",
                &own
            )
            .is_some()
        );
        assert!(
            pick(
                "mailto:?to=%22Doe%2C%20Jane%22%20%3Cjane@acme.com%3E&subject=unsubscribe",
                &own
            )
            .is_none(),
            "quoted display name with comma must still reject own-domain recipient"
        );
    }
}

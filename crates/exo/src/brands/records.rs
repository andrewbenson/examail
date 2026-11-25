//! What a brand's logo rests on: Gmail's DMARC verdict on a message (its own
//! `Authentication-Results`), and the sending domain's DMARC and BIMI records in DNS.

use exo_store::Header;
use mail_parser::{Address, MessageParser};

/// The domain whose policy Gmail applied to a message that passed DMARC.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct Passed {
    /// The `From` address's domain, lower-cased, aligned with `header.from`.
    pub(super) domain: String,
}

/// Gmail's verdict in `headers`: a single RFC 5322 `From` header naming one mailbox, and Gmail's
/// own `Authentication-Results` (the first naming `mx.google.com`, which Gmail adds above any the
/// message came with) saying `dmarc=pass` for an aligned `header.from` under a policy that
/// quarantines or rejects mail that fails (the subdomain policy for a subdomain).
pub(super) fn passed(headers: &[Header]) -> Option<Passed> {
    let from_domain = single_from_domain(headers)?;
    let results = headers.iter().find_map(|header| {
        let value = header
            .name
            .eq_ignore_ascii_case("Authentication-Results")
            .then_some(header.value.as_str())?;
        let (server, results) = value.split_once(';')?;
        (server.split_whitespace().next()? == "mx.google.com").then_some(results)
    })?;
    let dmarc = outside_comments(results)
        .into_iter()
        .find(|(method, _)| method.trim().starts_with("dmarc="))?;
    let (method, comment) = dmarc;
    let mut words = method.split_whitespace();
    if words.next()? != "dmarc=pass" {
        return None;
    }
    let auth_domain = words
        .find_map(|word| word.strip_prefix("header.from="))?
        .trim_end_matches('.')
        .to_lowercase();
    if auth_domain.is_empty() || !domains_align(&from_domain, &auth_domain) {
        return None;
    }
    let tag = |name: &str| {
        comment
            .split_whitespace()
            .find_map(|tag| tag.strip_prefix(name)?.strip_prefix('='))
            .map(str::to_uppercase)
    };
    let organization = psl::domain_str(&from_domain).unwrap_or(&from_domain);
    let policy = match tag("sp") {
        Some(policy) if organization != from_domain => policy,
        _ => tag("p")?,
    };
    matches!(policy.as_str(), "QUARANTINE" | "REJECT").then_some(Passed {
        domain: from_domain,
    })
}

/// The lower-cased domain of the one mailbox in `headers`' sole `From` header, or `None` when
/// `headers` has zero or multiple `From` headers, multiple mailboxes, group syntax or no valid
/// address.
fn single_from_domain(headers: &[Header]) -> Option<String> {
    let mut from_headers = headers
        .iter()
        .filter(|header| header.name.eq_ignore_ascii_case("From"));
    let value = from_headers.next()?.value.as_str();
    if from_headers.next().is_some() || has_unquoted_list_separator(value) {
        return None;
    }
    let raw = format!("From: {value}\r\n\r\n");
    let parsed = MessageParser::default().parse(raw.as_bytes())?;
    let Address::List(mailboxes) = parsed.from()? else {
        return None;
    };
    let [mailbox] = mailboxes.as_slice() else {
        return None;
    };
    let address = mailbox.address()?.trim();
    if address
        .chars()
        .any(|ch| ch.is_whitespace() || ch.is_control())
    {
        return None;
    }
    let (local, domain) = address.rsplit_once('@')?;
    let domain = domain.trim_end_matches('.').to_lowercase();
    (!local.is_empty()
        && !local.contains('@')
        && !domain.is_empty()
        && !domain.starts_with('.')
        && domain.contains('.'))
    .then_some(domain)
}

/// Whether `header` has `,` or `;` outside RFC 5322 quoted-strings and comments, marking a
/// multi-mailbox list or group syntax.
fn has_unquoted_list_separator(header: &str) -> bool {
    let mut quoted = false;
    let mut escaped = false;
    let mut comments = 0usize;
    for ch in header.chars() {
        if escaped {
            escaped = false;
            continue;
        }
        if ch == '\\' && (quoted || comments > 0) {
            escaped = true;
            continue;
        }
        if comments == 0 && ch == '"' {
            quoted = !quoted;
            continue;
        }
        if !quoted {
            match ch {
                '(' => comments += 1,
                ')' => comments = comments.saturating_sub(1),
                ',' | ';' if comments == 0 => return true,
                _ => {}
            }
        }
    }
    quoted
}

/// Whether `from_domain` and `auth_domain` align under DMARC: exact match or shared registrable
/// organizational domain.
fn domains_align(from_domain: &str, auth_domain: &str) -> bool {
    from_domain == auth_domain
        || psl::domain_str(from_domain)
            .zip(psl::domain_str(auth_domain))
            .is_some_and(|(from_org, auth_org)| from_org == auth_org)
}

/// `results`' `;`-separated methods, each with what its comments say (`(p=REJECT sp=REJECT)`).
fn outside_comments(results: &str) -> Vec<(String, String)> {
    let (mut methods, mut text, mut comment, mut depth) =
        (Vec::new(), String::new(), String::new(), 0usize);
    for character in results.chars() {
        match character {
            '(' => {
                depth += 1;
                if depth == 1 {
                    continue;
                }
            }
            ')' if depth > 0 => {
                depth -= 1;
                if depth == 0 {
                    comment.push(' ');
                    continue;
                }
            }
            ';' if depth == 0 => {
                methods.push((std::mem::take(&mut text), std::mem::take(&mut comment)));
                continue;
            }
            _ => {}
        }
        if depth > 0 {
            comment.push(character);
        } else {
            text.push(character);
        }
    }
    methods.push((text, comment));
    methods
}

/// A DNS TXT record's `tag=value` pairs (DMARC's and BIMI's syntax), tags lower-cased.
fn tags(record: &str) -> Vec<(String, String)> {
    record
        .split(';')
        .filter_map(|pair| {
            let (tag, value) = pair.split_once('=')?;
            Some((tag.trim().to_lowercase(), value.trim().to_owned()))
        })
        .collect()
}

/// Whether `record` is a DMARC record (`v=DMARC1` first) that applies its policy to all of the
/// mail that fails (`pct=100`, the default).
pub(super) fn dmarc_applies_to_all(record: &str) -> bool {
    let tags = tags(record);
    let version = tags
        .first()
        .map(|(tag, value)| (tag.as_str(), value.as_str()));
    version == Some(("v", "DMARC1"))
        && tags
            .iter()
            .find(|(tag, _)| tag == "pct")
            .is_none_or(|(_, pct)| pct == "100")
}

/// A BIMI assertion record's logo and certificate (`l=` and `a=`), each an HTTPS URL.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct Bimi {
    pub(super) logo: Option<String>,
    pub(super) certificate: Option<String>,
}

/// `record` as a BIMI assertion record (`v=BIMI1` first), or `None` when it isn't one.
pub(super) fn bimi(record: &str) -> Option<Bimi> {
    let tags = tags(record);
    let (tag, version) = tags.first()?;
    if (tag.as_str(), version.as_str()) != ("v", "BIMI1") {
        return None;
    }
    let https = |name: &str| {
        tags.iter()
            .find(|(tag, _)| tag == name)
            .map(|(_, value)| value.clone())
            .filter(|value| value.starts_with("https://"))
    };
    Some(Bimi {
        logo: https("l"),
        certificate: https("a"),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn message_headers(from: &str, value: &str) -> Vec<Header> {
        vec![
            Header {
                name: "From".into(),
                value: from.into(),
            },
            Header {
                name: "Authentication-Results".into(),
                value: value.into(),
            },
        ]
    }

    fn results(value: &str) -> Vec<Header> {
        message_headers("\"Brand, Inc.\" <news@brand.example>", value)
    }

    #[test]
    fn gmails_dmarc_pass_under_an_enforcing_policy_counts() {
        let gmail = "mx.google.com;\r\n       dkim=pass header.i=@brand.example header.s=s1;\r\n       spf=pass (google.com: domain of bounce@brand.example designates 192.0.2.1 as permitted sender) smtp.mailfrom=bounce@brand.example;\r\n       dmarc=pass (p=REJECT sp=REJECT dis=NONE) header.from=Brand.example";
        assert_eq!(
            passed(&results(gmail)),
            Some(Passed {
                domain: "brand.example".into()
            })
        );
        let monitoring = gmail.replace("p=REJECT sp=REJECT", "p=NONE sp=NONE");
        assert_eq!(
            passed(&results(&monitoring)),
            None,
            "p=none enforces nothing"
        );
        let failed = gmail.replace("dmarc=pass", "dmarc=fail");
        assert_eq!(passed(&results(&failed)), None);
        let forged = gmail.replace("mx.google.com", "mx.attacker.example");
        assert_eq!(passed(&results(&forged)), None, "only Gmail's own counts");
    }

    #[test]
    fn a_subdomain_takes_the_subdomain_policy() {
        let mail =
            "mx.google.com; dmarc=pass (p=REJECT sp=NONE dis=NONE) header.from=news.brand.co.uk";
        assert_eq!(
            passed(&message_headers("Brand <news@news.brand.co.uk>", mail)),
            None
        );
        let enforced = mail.replace("sp=NONE", "sp=QUARANTINE");
        assert_eq!(
            passed(&message_headers("Brand <news@news.brand.co.uk>", &enforced)),
            Some(Passed {
                domain: "news.brand.co.uk".into()
            })
        );
    }

    #[test]
    fn requires_one_single_mailbox_from_aligned_with_header_from() {
        let gmail =
            "mx.google.com; dmarc=pass (p=REJECT sp=REJECT dis=NONE) header.from=brand.example";
        // Missing From header is rejected.
        assert_eq!(
            passed(&[Header {
                name: "Authentication-Results".into(),
                value: gmail.into(),
            }]),
            None
        );
        // Multiple From headers are rejected.
        assert_eq!(
            passed(&[
                Header {
                    name: "From".into(),
                    value: "news@brand.example".into(),
                },
                Header {
                    name: "From".into(),
                    value: "spoof@paypal.com".into(),
                },
                Header {
                    name: "Authentication-Results".into(),
                    value: gmail.into(),
                },
            ]),
            None
        );
        // Multiple mailboxes or RFC 5322 group syntax in From are rejected.
        assert_eq!(
            passed(&message_headers(
                "news@brand.example, spoof@paypal.com",
                gmail
            )),
            None
        );
        assert_eq!(
            passed(&message_headers("Brand: news@brand.example;", gmail)),
            None
        );
        // Unaligned From vs Authentication-Results header.from is rejected.
        assert_eq!(
            passed(&message_headers("Security <security@paypal.com>", gmail)),
            None
        );
        assert_eq!(
            passed(&message_headers(
                "a@one.co.uk",
                "mx.google.com; dmarc=pass (p=REJECT sp=REJECT) header.from=two.co.uk"
            )),
            None
        );
        // Organizational-domain alignment passes and returns From's domain.
        assert_eq!(
            passed(&message_headers(
                "Alerts <alerts@mail.brand.example>",
                gmail
            )),
            Some(Passed {
                domain: "mail.brand.example".into()
            })
        );
    }

    #[test]
    fn records_are_read_by_their_tags() {
        assert!(dmarc_applies_to_all(
            "v=DMARC1; p=reject; rua=mailto:d@brand.example"
        ));
        assert!(!dmarc_applies_to_all("v=DMARC1; p=quarantine; pct=50"));
        assert!(!dmarc_applies_to_all("v=spf1 -all"));
        assert_eq!(
            bimi("v=BIMI1; l=https://brand.example/logo.svg; a=https://brand.example/vmc.pem"),
            Some(Bimi {
                logo: Some("https://brand.example/logo.svg".into()),
                certificate: Some("https://brand.example/vmc.pem".into()),
            })
        );
        assert_eq!(
            bimi("v=BIMI1; l=http://brand.example/logo.svg; a="),
            Some(Bimi {
                logo: None,
                certificate: None
            }),
            "only HTTPS"
        );
        assert_eq!(bimi("l=https://brand.example/logo.svg"), None);
    }
}

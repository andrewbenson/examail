//! Reads `mailto:` links into the fields of the new message that a click on one opens.

pub(super) use exo_mail::escape_html as escape;
use exo_mail::percent_decode_lossy as percent_decode;

use super::{factory::Fields, recipients};

/// The new message `url` asks for: its addresses (the path and `to=`), `cc`, `bcc`, `subject`
/// and `body` (HTML-escaped, newlines as `<br>`, as compose takes it). `None` for a URL that
/// isn't `mailto:`.
pub(crate) fn fields(url: &str) -> Option<Fields> {
    let rest = exo_mail::strip_ascii_prefix(url, "mailto:")?;
    let (path, query) = rest.split_once('?').unwrap_or((rest, ""));
    let mut fields = Fields::default();
    let mut to = exo_mail::split_mailto_addresses(path);
    for pair in query.split('&').filter(|pair| !pair.is_empty()) {
        let (raw_key, raw_value) = pair.split_once('=').unwrap_or((pair, ""));
        let key = percent_decode(raw_key);
        let list = |raw: &str| {
            exo_mail::split_mailto_addresses(raw)
                .into_iter()
                .map(|address| recipients::parse(address.trim()))
                .filter(|address| !address.email.is_empty() || address.name.is_some())
                .collect::<Vec<_>>()
        };
        match key.to_ascii_lowercase().as_str() {
            "to" => to.extend(exo_mail::split_mailto_addresses(raw_value)),
            "cc" => fields.cc = list(raw_value),
            "bcc" => fields.bcc = list(raw_value),
            // The subject is a single line, so line breaks go.
            "subject" => fields.subject = percent_decode(raw_value).replace(['\r', '\n'], ""),
            "body" => {
                fields.body = escape(&percent_decode(raw_value))
                    .replace("\r\n", "\n")
                    .replace('\n', "<br>");
            }
            _ => {}
        }
    }
    fields.to = to
        .iter()
        .map(|address| recipients::parse(address.trim()))
        .filter(|address| !address.email.is_empty() || address.name.is_some())
        .collect();
    Some(fields)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_addresses_and_headers() {
        let fields = fields("mailto:ann@x.com,Bob%20Lee%20%3Cbob@x.com%3E?cc=c@x.com&Subject=Hello%20there&body=Line%201%0D%0ALine%20%3C2%3E").expect("mailto");
        let to: Vec<&str> = fields
            .to
            .iter()
            .map(|address| address.email.as_str())
            .collect();
        assert_eq!(to, ["ann@x.com", "bob@x.com"]);
        assert_eq!(fields.to[1].name.as_deref(), Some("Bob Lee"));
        assert_eq!(fields.cc[0].email, "c@x.com");
        assert_eq!(fields.subject, "Hello there");
        assert_eq!(fields.body, "Line 1<br>Line &lt;2&gt;");
    }

    #[test]
    fn a_subject_keeps_to_one_line() {
        let fields =
            fields("mailto:boss@x.com?subject=Invoice%0D%0ABcc:%20eve@evil.com").expect("mailto");
        assert_eq!(fields.subject, "InvoiceBcc: eve@evil.com");
    }

    #[test]
    fn only_mailto() {
        assert!(fields("https://x.com").is_none());
        assert!(fields("MAILTO:a@x.com").is_some());
    }

    #[test]
    fn preserves_plus_signs_encoded_commas_and_malformed_percent_sequences() {
        let parsed = fields(
            "mailto:%22Lee%2C%20Bob%22%20%3Cbob+vip@x.com%3E?to=ann+tag@x.com&subject=C++%201+1%20%+a&body=a+b%20%+a",
        )
        .expect("mailto");
        assert_eq!(parsed.to.len(), 2);
        assert_eq!(parsed.to[0].email, "bob+vip@x.com");
        assert_eq!(parsed.to[0].name.as_deref(), Some("Bob Lee"));
        assert_eq!(parsed.to[1].email, "ann+tag@x.com");
        assert_eq!(parsed.subject, "C++ 1+1 %+a");
        assert_eq!(parsed.body, "a+b %+a");

        let plus_subject = fields("mailto:a@x.com?subject=1+1%3D2%20%+a").expect("mailto");
        assert_eq!(plus_subject.subject, "1+1=2 %+a");
        assert_eq!(percent_decode("%+a%-1%zz%41"), "%+a%-1%zzA");
    }
}

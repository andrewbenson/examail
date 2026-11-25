//! Splitting a draft written in another app (Gmail web, Superhuman, Apple Mail) into what the user
//! wrote and the quote that follows it, so the quote can wait behind a `…` as in Examail's own
//! replies.

use crate::dom::{self, NodeRef};

/// How many lone wrappers (a single `<div>` around everything) are looked through for the quote.
const WRAPPERS: usize = 4;

/// `html`'s text before its quote, and the quote with all that follows it (a signature placed
/// after the quote too), or `None` when it has no quote at its top level.
///
/// The quote is the first block that a mail app starts one with: Gmail's and Superhuman's
/// `gmail_quote`, Examail's own `sh-quoted-content`, Apple Mail's `<blockquote type="cite">`, or
/// Outlook's `divRplyFwdMsg`. The line breaks between the text and the quote go with neither.
#[must_use]
pub fn split_draft_quote(html: &str) -> Option<(String, String)> {
    let roots = dom::parse_fragment(html);
    let split = split_nodes(&roots);
    for root in roots {
        dom::drop_tree(root);
    }
    split
}

fn split_nodes(roots: &[NodeRef]) -> Option<(String, String)> {
    let mut nodes = roots.to_vec();
    for _ in 0..=WRAPPERS {
        if let Some(at) = nodes.iter().position(is_quote) {
            let mut end = at;
            while end > 0 && is_blank(&nodes[end - 1]) {
                end -= 1;
            }
            let body: String = nodes[..end].iter().map(dom::outer_html).collect();
            let quote: String = nodes[at..].iter().map(dom::outer_html).collect();
            return Some((body, quote));
        }
        let mut filled = nodes.iter().filter(|node| !is_blank(node));
        match (filled.next(), filled.next()) {
            (Some(only), None) if dom::is(only, "div") => nodes = dom::children(only),
            _ => return None,
        }
    }
    None
}

fn is_quote(node: &NodeRef) -> bool {
    dom::is_element(node)
        && (dom::has_class(node, "gmail_quote")
            || dom::has_class(node, "gmail_quote_container")
            || dom::has_class(node, "sh-quoted-content")
            || (dom::is(node, "blockquote")
                && dom::attr(node, "type").is_some_and(|kind| kind.eq_ignore_ascii_case("cite")))
            || dom::attr(node, "id")
                .is_some_and(|id| id == "divRplyFwdMsg" || id == "appendonsend"))
}

/// Whitespace, a comment, or a line break.
fn is_blank(node: &NodeRef) -> bool {
    dom::is_comment(node)
        || dom::is(node, "br")
        || (dom::is_text(node) && dom::text(node).is_none_or(|text| text.trim().is_empty()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn superhuman_keeps_its_signature_after_the_quote() {
        let draft = "<div class=\"sh-color\">Approved! Go ahead.</div><br><br>\
            <div class='gmail_quote sh-color'>On Wed, Hugo wrote:<br />\
            <blockquote class='gmail_quote'><div>It's yearly</div></blockquote></div>\
            <div><br clear=\"all\"></div><span class=\"gmail_signature_prefix\">-- </span><br>\
            <div class=\"gmail_signature\"><div>Andrew</div></div>";
        let (body, quote) = split_draft_quote(draft).expect("has a quote");
        assert_eq!(body, "<div class=\"sh-color\">Approved! Go ahead.</div>");
        assert!(quote.starts_with("<div class=\"gmail_quote sh-color\">On Wed, Hugo wrote:"));
        assert!(quote.contains("It's yearly") && quote.contains("gmail_signature\"><div>Andrew"));
    }

    #[test]
    fn gmail_web_and_wrapped_drafts_split_inside_their_wrapper() {
        let gmail = "<div dir=\"ltr\">Sounds good</div><br>\
            <div class=\"gmail_quote gmail_quote_container\"><div class=\"gmail_attr\">On Tue, Tom \
            wrote:<br></div><blockquote class=\"gmail_quote\">Heads up</blockquote></div>";
        let (body, quote) = split_draft_quote(gmail).expect("has a quote");
        assert_eq!(body, "<div dir=\"ltr\">Sounds good</div>");
        assert!(quote.contains("Heads up"));

        let wrapped = "<div><div><div>Thanks</div><br></div><br>\
            <div class=\"sh-quoted-content\"><div>Earlier</div></div></div>";
        let (body, quote) = split_draft_quote(wrapped).expect("looks inside the wrapper");
        assert_eq!(body, "<div><div>Thanks</div><br></div>");
        assert_eq!(
            quote,
            "<div class=\"sh-quoted-content\"><div>Earlier</div></div>"
        );
    }

    #[test]
    fn apple_mail_cites_and_new_mail_has_none() {
        let apple = "<div>Yes</div><br><blockquote type=\"cite\">Can you?</blockquote>";
        assert_eq!(
            split_draft_quote(apple).map(|(body, _)| body).as_deref(),
            Some("<div>Yes</div>")
        );
        assert_eq!(split_draft_quote("<div>Hello</div><div>there</div>"), None);
        // A quote nested in what's written isn't the reply's quote.
        assert_eq!(
            split_draft_quote("<div>a</div><div><p>b</p><div class=\"gmail_quote\">c</div></div>"),
            None
        );
    }
}

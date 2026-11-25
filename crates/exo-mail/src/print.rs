//! A message body for printing: the body under its `@media print` rules, as markup for a browser
//! engine's shadow root, which lays it out and paginates it the way Chromium does (Exo's print view
//! does this with WebKit).
//!
//! It is [`BodyView`](crate::BodyView)'s document without what only Blitz needs: no host
//! isolation (a real shadow root isolates) and no presentational hints (a browser maps those
//! attributes itself). It is always Snow's, because the Carbon rules for bodies, like the thread
//! view's, are screen-only. So Carbon prints exactly as Snow does.
//!
//! Remote content can't load from the engine. Every remote URL is rewritten to
//! [`REMOTE_SCHEME_PREFIX`]`http(s)`, which only the print view's own handler loads, through the
//! same guarded fetches as a body on screen. With remote content blocked, it's rewritten to
//! [`BLOCKED_SCHEME`] instead, which nothing loads.

use std::sync::LazyLock;

use regex::{Captures, Regex};

use crate::{Render, Theme, ViewOptions, dom, remote, view};

/// The body's print rules, after its own stylesheets.
const PRINT_CSS: &str = include_str!("print.css");

/// What a printed body's remote `https:`/`http:` URLs start with instead: `exo-https:`,
/// `exo-http:`. A protocol-relative URL resolves against the document, which is the print
/// view's job to put on `exo-https:` too.
pub const REMOTE_SCHEME_PREFIX: &str = "exo-";
/// The scheme a blocked body's remote URLs get, which no handler answers.
pub const BLOCKED_SCHEME: &str = "exo-blocked";

/// A remote URL's scheme (or a protocol-relative URL's `//`) in CSS: `url(…)` or `@import "…"`.
static CSS_URL: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r#"(?i)(url\(\s*['"]?\s*|@import\s*['"]\s*)((?:exo-)?https?:|//)"#)
        .expect("valid regex")
});
/// A remote candidate's scheme in a `srcset`.
static SRCSET_URL: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?i)(^|,)(\s*)((?:exo-)?https?:|//)").expect("valid regex"));

/// `render`'s body as it prints: the markup that goes inside the message's shadow root. It has its
/// quoted part when `show_quoted` (the `…` itself doesn't print), and loads nothing remote when
/// `block_remote_content`. A body shown shrunk to fit its column ([`BodyView::zoom`]) prints
/// shrunk as much, `zoom`.
///
/// [`BodyView::zoom`]: crate::BodyView::zoom
pub fn printable_body(
    render: &Render,
    show_quoted: bool,
    block_remote_content: bool,
    zoom: f32,
) -> String {
    let options = ViewOptions {
        theme: Theme::Light,
        show_quoted,
        ..ViewOptions::default()
    };
    let root_class = view::root_class(render, options);
    let mut printed = Render {
        unquoted_html: rewrite_html(&render.unquoted_html, block_remote_content),
        quoted_html: if show_quoted {
            rewrite_html(&render.quoted_html, block_remote_content)
        } else {
            String::new()
        },
        has_quoted_content: render.has_quoted_content,
        body_attributes: render.body_attributes.clone(),
        unquoted_is_marketing: render.unquoted_is_marketing,
        quoted_is_marketing: render.quoted_is_marketing,
        ..Render::default()
    };
    if let Some(style) = &mut printed.body_attributes.style {
        *style = rewrite_css(style, block_remote_content);
    }
    view::shadow_root(&printed, show_quoted, &root_class, "", PRINT_CSS, zoom)
}

/// Rewrites an HTML fragment's remote URLs for the print view: to `exo-https:`/`exo-http:` when
/// loading remote content, or to [`BLOCKED_SCHEME`] (with `data-exo-blocked` on remote images)
/// when `block_remote_content` is set.
pub fn rewrite_print_html(html: &str, block_remote_content: bool) -> String {
    rewrite_html(html, block_remote_content)
}

/// `html` (a fragment) with its remote URLs rewritten, and with `block`, its remote images
/// marked for `body.css`'s blocked-image box.
fn rewrite_html(html: &str, block: bool) -> String {
    if html.is_empty() {
        return String::new();
    }
    // Inside a `<div>` in the body, even a leading `<style>` stays where it is.
    let document = dom::parse(&format!(
        "<!DOCTYPE html><html><body><div>{html}</div></body></html>"
    ));
    let Some(root) =
        dom::body(&document).and_then(|body| dom::element_children(&body).into_iter().next())
    else {
        dom::drop_tree(document);
        return html.to_owned();
    };
    for element in dom::select(&root, dom::is_element) {
        let svg_image = dom::is_svg_image(&element);
        if block
            && ((dom::is(&element, "img")
                && (remote::is_remote(&dom::attr(&element, "src").unwrap_or_default())
                    || remote::srcset_is_remote(
                        &dom::attr(&element, "srcset").unwrap_or_default(),
                    )))
                || (svg_image
                    && ["href", "xlink:href"].iter().any(|name| {
                        remote::is_remote(&dom::attr(&element, name).unwrap_or_default())
                    })))
        {
            // Marks the image for `body.css`'s box. The print view sizes it as Chromium sizes a
            // broken image.
            dom::set_attr(&element, "data-exo-blocked", "");
        }
        // A lazy image would never load, because a page that prints isn't scrolled.
        if dom::is(&element, "img") {
            dom::remove_attr(&element, "loading");
        }
        let attrs: &[&str] = if svg_image {
            &["src", "background", "poster", "href", "xlink:href"]
        } else {
            &["src", "background", "poster"]
        };
        for &name in attrs {
            if let Some(url) = dom::attr(&element, name)
                && let Some(printed) = rewrite_url(&url, block)
            {
                dom::set_attr(&element, name, &printed);
            }
        }
        if let Some(srcset) = dom::attr(&element, "srcset") {
            let printed = SRCSET_URL.replace_all(&srcset, |found: &Captures| {
                format!("{}{}{}", &found[1], &found[2], scheme(&found[3], block))
            });
            dom::set_attr(&element, "srcset", &printed);
        }
        if let Some(style) = dom::attr(&element, "style") {
            dom::set_attr(&element, "style", &rewrite_css(&style, block));
        }
        if dom::is(&element, "style") {
            let css = dom::text_content(&element);
            let printed = rewrite_css(&css, block);
            if printed != css {
                dom::set_children(&element, vec![dom::text_node(&printed)]);
            }
        }
    }
    let out = dom::inner_html(&root);
    dom::drop_tree(document);
    out
}

/// `css` with the URLs it loads from the network rewritten.
fn rewrite_css(css: &str, block: bool) -> String {
    CSS_URL
        .replace_all(css, |found: &Captures| {
            format!("{}{}", &found[1], scheme(&found[2], block))
        })
        .into_owned()
}

/// A remote `url` as the printed body names it, or `None` for one that isn't remote.
fn rewrite_url(url: &str, block: bool) -> Option<String> {
    let trimmed = url.trim_start();
    if !remote::is_remote(trimmed) {
        return None;
    }
    let (start, rest) = match trimmed.strip_prefix("//") {
        Some(rest) => ("//", rest),
        None => {
            let colon = trimmed.find(':')?;
            (&trimmed[..=colon], &trimmed[colon + 1..])
        }
    };
    Some(format!("{}{rest}", scheme(start, block)))
}

/// What replaces a remote URL's `https:`, `http:`, `exo-https:`, `exo-http:` or `//`.
fn scheme(start: &str, block: bool) -> String {
    let start = start.to_ascii_lowercase();
    match (start.as_str(), block) {
        ("//", false) => format!("{REMOTE_SCHEME_PREFIX}https://"),
        ("//", true) => format!("{BLOCKED_SCHEME}://"),
        (_, true) => format!("{BLOCKED_SCHEME}:"),
        (already, false) if already.starts_with(REMOTE_SCHEME_PREFIX) => already.to_owned(),
        (raw, false) => format!("{REMOTE_SCHEME_PREFIX}{raw}"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::BodyAttributes;

    #[test]
    fn remote_urls_load_through_the_print_view() {
        let html = rewrite_html(
            r#"<img src="https://x.io/a.png" srcset="cid:b 1x, HTTP://x.io/b.png 2x"><table background="//x.io/bg.png"><tr><td style="background:url( 'https://x.io/c.png' )">x</td></tr></table><style>@import "http://x.io/a.css"; p { background: url(//x.io/d.png) }</style><p>See https://x.io/e.png</p><img src="cid:f"><img src="data:image/gif;base64,R0lG">"#,
            false,
        );
        assert!(html.contains(r#"src="exo-https://x.io/a.png""#), "{html}");
        assert!(
            html.contains(r#"srcset="cid:b 1x, exo-http://x.io/b.png 2x""#),
            "{html}"
        );
        assert!(
            html.contains(r#"background="exo-https://x.io/bg.png""#),
            "{html}"
        );
        assert!(html.contains("url( 'exo-https://x.io/c.png' )"), "{html}");
        assert!(
            html.contains(r#"@import "exo-http://x.io/a.css""#)
                && html.contains("url(exo-https://x.io/d.png)"),
            "{html}"
        );
        assert!(
            html.contains("See https://x.io/e.png"),
            "text stays: {html}"
        );
        assert!(
            html.contains(r#"src="cid:f""#) && html.contains(r#"src="data:image/gif;base64,R0lG""#),
            "{html}"
        );
        assert!(!html.contains("data-exo-blocked"), "{html}");
    }

    #[test]
    fn blocked_bodies_load_nothing_remote() {
        let html = rewrite_html(
            r#"<img src="https://x.io/a.png" alt="A"><img src="cid:b"><svg><image xlink:href="https://x.io/s.png"></image><feImage href="//x.io/f.png"></feImage></svg><div style="background:url(//x.io/c.png)">x</div>"#,
            true,
        );
        assert!(
            html.contains(r#"src="exo-blocked://x.io/a.png""#) && html.contains("data-exo-blocked"),
            "{html}"
        );
        assert!(
            html.contains(r#"xlink:href="exo-blocked://x.io/s.png""#)
                && html.contains(r#"href="exo-blocked://x.io/f.png""#),
            "{html}"
        );
        assert!(
            html.contains(r#"<img src="cid:b">"#),
            "an inline attachment still shows: {html}"
        );
        assert!(html.contains("url(exo-blocked://x.io/c.png)"), "{html}");
        assert!(!html.contains("https:"), "{html}");
    }

    #[test]
    fn a_body_prints_in_snow_without_its_quote_unless_shown() {
        let render = Render {
            unquoted_html: "<p>New</p>".into(),
            quoted_html: "<blockquote>Old</blockquote>".into(),
            has_quoted_content: true,
            body_attributes: BodyAttributes {
                style: Some("background:url(https://x.io/bg.png)".into()),
                ..BodyAttributes::default()
            },
            ..Render::default()
        };
        let folded = printable_body(&render, false, false, 1.0);
        assert!(
            folded.contains("ShadowHTML Theme-light Theme-snow"),
            "{folded}"
        );
        assert!(
            folded.contains("New") && !folded.contains("Old"),
            "{folded}"
        );
        assert!(
            folded.contains("exo-https://x.io/bg.png"),
            "the body's own background: {folded}"
        );
        assert!(
            folded.ends_with(&format!("<style>{PRINT_CSS}</style></div></div>")),
            "print rules come last"
        );
        let open = printable_body(&render, true, false, 1.0);
        assert!(
            open.contains("Old") && open.contains("Exo-quoting-ellipsis isOpen"),
            "{open}"
        );
        assert!(!open.contains("zoom"), "{open}");
        let shrunk = printable_body(&render, false, false, 0.8);
        assert!(
            shrunk.contains(
                "ShadowWrapper styleQuotedContent styleUnquotedContent\" style=\"zoom: 0.8\""
            ),
            "shrunk as on screen: {shrunk}"
        );
    }

    #[test]
    fn preexisting_exo_http_schemes_are_blocked_and_never_double_prefixed() {
        let input = concat!(
            r#"<img src="exo-https://evil.example/a.png" srcset="cid:b 1x, EXO-HTTP://evil.example/b.png 2x">"#,
            r#"<div style="background:url('exo-https://evil.example/c.png')">x</div>"#,
            r#"<style>@import "EXO-HTTPS://evil.example/a.css"; p { background: url(exo-http://evil.example/d.png) }</style>"#,
        );
        let blocked = rewrite_print_html(input, true);
        assert!(
            blocked.contains(r#"src="exo-blocked://evil.example/a.png""#)
                && blocked.contains("data-exo-blocked"),
            "{blocked}"
        );
        assert!(
            blocked.contains(r#"srcset="cid:b 1x, exo-blocked://evil.example/b.png 2x""#),
            "{blocked}"
        );
        assert!(
            blocked.contains("url('exo-blocked://evil.example/c.png')"),
            "{blocked}"
        );
        assert!(
            blocked.contains(r#"@import "exo-blocked://evil.example/a.css""#)
                && blocked.contains("url(exo-blocked://evil.example/d.png)"),
            "{blocked}"
        );
        assert!(
            !blocked.to_ascii_lowercase().contains("exo-http"),
            "no exo-http(s) should survive blocking: {blocked}"
        );

        let allowed = rewrite_print_html(input, false);
        assert!(
            allowed.contains(r#"src="exo-https://evil.example/a.png""#)
                && allowed.contains(r#"srcset="cid:b 1x, exo-http://evil.example/b.png 2x""#)
                && allowed.contains("url('exo-https://evil.example/c.png')")
                && allowed.contains(r#"@import "exo-https://evil.example/a.css""#)
                && allowed.contains("url(exo-http://evil.example/d.png)"),
            "{allowed}"
        );
        assert!(
            !allowed.contains("exo-exo-"),
            "must never double-prefix: {allowed}"
        );

        let render = Render {
            unquoted_html: input.into(),
            body_attributes: BodyAttributes {
                style: Some("background:url(exo-https://evil.example/bg.png)".into()),
                ..BodyAttributes::default()
            },
            ..Render::default()
        };
        let printed_blocked = printable_body(&render, false, true, 1.0);
        assert!(
            !printed_blocked.to_ascii_lowercase().contains("exo-http"),
            "printable_body must block pre-existing exo-http(s): {printed_blocked}"
        );
        assert!(
            printed_blocked.contains("url(exo-blocked://evil.example/bg.png)"),
            "{printed_blocked}"
        );
    }
}

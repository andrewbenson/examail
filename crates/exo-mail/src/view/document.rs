//! Assembling a message's shadow-root wrapper, CSS layers, dark-mode and marketing classes, and
//! constructing its Blitz [`HtmlDocument`].

use std::sync::Arc;

use blitz_dom::{DocumentConfig, UserAgentStylesheet, local_name};
use blitz_html::HtmlDocument;
use blitz_traits::shell::{ColorScheme, Viewport};

use super::{Theme, ViewOptions, images::mark_blocked_images};
use crate::{Fonts, Render, dom::escape_attribute, hints, net::FetchProvider};

/// The stylesheet that precedes the message's own CSS.
const BODY_CSS: &str = include_str!("../body.css");
/// The personal-mail restyle that follows it.
const RESTYLE_CSS: &str = include_str!("../restyle.css");

/// Where relative URLs resolve. The pipeline leaves none in `src`/`href`, but a sender's CSS can
/// still say `url(a.png)`. This reserved domain never resolves, and [`FetchProvider`] drops it.
pub(crate) const BASE_URL: &str = "https://exo.invalid/";

/// The narrowest window. Email `@media` queries see the window, not the message column, and a
/// window is never narrower than this.
pub(super) const MIN_WINDOW_WIDTH: f32 = 780.0;

/// Shadow-root isolation, recreated in a plain document. A body never sees `html`/`body` rules
/// from the sender's CSS (a shadow root has neither element). `:host { all: initial }` stops
/// inheritance at the boundary, and `all: initial` on `<body>` does that here. User-agent
/// `!important` beats any author rule, so senders can't undo it. The message column's width comes
/// in through `--exo-width` (see [`super::BodyView::layout`]).
///
/// `.ShadowBody` doesn't clip a too-wide body or scroll it itself. The body is painted whole, and
/// the app shows the part scrolled into view (see [`super::Region::width`]). `flow-root` keeps the
/// block formatting context `overflow-x: auto` would make.
fn host_css(rem: f32) -> String {
    format!(
        "html, body {{ all: initial !important; display: block !important; }}
         html {{ font-size: {rem}px !important; }}
         .ShadowHTML {{ width: var(--exo-width) !important; }}
         .ShadowHTML, .ShadowBody {{ display: flow-root !important; overflow: visible !important; }}"
    )
}

/// Builds the Blitz [`HtmlDocument`] standing in for `render`'s shadow root, returning the
/// document, its `.ShadowHTML` root node, and its root class string.
pub(super) fn build_document(
    render: &Render,
    options: ViewOptions,
    fonts: &Fonts,
    provider: Option<Arc<FetchProvider>>,
) -> (HtmlDocument, blitz_dom::NodeId, String) {
    enable_has_selector();
    let root_class = root_class(render, options);
    let html = compose(render, options, &root_class);
    let config = DocumentConfig {
        ua_stylesheets: vec![
            UserAgentStylesheet::default_sheet(),
            UserAgentStylesheet::parse(&host_css(options.rem)),
        ],
        base_url: Some(BASE_URL.to_owned()),
        font_ctx: Some(fonts.context()),
        net_provider: provider.map(|provider| provider as _),
        viewport: Some(viewport(MIN_WINDOW_WIDTH, options.theme)),
        ..DocumentConfig::default()
    };
    let mut document = HtmlDocument::from_html(&html, config);
    // The selection colour: macOS's highlight colour in Snow, rgba(106, 134, 173, .996) in
    // Carbon, as the editor's.
    document.set_text_selection_color(match options.theme {
        Theme::Light => blitz_dom::util::Color::from_rgb8(0xb4, 0xd5, 0xfe),
        Theme::Dark => blitz_dom::util::Color::from_rgba8(106, 134, 173, 254),
    });
    let root = document
        .query_selector(".ShadowHTML")
        .ok()
        .flatten()
        .expect("the composed document has a .ShadowHTML root");
    if let Some(dynamic_hints) = hints::dynamic_rules(&document, root)
        && let Some(hints_style) = find_first_style(&document, root)
    {
        document
            .mutate()
            .append_text_to_node(hints_style, &dynamic_hints)
            .ok();
    }
    if options.block_remote_content {
        mark_blocked_images(&mut document);
    }
    (document, root, root_class)
}

/// A viewport `width` CSS px wide at 1×, because layout happens in CSS px (see [`super::BodyView`]).
pub(super) fn viewport(width: f32, theme: Theme) -> Viewport {
    let scheme = match theme {
        Theme::Light => ColorScheme::Light,
        Theme::Dark => ColorScheme::Dark,
    };
    Viewport::new(width.ceil() as u32, 1000, 1.0, scheme)
}

/// Turns on Stylo's `:has()`, which Blitz leaves off and the body CSS uses to hide Outlook's
/// `Image removed by sender.` boxes (`body.css`). It's a process-wide preference, and it has to be
/// on before a body's stylesheets are parsed.
fn enable_has_selector() {
    style_config::set_pref!("layout.css.has-selector.enabled", true);
}

/// The classes of the `.ShadowHTML` element.
pub(crate) fn root_class(render: &Render, options: ViewOptions) -> String {
    let theme = match options.theme {
        Theme::Light => "Theme-light Theme-snow",
        Theme::Dark => "Theme-dark Theme-carbon",
    };
    let marketing = if render.unquoted_is_marketing {
        " isMarketing"
    } else {
        ""
    };
    format!("ShadowHTML {theme} Platform-mac{marketing}")
}

/// The first `<style>` element under `root` (the hints stylesheet inside `.ShadowBody`).
fn find_first_style(
    document: &blitz_dom::BaseDocument,
    root: blitz_dom::NodeId,
) -> Option<blitz_dom::NodeId> {
    let shadow_body = document.get_node(root)?.children.first().copied()?;
    document
        .get_node(shadow_body)?
        .children
        .iter()
        .copied()
        .find(|&id| {
            document
                .get_node(id)
                .and_then(|node| node.element_data())
                .is_some_and(|el| el.name.local == local_name!("style"))
        })
}

/// The document standing in for a body's shadow root.
fn compose(render: &Render, options: ViewOptions, root_class: &str) -> String {
    format!(
        "<!DOCTYPE html><html><head></head><body>{}</body></html>",
        shadow_root(
            render,
            options.show_quoted,
            root_class,
            hints::STATIC_RULES,
            "",
            1.0
        )
    )
}

/// What a body's shadow root holds, `.ShadowHTML` down: the body stylesheet after
/// `first_css`, the unquoted part with the original `<body>` attributes, the `…` and the quoted
/// part when it's shown, then the restyle sheet, the fixes for the message's sender and
/// `last_css` (when there are any). A `zoom` other than 1 shrinks the content (its `.ShadowWrapper`) by that much.
pub(crate) fn shadow_root(
    render: &Render,
    show_quoted: bool,
    root_class: &str,
    first_css: &str,
    last_css: &str,
    zoom: f32,
) -> String {
    let mut wrapper = String::from("exo-color-transparent-bg ShadowWrapper");
    if !render.quoted_is_marketing {
        wrapper.push_str(" styleQuotedContent");
    }
    if !render.unquoted_is_marketing {
        wrapper.push_str(" styleUnquotedContent");
    }
    let body = &render.body_attributes;
    let mut unquoted_class = String::from("unquoted-content exo-color-black");
    if !body.has_background {
        unquoted_class.push_str(" exo-color-transparent-bg");
    }
    if let Some(class) = &body.class {
        unquoted_class.push(' ');
        unquoted_class.push_str(class);
    }
    let mut unquoted_attributes = format!(" class=\"{}\"", escape_attribute(&unquoted_class));
    if let Some(id) = &body.id {
        unquoted_attributes.push_str(&format!(" id=\"{}\"", escape_attribute(id)));
    }
    if let Some(style) = &body.style {
        unquoted_attributes.push_str(&format!(" style=\"{}\"", escape_attribute(style)));
    }
    let quoted = if !render.has_quoted_content {
        String::new()
    } else if show_quoted {
        format!(
            "<div class=\"Exo-quoting-ellipsis isOpen\"><span></span></div><div id=\"quoted-content\">{}</div>",
            render.quoted_html
        )
    } else {
        "<div class=\"Exo-quoting-ellipsis\"><span></span></div>".to_owned()
    };
    let style = |css: &str| {
        if css.is_empty() {
            String::new()
        } else {
            format!("<style>{css}</style>")
        }
    };
    let (sender, last) = (style(render.sender_css), style(last_css));
    format!(
        "<div class=\"{root_class}\"><div class=\"ShadowBody\">\
         <style>{first_css}</style><style>{BODY_CSS}</style>\
         <div class=\"{wrapper}\"{zoom}><div{unquoted_attributes}>{unquoted}</div>{quoted}</div>\
         <style>{RESTYLE_CSS}</style>{sender}{last}\
         </div></div>",
        unquoted = render.unquoted_html,
        zoom = if zoom == 1.0 {
            String::new()
        } else {
            format!(" style=\"zoom: {zoom}\"")
        },
    )
}

#[cfg(test)]
mod tests {
    use std::sync::LazyLock;

    use regex::Regex;

    use super::*;
    use crate::{Message, dom, purify, render};

    /// A small xorshift generator, so failures reproduce.
    struct Rng(u64);

    impl Rng {
        fn below(&mut self, n: usize) -> usize {
            self.0 ^= self.0 << 13;
            self.0 ^= self.0 >> 7;
            self.0 ^= self.0 << 17;
            (self.0 % n as u64) as usize
        }

        fn pick<'a>(&mut self, items: &[&'a str]) -> &'a str {
            items[self.below(items.len())]
        }

        fn pick_word<'a>(&mut self, words: &'a str) -> &'a str {
            let words: Vec<&str> = words.split_ascii_whitespace().collect();
            self.pick(&words)
        }
    }

    const TAGS: &str = "\
        div p span a b table tr td blockquote img br style script svg math mtext mglyph \
        malignmark mi foreignObject annotation-xml title textarea noscript xmp iframe template \
        form select option noembed plaintext desc image use o:p base link meta font pre li hr \
        video object embed body html head";
    const ATTRIBUTES: &[(&str, &[&str])] = &[
        (
            "href",
            &[
                "javascript:alert(1)",
                "&#1;javascript:x",
                " https://a.co ",
                "a@b.co",
                "x.io",
                "#q",
                "vbscript:x",
            ],
        ),
        (
            "src",
            &[
                "https://t.yesware.com/t.gif",
                "cid:a@b",
                "x.png",
                "data:image/gif;base64,R0lG",
                "\tjavascript:x",
                "cid:1f600@goomoji.gmail",
            ],
        ),
        ("xlink:href", &["javascript:x", "data:text/html,x"]),
        ("onerror", &["alert(1)"]),
        (
            "style",
            &[
                "color:red",
                "position:fixed;width:100vw",
                "display:none",
                "border:1px solid",
                "font-size:30px",
                "background:url(x)",
            ],
        ),
        (
            "class",
            &[
                "gmail_quote",
                "gmail_signature",
                "exo-date",
                "sh-date",
                "Exo-quoting-ellipsis",
                "ShadowHTML",
                "exo-external-warning-banner",
            ],
        ),
        ("align", &["center", "left"]),
        ("width", &["1", "200"]),
        ("height", &["1"]),
        ("face", &["</style><img src=x onerror=1>", "Arial\"; x"]),
        ("color", &["#ff0000", "red;}*{color:blue"]),
        ("cellpadding", &["4"]),
        ("background", &["https://x.io/b.png", "javascript:x"]),
        ("attributename", &["href"]),
        ("title", &["--><img src=x onerror=1>"]),
        ("srcset", &["https://x.io/a.png 1x"]),
    ];
    const TEXTS: &[&str] = &[
        "hello there",
        "<img src=x onerror=alert(1)>",
        "&lt;script&gt;",
        "</style>",
        "<!--",
        "-->",
        "On Mon, Nov 16, 2026 at 5:54 PM Ann Lee <ann@x.co> wrote:",
        "see you tomorrow",
        "a.com",
        "--",
        "Sent from my iPhone",
        "Ann Lee",
        "\u{0}",
        "<![CDATA[x]]>",
        "&#0;",
        "\u{2028}",
        "é✓",
        "From: Ann\nSent: Mon\nTo: Bob\nSubject: Hi",
        "Best,",
        "> quoted",
        "unsubscribe",
        "\n\n",
        // Known mutation XSS: markup that only becomes elements when parsed a second time.
        "<math><mtext><table><mglyph><style><img src=x onerror=alert(1)>",
        "<form><math><mtext></form><form><mglyph><style></math><img src onerror=alert(1)>",
        "<svg></p><style><a id=\"</style><img src=1 onerror=alert(1)>\">",
    ];

    fn html(rng: &mut Rng, depth: usize) -> String {
        let mut out = String::new();
        for _ in 0..=rng.below(4) {
            match rng.below(7) {
                0 | 1 => out.push_str(rng.pick(TEXTS)),
                2 => out.push_str(&format!("<!--{}-->", rng.pick(TEXTS))),
                3 => out.push_str(&format!("</{}>", rng.pick_word(TAGS))),
                _ => {
                    let tag = rng.pick_word(TAGS);
                    out.push_str(&format!("<{tag}"));
                    for _ in 0..rng.below(3) {
                        let (name, values) = ATTRIBUTES[rng.below(ATTRIBUTES.len())];
                        out.push_str(&format!(" {name}=\"{}\"", rng.pick(values)));
                    }
                    out.push('>');
                    if depth < 6 {
                        out.push_str(&html(rng, depth + 1));
                    }
                    if rng.below(4) != 0 {
                        out.push_str(&format!("</{tag}>"));
                    }
                }
            }
        }
        out
    }

    /// What a URL parser sees of an attribute value.
    fn compact(value: &str) -> String {
        value
            .chars()
            .filter(|&c| c > ' ' && !c.is_whitespace())
            .collect::<String>()
            .to_ascii_lowercase()
    }

    /// Everything the composed document gives Blitz is something the sanitizer allows.
    fn assert_sanitized(html: &str) {
        static SCRIPT_URL: LazyLock<Regex> =
            LazyLock::new(|| Regex::new(r"^[0-9a-z_]+script:").expect("valid regex"));
        let document = dom::parse(html);
        for element in dom::select(&document, |_| true) {
            let tag = dom::tag(&element).unwrap_or_default().to_ascii_lowercase();
            assert!(purify::TAGS.contains(tag.as_str()), "<{tag}> in {html}");
            for name in ["onerror", "onload", "onclick"] {
                assert!(!dom::has_attr(&element, name), "{name} in {html}");
            }
            for name in [
                "href",
                "src",
                "xlink:href",
                "action",
                "background",
                "poster",
            ] {
                if let Some(value) = dom::attr(&element, name) {
                    assert!(
                        !SCRIPT_URL.is_match(&compact(&value)),
                        "{name}={value:?} in {html}"
                    );
                }
            }
            let source_attrs: &[&str] = match tag.as_str() {
                "img" => &["src"],
                "image" | "feimage" => &["href", "xlink:href"],
                _ => &[],
            };
            for &attr in source_attrs {
                if let Some(src) = dom::attr(&element, attr) {
                    let fine = src.is_empty()
                        || ["http:", "https:", "data:", "cid:"].iter().any(|scheme| {
                            src.get(..scheme.len())
                                .is_some_and(|head| head.eq_ignore_ascii_case(scheme))
                        });
                    assert!(fine, "{tag} {attr}={src:?} in {html}");
                }
            }
        }
        dom::drop_tree(document);
    }

    #[test]
    fn hostile_html_stays_sanitized() {
        let mut rng = Rng(0x2545_f491_4f6c_dd1d);
        // `FUZZ_CASES=100000` searches harder.
        let cases = std::env::var("FUZZ_CASES").map_or(300, |n| n.parse().expect("a count"));
        for _ in 0..cases {
            let previous = Message {
                html: html(&mut rng, 0),
                from_email: "ann@x.co".into(),
                from_name: "Ann Lee".into(),
                ..Message::default()
            };
            let message = Message {
                html: format!(
                    "{}<blockquote>{}</blockquote>",
                    html(&mut rng, 0),
                    previous.html
                ),
                from_email: "bob@y.co".into(),
                from_name: "Bob".into(),
                attachment_names: vec!["a.png".into()],
                ..Message::default()
            };
            println!("{}", message.html);
            let first = render(&previous, None, &[]);
            let reply = render(&message, Some((&previous, &first)), &[]);
            for rendered in [&first, &reply] {
                let options = ViewOptions {
                    show_quoted: true,
                    ..ViewOptions::default()
                };
                let hints = hints::stylesheet(&format!(
                    "{}{}",
                    rendered.unquoted_html, rendered.quoted_html
                ));
                let full = format!(
                    "<!DOCTYPE html><html><head></head><body>{}</body></html>",
                    shadow_root(
                        rendered,
                        options.show_quoted,
                        &root_class(rendered, options),
                        &hints,
                        "",
                        1.0,
                    )
                );
                assert_sanitized(&full);
            }
        }
    }
}

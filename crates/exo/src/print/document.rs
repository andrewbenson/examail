//! The page a conversation prints from. It's HTML for the open thread under the print rules
//! (`print.css`), with each body in a shadow root of its own (`exo_mail::printable_body`), and it
//! loads nothing but what the print view's handler answers.

use std::fmt::Write as _;

use egui::Color32;
use exo_mail::escape_html as text;
use exo_store::Attachments;

use super::{
    Conversation, Draft, DraftHeader, File, Header, Invite, InviteValue, Message, Row, Run, When,
};

/// The stylesheet for everything but the bodies.
const PRINT_CSS: &str = include_str!("print.css");

/// The scheme Exo's own resources load from: `exo-print://font/<name>.ttf`.
pub(super) const SCHEME: &str = "exo-print";
/// Where a body's relative URLs resolve, which is a reserved domain on the remote scheme that the
/// handler refuses (as with `exo_mail`'s base on screen).
pub(super) const BASE_URL: &str = "exo-https://exo.invalid/";

/// Only what the print view's handler answers loads: fonts from Exo, a body's remote content
/// through the guarded fetches, its inline attachments, and `data:` images. No scripts at all.
const CONTENT_SECURITY_POLICY: &str = "default-src 'none'; img-src exo-https: exo-http: cid: data:; style-src 'unsafe-inline' exo-https: exo-http:; font-src exo-print: exo-https: exo-http: data:";
/// Tighter CSP when every message and draft in the document has remote content blocked, omitting
/// `exo-https:` and `exo-http:` altogether.
const CONTENT_SECURITY_POLICY_BLOCKED: &str =
    "default-src 'none'; img-src cid: data:; style-src 'unsafe-inline'; font-src exo-print: data:";

/// The legacy icons the printed page shows, each in the color it prints in.
const PAPERCLIP: (&str, &str) = ("legacy/DEPRECATED_paperclip", "#000");
const CALENDAR: (&str, &str) = ("legacy/DEPRECATED_calendar", "#000");
const COLLAPSE: (&str, &str) = ("legacy/DEPRECATED_collapse", "rgba(0,0,0,0.7)");
const PIXEL_BLOCKING: (&str, &str) = ("legacy/DEPRECATED_pixelBlocking", "rgba(0,0,0,0.45)");
const CROSS: (&str, &str) = ("css/isInvalid-cross", "#fff");

/// A rendered print document and whether any message or draft in it loads remote content.
pub(super) struct Rendered {
    pub(super) html: String,
    pub(super) allow_remote: bool,
}

/// The page for `conversation`. When `block_draft_remote_content` is set, remote URLs in open
/// drafts are rewritten to `exo-blocked:`; if no message body or draft loads remote content, the
/// page's CSP omits `exo-https:` and `exo-http:` and [`Rendered::allow_remote`] is `false`.
pub(super) fn html(conversation: &Conversation, block_draft_remote_content: bool) -> Rendered {
    let icon = |(name, color): (&str, &str)| -> String {
        exo_ui::icon_svg(name)
            .map(|svg| svg.replace("\"#fff\"", &format!("\"{color}\"")))
            .unwrap_or_default()
    };
    let icons = Icons {
        paperclip: icon(PAPERCLIP),
        calendar: icon(CALENDAR),
        collapse: icon(COLLAPSE),
        pixel_blocking: icon(PIXEL_BLOCKING),
        cross: icon(CROSS),
    };
    let mut body_html = String::new();
    let mut allow_remote = false;
    header(&mut body_html, conversation);
    let scrolled = if conversation.scrolled {
        " isScrolled"
    } else {
        ""
    };
    let _ = write!(
        body_html,
        "<div class=\"ThreadPane-messages{scrolled}\"><div class=\"ThreadPane-table\">"
    );
    for message in &conversation.messages {
        match message {
            Message::Collapsed {
                from,
                snippet,
                icon,
                when,
            } => {
                let clip = match icon {
                    Attachments::Files => format!(
                        "<div class=\"MessagePane-attachmentIcon\">{}</div>",
                        icons.paperclip
                    ),
                    Attachments::CalendarInvite => format!(
                        "<div class=\"MessagePane-calendarIcon\">{}</div>",
                        icons.calendar
                    ),
                    Attachments::None => String::new(),
                };
                let _ = write!(
                    body_html,
                    "<div class=\"MessagePane-collapsed\"><div class=\"MessagePane-from\">{}</div><div class=\"MessagePane-snippet\">{}</div><div class=\"MessagePane-datetime\">{clip}{}</div></div>",
                    text(from),
                    text(snippet),
                    when_html(when)
                );
            }
            Message::Marker {
                text: body,
                link,
                when,
            } => {
                let link = link
                    .as_ref()
                    .map(|link| {
                        format!(
                            "<span class=\"ThreadShareMarker-show-trashed-message\">{}</span>",
                            text(link)
                        )
                    })
                    .unwrap_or_default();
                let when = when
                    .as_ref()
                    .map(|when| {
                        format!(
                            "<span class=\"ThreadShareMarker-datetime\">{}</span>",
                            text(when)
                        )
                    })
                    .unwrap_or_default();
                let _ = write!(
                    body_html,
                    "<div class=\"ThreadShareMarker\"><span class=\"ThreadShareMarker-body\">{}{link}</span>{when}</div>",
                    text(body)
                );
            }
            Message::Expanded {
                header,
                invite,
                body,
                files,
                draft,
            } => {
                body_html.push_str("<div class=\"MessagePane-expanded\">");
                match header {
                    Header::Compact { participants, when } => {
                        let _ = write!(
                            body_html,
                            "<div class=\"MessagePane-header\"><div class=\"MessagePane-header-from\">{}</div><div class=\"MessagePane-datetime isSent\">{}</div></div>",
                            text(participants),
                            when_html(when)
                        );
                    }
                    Header::Full {
                        rows,
                        when,
                        chevron,
                        tracking_blocked,
                    } => full_header(
                        &mut body_html,
                        rows,
                        when,
                        *chevron,
                        *tracking_blocked,
                        &icons,
                    ),
                }
                body_html.push_str("<div class=\"MessagePane-iframe-wrapper\">");
                if let Some(invite) = invite {
                    invite_card(&mut body_html, invite);
                }
                if let Some(body) = body {
                    if !allow_remote && html_has_remote_resources(body) {
                        allow_remote = true;
                    }
                    // A script the print view injects moves each template into its host's shadow
                    // root (`macos::SHADOW_ROOTS`).
                    let _ = write!(
                        body_html,
                        "<div class=\"Shadowbox\"><template>{body}</template></div>"
                    );
                }
                if !files.is_empty() {
                    body_html.push_str("<div class=\"AttachmentList\">");
                    for file in files {
                        attachment(&mut body_html, file);
                    }
                    body_html.push_str("</div>");
                }
                body_html.push_str("</div>");
                if let Some(draft) = draft
                    && draft_form(&mut body_html, draft, &icons, block_draft_remote_content)
                {
                    allow_remote = true;
                }
                body_html.push_str("</div>");
            }
        }
    }
    body_html.push_str("</div></div>");
    let page = conversation.page;
    let narrow = if page.narrow() {
        " class=\"isNarrow\""
    } else {
        ""
    };
    let csp = if allow_remote {
        CONTENT_SECURITY_POLICY
    } else {
        CONTENT_SECURITY_POLICY_BLOCKED
    };
    let html = format!(
        "<!DOCTYPE html><html{narrow} style=\"font-size: {rem}px; width: {width}px\"><head><meta charset=\"utf-8\">\
         <meta http-equiv=\"Content-Security-Policy\" content=\"{csp}\">\
         <meta http-equiv=\"x-dns-prefetch-control\" content=\"off\">\
         <title>{title}</title><style>{fonts}{PRINT_CSS}</style></head><body>{body_html}</body></html>",
        rem = page.rem(),
        // Rounded to the hundredth, since a float's last digits mean nothing to a layout.
        width = (page.width() * 100.0).round() / 100.0,
        title = text(&conversation.title),
        fonts = font_faces(),
    );
    Rendered { html, allow_remote }
}

/// A draft's form without its buttons: its header (`Draft to …`, or its recipient lists), what's
/// written, and the `…` of its quote. Returns whether the rewritten draft body loads any remote
/// content.
fn draft_form(html: &mut String, draft: &Draft, icons: &Icons, block_remote_content: bool) -> bool {
    html.push_str("<div class=\"ComposeForm\"><div class=\"ComposeForm-header\">");
    match &draft.header {
        DraftHeader::Collapsed(recipients) => {
            let _ = write!(
                html,
                "<div class=\"ComposeForm-collapsed-header\"><span class=\"ComposeForm-collapsed-header-text\"><span class=\"ComposeForm-draft\">Draft </span>to {}</span></div>",
                text(recipients)
            );
        }
        DraftHeader::Lists(lists) => {
            for (label, chips) in lists {
                let _ = write!(
                    html,
                    "<div class=\"RecipientList\"><span class=\"RecipientList-label\">{label}</span>"
                );
                for (index, chip) in chips.iter().enumerate() {
                    let invalid = if chip.invalid { " isInvalid" } else { "" };
                    let last = if index + 1 == chips.len() {
                        " hideSeparator"
                    } else {
                        ""
                    };
                    let cross = if chip.invalid {
                        icons.cross.as_str()
                    } else {
                        ""
                    };
                    let _ = write!(
                        html,
                        "<div class=\"Recipient{invalid}{last}\"><span class=\"ContactName\">{}</span><span class=\"tail\"><span class=\"cross\">{cross}</span><span class=\"separator\">•</span></span></div>",
                        text(&chip.label)
                    );
                }
                html.push_str("<div class=\"RecipientList-input\"></div></div>");
            }
        }
    }
    let body = exo_mail::rewrite_print_html(&draft.body, block_remote_content);
    let has_remote = !block_remote_content && html_has_remote_resources(&body);
    let _ = write!(
        html,
        "</div><div class=\"ComposeForm-body\"><div class=\"ComposeForm-editor\">{body}</div>",
    );
    if draft.quote {
        html.push_str("<div class=\"ComposeForm-quote\"><span></span></div>");
    }
    html.push_str("</div></div>");
    has_remote
}

fn is_remote_scheme(url: &str) -> bool {
    let trimmed = url.trim_start();
    let Some(rest) = exo_mail::strip_ascii_prefix(trimmed, exo_mail::REMOTE_SCHEME_PREFIX) else {
        return false;
    };
    exo_mail::strip_ascii_prefix(rest, "https:").is_some()
        || exo_mail::strip_ascii_prefix(rest, "http:").is_some()
}

fn srcset_has_remote(srcset: &str) -> bool {
    srcset.split(',').any(is_remote_scheme)
}

fn css_has_remote(css: &str) -> bool {
    static CSS_REMOTE: std::sync::LazyLock<regex::Regex> = std::sync::LazyLock::new(|| {
        regex::Regex::new(r#"(?i)(?:url\(\s*['"]?\s*|@import\s*['"]\s*)exo-https?:"#)
            .expect("valid regex")
    });
    CSS_REMOTE.is_match(css)
}

/// Whether `html` (already rewritten for printing) references any `exo-http:` or `exo-https:`
/// resource in an element attribute or CSS declaration. Plain text and `<a href>` links don't
/// count.
fn html_has_remote_resources(html: &str) -> bool {
    if !html
        .as_bytes()
        .windows(8)
        .any(|window| window.eq_ignore_ascii_case(b"exo-http"))
    {
        return false;
    }
    let document = exo_mail::dom::parse(&format!(
        "<!DOCTYPE html><html><body><div>{html}</div></body></html>"
    ));
    let found = exo_mail::dom::body(&document)
        .and_then(|body| exo_mail::dom::element_children(&body).into_iter().next())
        .is_some_and(|root| {
            exo_mail::dom::select(&root, exo_mail::dom::is_element)
                .into_iter()
                .any(|element| {
                    let has_href_resource = exo_mail::dom::is_svg_image(&element)
                        || exo_mail::dom::is(&element, "link");
                    let attrs: &[&str] = if has_href_resource {
                        &["src", "background", "poster", "href", "xlink:href"]
                    } else {
                        &["src", "background", "poster"]
                    };
                    attrs.iter().any(|name| {
                        exo_mail::dom::attr(&element, name)
                            .is_some_and(|value| is_remote_scheme(&value))
                    }) || exo_mail::dom::attr(&element, "srcset")
                        .is_some_and(|value| srcset_has_remote(&value))
                        || exo_mail::dom::attr(&element, "style")
                            .is_some_and(|value| css_has_remote(&value))
                        || (exo_mail::dom::is(&element, "style")
                            && css_has_remote(&exo_mail::dom::text_content(&element)))
                })
        });
    exo_mail::dom::drop_tree(document);
    found
}

/// An attachment card: the file's name, and its type's badge.
fn attachment(html: &mut String, file: &File) {
    let badge = if file.kind.is_empty() {
        String::new()
    } else {
        format!(
            "<div class=\"Attachment-fileType\" style=\"color: {}\"><div class=\"Attachment-fileType-inner\">{}</div></div>",
            css_color(file.color),
            text(&file.kind)
        )
    };
    let _ = write!(
        html,
        "<div class=\"Attachment\"><div class=\"Attachment-container\"><div class=\"Attachment-name\">{}</div><div class=\"Attachment-bottom-container\">{badge}</div></div></div>",
        text(&file.name)
    );
}

/// An invitation's card, with the guest's note above it.
fn invite_card(html: &mut String, invite: &Invite) {
    if let Some(note) = &invite.note {
        html.push_str("<div class=\"MessagePane-calendarNote\">");
        runs(html, note);
        html.push_str("</div>");
    }
    html.push_str("<div class=\"CalendarInvite\"><div class=\"CalendarInvite-image\">");
    if let Some(status) = &invite.status {
        let _ = write!(
            html,
            "<div class=\"CalendarInvite-status-update\">{}</div>",
            text(status)
        );
    }
    html.push_str("</div><div class=\"CalendarInvite-rows\">");
    for row in &invite.rows {
        let question = format!(
            "<span class=\"CalendarInvite-question\">{}</span>",
            text(row.question)
        );
        let kind = match row.question {
            "When" => "when",
            "Where" => "where",
            "Who" => "who",
            "About" => "about",
            "Files" => "files",
            _ => "video-link",
        };
        let _ = match &row.value {
            // The buttons don't print. Their row is a region of its own, as on screen.
            InviteValue::Buttons => write!(
                html,
                "<div class=\"FocusRegion\"><div class=\"CalendarInvite-row\">{question}</div></div>"
            ),
            InviteValue::When(when) => write!(
                html,
                "<div class=\"CalendarInvite-row\">{question}<div class=\"CalendarInvite-property CalendarInvite-property-when\"><div><span class=\"CalendarInvite-eventTime\">{}</span></div></div></div>",
                event_time(when)
            ),
            InviteValue::Text(lines) => {
                let mut value = String::new();
                for (at, (indent, line)) in lines.iter().enumerate() {
                    if at > 0 {
                        value.push('\n');
                    }
                    if *indent > 0.0 {
                        let _ = write!(value, "<span style=\"padding-left: {indent}px\">");
                        runs(&mut value, line);
                        value.push_str("</span>");
                    } else {
                        runs(&mut value, line);
                    }
                }
                write!(
                    html,
                    "<div class=\"CalendarInvite-row\">{question}<div class=\"CalendarInvite-property CalendarInvite-property-{kind}\"><div>{value}</div></div></div>"
                )
            }
            InviteValue::Files(files) => {
                let _ = write!(
                    html,
                    "<div class=\"CalendarInvite-row\">{question}<div class=\"CalendarInvite-property CalendarInvite-property-files\"><div>"
                );
                for file in files {
                    attachment(html, file);
                }
                write!(html, "</div></div></div>")
            }
        };
    }
    html.push_str("</div></div>");
}

/// Runs of text: links, bold, and plain.
fn runs(html: &mut String, runs: &[Run]) {
    for run in runs {
        let content = text(&run.text);
        let _ = match (&run.link, run.bold) {
            (Some(_), _) => write!(html, "<a>{content}</a>"),
            (None, true) => write!(html, "<b>{content}</b>"),
            (None, false) => write!(html, "{content}"),
        };
    }
}

/// When, which breaks only around its `,`s, ` at` and `– `.
fn event_time(when: &str) -> String {
    static SEPARATOR: std::sync::LazyLock<regex::Regex> =
        std::sync::LazyLock::new(|| regex::Regex::new(r"( at|,|– )").expect("valid regex"));
    let mut html = String::new();
    let mut at = 0;
    for found in SEPARATOR.find_iter(when) {
        for piece in [&when[at..found.start()], found.as_str()] {
            if !piece.is_empty() {
                if !html.is_empty() {
                    html.push_str("<wbr>");
                }
                html.push_str(&text(piece));
            }
        }
        at = found.end();
    }
    if at < when.len() {
        if !html.is_empty() {
            html.push_str("<wbr>");
        }
        html.push_str(&text(&when[at..]));
    }
    html
}

/// The icons, as inline SVG markup.
struct Icons {
    paperclip: String,
    calendar: String,
    collapse: String,
    pixel_blocking: String,
    /// An invalid recipient chip's cross.
    cross: String,
}

/// The subject and the label chips.
fn header(html: &mut String, conversation: &Conversation) {
    let scrolled = if conversation.scrolled {
        " isScrolled"
    } else {
        ""
    };
    let _ = write!(
        html,
        "<div class=\"ThreadPane-header-container\"><div class=\"ThreadPane-header{scrolled}\"><span class=\"ThreadPane-subject-label\"><span class=\"ThreadPane-subject\">{}</span>",
        text(&conversation.subject)
    );
    if !conversation.labels.is_empty() {
        html.push_str("<div class=\"ThreadPane-header-labels\">");
        for label in &conversation.labels {
            let _ = write!(html, "<div class=\"ThreadLabel\">{}</div>", text(label));
        }
        html.push_str("</div>");
    }
    html.push_str("</span></div></div>");
}

/// The header that Shift-H shows, with a row for each of From, To, Cc and Bcc, then the date (or
/// status) and the chevron.
fn full_header(
    html: &mut String,
    rows: &[Row],
    when: &When,
    chevron: bool,
    tracking_blocked: bool,
    icons: &Icons,
) {
    html.push_str(
        "<div class=\"MessagePane-header headerExpanded\"><div class=\"MessagePane-emailHeader\">",
    );
    for row in rows {
        let _ = write!(
            html,
            "<div><span class=\"MessagePane-headerLabel\">{}</span>",
            row.label
        );
        for (at, (name, email)) in row.contacts.iter().enumerate() {
            if at > 0 {
                html.push_str(", ");
            }
            match name {
                Some(name) => {
                    let _ = write!(
                        html,
                        "<span class=\"ContactName-fullName\">{}</span><span class=\"ContactName-email\"> &lt;{}&gt;</span>",
                        text(name),
                        text(email)
                    );
                }
                None => {
                    let _ = write!(
                        html,
                        "<span class=\"ContactName-fullName\">{}</span>",
                        text(email)
                    );
                }
            }
        }
        if row.label == "From" && tracking_blocked && !icons.pixel_blocking.is_empty() {
            let _ = write!(
                html,
                "<span class=\"PixelBlocking\">{}</span>",
                icons.pixel_blocking
            );
        }
        html.push_str("</div>");
    }
    let _ = write!(html, "<div>{}", when_html(when));
    if chevron && !icons.collapse.is_empty() {
        let _ = write!(
            html,
            "<span class=\"MessagePane-headerCollapse\"><span class=\"MessagePane-headerCollapse-icon\">{}</span></span>",
            icons.collapse
        );
    }
    html.push_str("</div></div></div>");
}

/// What goes in a date's place.
fn when_html(when: &When) -> String {
    match when {
        When::Date(date) => format!("<time class=\"DateTime\">{}</time>", text(date)),
        When::Status {
            text: status,
            color,
        } => format!(
            "<span class=\"Status\" style=\"color: {}\">{}</span>",
            css_color(*color),
            text(status)
        ),
    }
}

/// The `@font-face` rules for the families the page and its bodies use, each loading its Exo font
/// from the print view's handler.
fn font_faces() -> String {
    let mut css = String::new();
    for &(family, weight, italic, font) in exo_mail::EMAIL_FACES {
        let style = if italic { "italic" } else { "normal" };
        let _ = write!(
            css,
            "@font-face {{ font-family: \"{family}\"; font-weight: {weight}; font-style: {style}; src: url({SCHEME}://font/{}.ttf); }}",
            font.name
        );
    }
    css
}

/// `color` as CSS.
fn css_color(color: Color32) -> String {
    format!("#{:02x}{:02x}{:02x}", color.r(), color.g(), color.b())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::print::{File, Page};

    fn conversation(messages: Vec<Message>) -> Conversation {
        Conversation {
            title: "MSA v3 redlines".into(),
            subject: "MSA <v3> & redlines".into(),
            labels: vec!["Clients/Calloway".into()],
            messages,
            page: Page::LETTER,
            ..Conversation::default()
        }
    }

    #[test]
    fn the_page_is_laid_out_for_its_paper() {
        let rendered = html(&conversation(Vec::new()), false);
        let html = rendered.html;
        assert!(
            html.starts_with(
                "<!DOCTYPE html><html class=\"isNarrow\" style=\"font-size: 10px; width: 777.6px\">"
            ),
            "{html}"
        );
        assert!(html.contains("<title>MSA v3 redlines</title>"));
        assert!(
            html.contains(
                "<span class=\"ThreadPane-subject\">MSA &lt;v3&gt; &amp; redlines</span>"
            ),
            "escaped: {html}"
        );
        assert!(html.contains("<div class=\"ThreadLabel\">Clients/Calloway</div>"));
        assert!(html.contains("font-family: \"Exo Sans Email\"; font-weight: 700; font-style: normal; src: url(exo-print://font/ExoSans-Bold.ttf)"));
        assert!(
            html.contains("default-src 'none'"),
            "nothing loads but what the handler answers"
        );
    }

    #[test]
    fn messages_print_as_the_thread_shows_them() {
        let messages = vec![
            Message::Collapsed {
                from: "Ben".into(),
                snippet: "Maya, Attached …".into(),
                icon: Attachments::Files,
                when: When::Date("NOV 16".into()),
            },
            Message::Marker {
                text: "This message was moved to Trash. ".into(),
                link: Some("Show message.".into()),
                when: None,
            },
            Message::Expanded {
                header: Header::Full {
                    rows: vec![
                        Row {
                            label: "From",
                            contacts: vec![(
                                Some("Daniel Reyes".into()),
                                "daniel@brightmoor.co".into(),
                            )],
                        },
                        Row {
                            label: "To",
                            contacts: vec![
                                (None, "ben@x.law".into()),
                                (Some("Maya".into()), "maya@b.co".into()),
                            ],
                        },
                    ],
                    when: When::Status {
                        text: "Send failed".into(),
                        color: Color32::from_rgb(0xef, 0x8e, 0x8e),
                    },
                    chevron: false,
                    tracking_blocked: false,
                },
                invite: None,
                body: Some("<div class=\"ShadowHTML\">3x</div>".into()),
                files: vec![File {
                    name: "Calloway_MSA_v3_redline".into(),
                    kind: "PDF".into(),
                    color: Color32::from_rgb(0xf7, 0x7b, 0x7b),
                }],
                draft: None,
            },
        ];
        let html = html(&conversation(messages), false).html;
        let clip = exo_ui::icon_svg(PAPERCLIP.0)
            .expect("compiled in")
            .replace("\"#fff\"", "\"#000\"");
        assert!(html.contains(&format!("<div class=\"MessagePane-from\">Ben</div><div class=\"MessagePane-snippet\">Maya, Attached …</div><div class=\"MessagePane-datetime\"><div class=\"MessagePane-attachmentIcon\">{clip}</div><time class=\"DateTime\">NOV 16</time></div>")), "{html}");
        assert!(html.contains("<div class=\"ThreadShareMarker\"><span class=\"ThreadShareMarker-body\">This message was moved to Trash. <span class=\"ThreadShareMarker-show-trashed-message\">Show message.</span></span></div>"), "{html}");
        assert!(html.contains("<span class=\"ContactName-fullName\">Daniel Reyes</span><span class=\"ContactName-email\"> &lt;daniel@brightmoor.co&gt;</span>"), "{html}");
        assert!(html.contains("<span class=\"ContactName-fullName\">ben@x.law</span>, <span class=\"ContactName-fullName\">Maya</span>"), "{html}");
        assert!(
            html.contains(
                "<span class=\"Status\" style=\"color: #ef8e8e\">Send failed</span></div>"
            ),
            "no chevron on a failed send: {html}"
        );
        assert!(html.contains(
            "<div class=\"Shadowbox\"><template><div class=\"ShadowHTML\">3x</div></template></div>"
        ));
        assert!(html.contains("<div class=\"Attachment-fileType\" style=\"color: #f77b7b\"><div class=\"Attachment-fileType-inner\">PDF</div></div>"));
    }

    #[test]
    fn a_draft_prints_its_form_without_its_buttons() {
        use crate::print::Chip;
        let form = |header| {
            let mut html = String::new();
            let icons = Icons {
                paperclip: String::new(),
                calendar: String::new(),
                collapse: String::new(),
                pixel_blocking: String::new(),
                cross: "<svg/>".into(),
            };
            let draft = Draft {
                header,
                body: "<div>Thanks</div>".into(),
                quote: true,
            };
            draft_form(&mut html, &draft, &icons, false);
            html
        };
        assert_eq!(
            form(DraftHeader::Collapsed("Tom & <Priya>".into())),
            "<div class=\"ComposeForm\"><div class=\"ComposeForm-header\">\
             <div class=\"ComposeForm-collapsed-header\"><span class=\"ComposeForm-collapsed-header-text\"><span class=\"ComposeForm-draft\">Draft </span>to Tom &amp; &lt;Priya&gt;</span></div>\
             </div><div class=\"ComposeForm-body\"><div class=\"ComposeForm-editor\"><div>Thanks</div></div>\
             <div class=\"ComposeForm-quote\"><span></span></div></div></div>"
        );
        let chip = |label: &str, invalid| Chip {
            label: label.into(),
            invalid,
        };
        let lists = form(DraftHeader::Lists(vec![(
            "To",
            vec![chip("Tom Becker", false), chip("dana", true)],
        )]));
        assert!(lists.contains(
            "<div class=\"RecipientList\"><span class=\"RecipientList-label\">To</span>\
             <div class=\"Recipient\"><span class=\"ContactName\">Tom Becker</span><span class=\"tail\"><span class=\"cross\"></span><span class=\"separator\">•</span></span></div>\
             <div class=\"Recipient isInvalid hideSeparator\"><span class=\"ContactName\">dana</span><span class=\"tail\"><span class=\"cross\"><svg/></span><span class=\"separator\">•</span></span></div>\
             <div class=\"RecipientList-input\"></div></div>"
        ), "{lists}");
    }

    #[test]
    fn an_invitation_prints_its_card_without_its_buttons() {
        use crate::print::{InviteRow, InviteValue, Run};
        let link = |text: &str| Run {
            text: text.into(),
            link: Some("https://x.io".into()),
            bold: false,
        };
        let card = Invite {
            note: Some(vec![
                Run {
                    text: "Running late: ".into(),
                    ..Run::default()
                },
                link("x.io"),
            ]),
            status: Some("Grace has accepted".into()),
            rows: vec![
                InviteRow {
                    question: "RSVP",
                    value: InviteValue::Buttons,
                },
                InviteRow {
                    question: "When",
                    value: InviteValue::When(
                        "Wednesday, November 25th, 2026 at 9:00 AM – 9:45 AM EST".into(),
                    ),
                },
                InviteRow {
                    question: "Meet",
                    value: InviteValue::Text(vec![(0.0, vec![link("meet.google.com/a")])]),
                },
            ],
        };
        let mut html = String::new();
        invite_card(&mut html, &card);
        assert_eq!(
            html,
            "<div class=\"MessagePane-calendarNote\">Running late: <a>x.io</a></div>\
             <div class=\"CalendarInvite\"><div class=\"CalendarInvite-image\"><div class=\"CalendarInvite-status-update\">Grace has accepted</div></div>\
             <div class=\"CalendarInvite-rows\">\
             <div class=\"FocusRegion\"><div class=\"CalendarInvite-row\"><span class=\"CalendarInvite-question\">RSVP</span></div></div>\
             <div class=\"CalendarInvite-row\"><span class=\"CalendarInvite-question\">When</span><div class=\"CalendarInvite-property CalendarInvite-property-when\"><div><span class=\"CalendarInvite-eventTime\">Wednesday<wbr>,<wbr> November 25th<wbr>,<wbr> 2026<wbr> at<wbr> 9:00 AM <wbr>– <wbr>9:45 AM EST</span></div></div></div>\
             <div class=\"CalendarInvite-row\"><span class=\"CalendarInvite-question\">Meet</span><div class=\"CalendarInvite-property CalendarInvite-property-video-link\"><div><a>meet.google.com/a</a></div></div></div>\
             </div></div>"
        );
    }

    #[test]
    fn open_draft_rewrites_remote_signature_images_for_print_csp() {
        let icons = Icons {
            paperclip: String::new(),
            calendar: String::new(),
            collapse: String::new(),
            pixel_blocking: String::new(),
            cross: String::new(),
        };
        let draft = Draft {
            header: DraftHeader::Collapsed("Tom".into()),
            body: concat!(
                r#"<div>Best,<br><img src="https://cdn.example.com/sig.png" srcset="HTTP://cdn.example.com/sig@1x.png 1x, //cdn.example.com/sig@2x.png 2x" loading="lazy"></div>"#,
                r#"<div style="background: url('https://cdn.example.com/bg.png')"><a href="https://example.com">https://example.com</a></div>"#,
                r#"<img src="cid:inline-logo"><img src="data:image/png;base64,iVBORw0K">"#,
            )
            .into(),
            quote: false,
        };
        let mut html = String::new();
        assert!(draft_form(&mut html, &draft, &icons, false));
        assert!(
            html.contains(r#"src="exo-https://cdn.example.com/sig.png""#),
            "{html}"
        );
        assert!(
            html.contains(
                r#"srcset="exo-http://cdn.example.com/sig@1x.png 1x, exo-https://cdn.example.com/sig@2x.png 2x""#
            ),
            "{html}"
        );
        assert!(!html.contains("loading="), "lazy loading stripped: {html}");
        assert!(
            html.contains("url('exo-https://cdn.example.com/bg.png')"),
            "{html}"
        );
        assert!(
            html.contains(r#"<a href="https://example.com">https://example.com</a>"#),
            "links and body text keep their scheme: {html}"
        );
        assert!(
            html.contains(r#"src="cid:inline-logo""#)
                && html.contains(r#"src="data:image/png;base64,iVBORw0K""#),
            "cid: and data: stay untouched: {html}"
        );
    }

    #[test]
    fn draft_with_angle_bracket_alt_and_preexisting_exo_scheme_honors_blocking() {
        let icons = Icons {
            paperclip: String::new(),
            calendar: String::new(),
            collapse: String::new(),
            pixel_blocking: String::new(),
            cross: String::new(),
        };
        let draft = Draft {
            header: DraftHeader::Collapsed("Tom".into()),
            body: concat!(
                r#"<img alt="a > b" src="https://cdn.example.com/sig.png">"#,
                r#"<img src="exo-https://cdn.example.com/pre.png">"#,
            )
            .into(),
            quote: false,
        };

        let mut allowed = String::new();
        assert!(draft_form(&mut allowed, &draft, &icons, false));
        assert!(
            allowed.contains(r#"src="exo-https://cdn.example.com/sig.png""#),
            "DOM rewriter handles '>' inside alt attribute: {allowed}"
        );
        assert!(
            allowed.contains(r#"src="exo-https://cdn.example.com/pre.png""#)
                && !allowed.contains("exo-exo-"),
            "pre-existing exo-https is preserved without double-prefixing: {allowed}"
        );

        let mut blocked = String::new();
        assert!(!draft_form(&mut blocked, &draft, &icons, true));
        assert!(
            blocked.contains(r#"src="exo-blocked://cdn.example.com/sig.png""#),
            "remote image blocked: {blocked}"
        );
        assert!(
            blocked.contains(r#"src="exo-blocked://cdn.example.com/pre.png""#),
            "pre-existing exo-https blocked: {blocked}"
        );
        assert!(blocked.contains("data-exo-blocked"), "{blocked}");
        assert!(
            !blocked.to_ascii_lowercase().contains("exo-http"),
            "no exo-http(s) survives blocking: {blocked}"
        );
    }

    #[test]
    fn print_csp_omits_remote_schemes_when_all_content_is_blocked() {
        let expanded = |body: Option<&str>, draft: Option<&str>| Message::Expanded {
            header: Header::Compact {
                participants: "Maya".into(),
                when: When::Date("NOV 16".into()),
            },
            invite: None,
            body: body.map(str::to_owned),
            files: Vec::new(),
            draft: draft.map(|body| Draft {
                header: DraftHeader::Collapsed("Tom".into()),
                body: body.into(),
                quote: false,
            }),
        };

        // Both message body (blocked + deceptive plain text) and blocked draft -> no remote schemes in CSP.
        let blocked_doc = html(
            &conversation(vec![expanded(
                Some(
                    r#"<img src="exo-blocked://x.io/a.png" data-exo-blocked=""><p>exo-https://plain.text</p><a href="exo-https://link.text">link</a>"#,
                ),
                Some(
                    r#"<img alt="a > b" src="https://cdn.example.com/sig.png"><img src="exo-https://cdn.example.com/pre.png">"#,
                ),
            )]),
            true,
        );
        assert!(!blocked_doc.allow_remote);
        assert!(
            blocked_doc.html.contains(CONTENT_SECURITY_POLICY_BLOCKED),
            "{}",
            blocked_doc.html
        );
        assert!(
            !blocked_doc.html.contains("img-src exo-https: exo-http:"),
            "{}",
            blocked_doc.html
        );

        // Allowed draft with remote image -> CSP permits exo-https:/exo-http:.
        let allowed_draft_doc = html(
            &conversation(vec![expanded(
                Some(r#"<img src="exo-blocked://x.io/a.png" data-exo-blocked="">"#),
                Some(r#"<img alt="a > b" src="https://cdn.example.com/sig.png">"#),
            )]),
            false,
        );
        assert!(allowed_draft_doc.allow_remote);
        assert!(
            allowed_draft_doc.html.contains(CONTENT_SECURITY_POLICY),
            "{}",
            allowed_draft_doc.html
        );

        // Blocked draft, but a message body explicitly revealed its images -> CSP permits remote.
        let revealed_message_doc = html(
            &conversation(vec![expanded(
                Some(r#"<img src="exo-https://x.io/revealed.png">"#),
                Some(r#"<img src="https://cdn.example.com/sig.png">"#),
            )]),
            true,
        );
        assert!(revealed_message_doc.allow_remote);
        assert!(
            revealed_message_doc.html.contains(CONTENT_SECURITY_POLICY),
            "{}",
            revealed_message_doc.html
        );
        assert!(
            revealed_message_doc
                .html
                .contains(r#"src="exo-blocked://cdn.example.com/sig.png""#),
            "draft itself still blocks its own remote images: {}",
            revealed_message_doc.html
        );
    }
}

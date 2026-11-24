//! Print and Save as PDF (⌘P, `Print`, `Save as PDF`).
//!
//! A conversation prints under `@media print` rules: everything but the open conversation hidden,
//! no card backgrounds or shadows, a hairline under each body. What prints is what the thread view
//! shows: collapsed messages as their one-line rows, expanded ones whole, a quoted part only when
//! its `…` is open, a header as Shift-H left it. Carbon's rules are screen-only, so a Carbon
//! account prints exactly as a Snow one.
//!
//! Exo writes the page as HTML ([`document`]) and hands it to WebKit, which lays it out, paginates
//! it and prints it the way Chromium does ([`macos`]). It prints through the macOS print panel,
//! whose PDF button saves it as a PDF, or, for `Save as PDF`, straight to the file chosen in a
//! save panel.
//! Bodies are the same markup Exo renders on screen ([`exo_mail::printable_body`]), and what they
//! load goes through the same guarded fetches (`crate::remote_content`) and the same Images
//! setting.

mod document;
#[cfg(target_os = "macos")]
mod macos;

use std::{collections::HashMap, path::PathBuf};

use egui::Color32;
use exo_store::{Attachment, Attachments};

use crate::account::Loader;
#[cfg(doc)]
use crate::thread::ThreadView;

/// A conversation as it prints.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Conversation {
    /// The print job's title ([`title`]), which `Save as PDF` also names the file after.
    pub title: String,
    pub subject: String,
    /// The header's label chips, as they're named on screen.
    pub labels: Vec<String>,
    /// The messages were scrolled under the header, which then prints its border
    /// (`.ThreadPane-header.isScrolled`).
    pub scrolled: bool,
    pub messages: Vec<Message>,
    /// The thread's inline attachments, which bodies show by `cid:`.
    pub inline: Vec<Attachment>,
    /// The paper it's laid out for.
    pub page: Page,
}

/// A sheet of paper, in points, inside `@page { margin: .6in .2in }` (portrait, `size: portrait`).
/// Chromium lays a printed page out 96 CSS px to the inch, and the thread view's widths follow the
/// page's width as they follow a window's.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Page {
    /// Its width and height in points.
    pub paper: [f32; 2],
}

impl Default for Page {
    fn default() -> Self {
        Self::LETTER
    }
}

impl Page {
    /// US Letter, which `Save as PDF` prints on.
    pub const LETTER: Self = Self {
        paper: [612.0, 792.0],
    };
    /// `@page { margin: .6in .2in }`: top and bottom, left and right.
    pub const MARGINS: [f32; 2] = [0.6 * 72.0, 0.2 * 72.0];

    /// The width it's laid out at, in CSS px.
    pub fn width(self) -> f32 {
        (self.paper[0] - 2.0 * Self::MARGINS[1]) * 96.0 / 72.0
    }

    /// The root font size the window's media queries give this width (`html { font-size }`).
    pub fn rem(self) -> f32 {
        exo_ui::rem(self.width(), false)
    }

    /// The `max-width: 1024px` rules apply.
    pub fn narrow(self) -> bool {
        self.width() <= 1024.0
    }

    /// A message's content width, which is `.ThreadPane-messages`
    /// (`max-width: calc(640px + 8rem)`) less the margins of `.ThreadPane-table` (`0 2rem`, 4px
    /// more on the right when narrow) and the card's `2rem` padding.
    pub fn message_width(self) -> f32 {
        let rem = self.rem();
        let narrow = if self.narrow() { 4.0 } else { 0.0 };
        self.width().min(640.0 + 8.0 * rem) - 8.0 * rem - narrow
    }
}

/// One message, as the thread view shows it.
#[derive(Clone, Debug, PartialEq)]
pub enum Message {
    /// A collapsed row (`.MessagePane-collapsed`).
    Collapsed {
        from: String,
        snippet: String,
        /// The paperclip for its files, or the calendar for its invite, before the date.
        icon: Attachments,
        when: When,
    },
    /// An expanded message (`.MessagePane-expanded`).
    Expanded {
        header: Header,
        /// Its invitation's card, above the body.
        invite: Option<Invite>,
        /// The body's shadow-root markup ([`exo_mail::printable_body`]), or `None` while it's
        /// still being rendered or the card stands in for it, which prints as nothing.
        body: Option<String>,
        files: Vec<File>,
        /// The reply or forward open under it, as its form prints without its buttons.
        draft: Option<Draft>,
    },
    /// A marker between messages (`.ThreadShareMarker`): a message in Trash folded into
    /// `This message was moved to Trash.` and its link, or `Reminder returned` and when.
    Marker {
        text: String,
        link: Option<String>,
        when: Option<String>,
    },
}

/// A draft open in the conversation, as its form prints: its header, then what's been written,
/// then the `…` of its quote or signature.
#[derive(Clone, Debug, PartialEq)]
pub struct Draft {
    pub header: DraftHeader,
    /// The editor's HTML.
    pub body: String,
    /// A quote or signature sits behind a `…`.
    pub quote: bool,
}

/// A draft form's header.
#[derive(Clone, Debug, PartialEq)]
pub enum DraftHeader {
    /// `Draft to {recipients}`.
    Collapsed(String),
    /// The recipient lists that are showing (To, then Cc and Bcc), each its label and chips.
    Lists(Vec<(&'static str, Vec<Chip>)>),
}

/// A recipient's chip.
#[derive(Clone, Debug, PartialEq)]
pub struct Chip {
    pub label: String,
    /// The address can't be sent to, which outlines the chip in red.
    pub invalid: bool,
}

/// An invitation's card as it prints: neither its background nor its photo prints, nor do its
/// buttons, so the RSVP row prints its name alone.
#[derive(Clone, Debug, PartialEq)]
pub struct Invite {
    /// The guest's note above the card.
    pub note: Option<Vec<Run>>,
    /// The status over the photo.
    pub status: Option<String>,
    pub rows: Vec<InviteRow>,
}

/// One of the card's rows: its name, then its value.
#[derive(Clone, Debug, PartialEq)]
pub struct InviteRow {
    pub question: &'static str,
    pub value: InviteValue,
}

/// A card row's value.
#[derive(Clone, Debug, PartialEq)]
pub enum InviteValue {
    /// Lines of runs, each indented by its first number (Where, the call, Who, About as far as
    /// it's open).
    Text(Vec<(f32, Vec<Run>)>),
    /// When, which breaks only after a `,`, ` at` or `– `.
    When(String),
    /// The RSVP buttons, or Add To Calendar.
    Buttons,
    /// Files, as attachment cards.
    Files(Vec<File>),
}

/// A run of text, a link or bold.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Run {
    pub text: String,
    pub link: Option<String>,
    pub bold: bool,
}

/// What a header shows in the date's place.
#[derive(Clone, Debug, PartialEq)]
pub enum When {
    /// The date.
    Date(String),
    /// A failed, scheduled or waiting send's status, bold in its color.
    Status { text: String, color: Color32 },
}

/// An expanded message's header.
#[derive(Clone, Debug, PartialEq)]
pub enum Header {
    /// The participants and the date.
    Compact { participants: String, when: When },
    /// The header that Shift-H shows, with From, To, Cc and Bcc in full, then the full date and
    /// the chevron that folds it (no chevron on a failed send).
    Full {
        rows: Vec<Row>,
        when: When,
        chevron: bool,
        tracking_blocked: bool,
    },
}

/// One of the full header's rows.
#[derive(Clone, Debug, PartialEq)]
pub struct Row {
    pub label: &'static str,
    /// Each contact's name (when it has one) and address.
    pub contacts: Vec<(Option<String>, String)>,
}

/// An attachment card.
#[derive(Clone, Debug, PartialEq)]
pub struct File {
    /// The file's name without its extension.
    pub name: String,
    /// Its extension, upper-cased (the type badge), if it has one.
    pub kind: String,
    /// The badge's color.
    pub color: Color32,
}

impl File {
    /// The card for a file named `filename` of type `mime_type`.
    pub(crate) fn of(filename: &str, mime_type: &str) -> Self {
        let (name, kind) = filename
            .rsplit_once('.')
            .map_or((filename, ""), |(stem, extension)| (stem, extension));
        Self {
            name: name.to_owned(),
            kind: kind.to_uppercase(),
            color: exo_ui::attachment_badge_color(filename, mime_type),
        }
    }
}

/// Hears whether a print job went out, meaning the panel closed on Print or the PDF was written.
pub type Done = Box<dyn FnOnce(bool)>;

/// How a print job goes out.
#[derive(Clone, Debug, PartialEq)]
pub enum Destination {
    /// The macOS print panel, its PDF button included (`Print`, ⌘P).
    Panel,
    /// A PDF file at this path, on US Letter (`Save as PDF`, after its save panel, and tests).
    Pdf(PathBuf),
}

/// A conversation ready to print on any paper ([`ThreadView::printout`]). What it prints is
/// settled; a page's width only cuts its collapsed rows' snippets again, and sets its widths.
pub struct Printout {
    /// The print job's title ([`title`]).
    pub title: String,
    lay_out: Box<dyn Fn(Page) -> Conversation>,
    /// Optional explicit override for whether open drafts block remote content (when `None`, the
    /// print job reads the account's `RemoteContentBlocking` setting before rendering).
    block_draft_remote_content: Option<bool>,
}

impl Printout {
    pub(crate) fn new(title: String, lay_out: impl Fn(Page) -> Conversation + 'static) -> Self {
        Self {
            title,
            lay_out: Box::new(lay_out),
            block_draft_remote_content: None,
        }
    }

    /// Sets whether open drafts in this printout block remote content (for example, `true` when
    /// `RemoteContentBlocking::All` is set and the user hasn't revealed draft images).
    #[must_use]
    pub fn block_draft_remote_content(mut self, block: bool) -> Self {
        self.block_draft_remote_content = Some(block);
        self
    }

    #[cfg(any(target_os = "macos", test))]
    pub(super) fn blocks_draft_remote_content(&self) -> Option<bool> {
        self.block_draft_remote_content
    }

    /// The conversation laid out for `page`.
    pub fn on(&self, page: Page) -> Conversation {
        (self.lay_out)(page)
    }
}

/// Prints the open conversation through the macOS print panel over Exo's window (⌘P,
/// File ▸ Print, ⌘K `Print`), laying it out again for the paper the panel is set to. `loader`
/// fetches what its bodies load. Returns at once, and the panel runs on its own.
pub fn conversation(printout: Printout, loader: &Loader) {
    print(printout, loader, Destination::Panel, None);
}

/// `Save as PDF` (`Print to PDF`, `Save to PDF`, `Export to PDF`). It opens a save panel titled
/// `Save as PDF` proposing `{title}.pdf`, then prints the conversation into the chosen file on US
/// Letter.
pub fn save_as_pdf(printout: Printout, loader: &Loader) {
    #[cfg(target_os = "macos")]
    {
        let loader = loader.clone();
        macos::choose_pdf(&file_name(&printout.title), move |path| {
            macos::print(printout, loader, Destination::Pdf(path), None)
        });
    }
    #[cfg(not(target_os = "macos"))]
    {
        let _ = (printout, loader);
        log::warn!("Save as PDF is macOS-only");
    }
}

/// Prints `printout` to `destination`, then calls `done` with whether it went (the panel closed
/// on Print, or the file was written). Returns at once.
pub fn print(printout: Printout, loader: &Loader, destination: Destination, done: Option<Done>) {
    #[cfg(target_os = "macos")]
    macos::print(printout, loader.clone(), destination, done);
    #[cfg(not(target_os = "macos"))]
    {
        let _ = (printout, loader, destination);
        log::warn!("printing is macOS-only");
        if let Some(done) = done {
            done(false);
        }
    }
}

/// What the print view loads for a URL its page asks for.
#[derive(Debug, PartialEq)]
enum Resource<'a> {
    /// One of Exo's fonts (`exo-print://font/<name>.ttf`).
    Font(&'static [u8]),
    /// A body's remote content, at this `http(s)` URL (`exo-https:`, `exo-http:`), which the
    /// guarded fetches load (`crate::remote_content`).
    Remote(String),
    /// A body's inline attachment (`cid:`).
    Inline(&'a Attachment),
}

/// What `url` loads, from `inline` (the thread's inline attachments by `Content-ID`). `None` for
/// anything else, which fails, as does a relative URL that a body's CSS names or any remote URL
/// when `allow_remote` is `false`.
fn resource<'a>(
    url: &str,
    inline: &'a HashMap<String, Attachment>,
    allow_remote: bool,
) -> Option<Resource<'a>> {
    if let Some(name) = url.strip_prefix(&format!("{}://font/", document::SCHEME)) {
        let name = name.strip_suffix(".ttf")?;
        return exo_mail::EMAIL_FACES
            .iter()
            .find(|(_, _, _, font)| font.name == name)
            .map(|(_, _, _, font)| Resource::Font(font.data));
    }
    if let Some(cid) = url.strip_prefix("cid:") {
        return crate::body::fetch::named(inline, cid).map(Resource::Inline);
    }
    if !allow_remote {
        return None;
    }
    let remote = url.strip_prefix(exo_mail::REMOTE_SCHEME_PREFIX)?;
    let parsed = url::Url::parse(remote).ok()?;
    let relative = url::Url::parse(document::BASE_URL).ok()?.host_str() == parsed.host_str();
    (matches!(parsed.scheme(), "http" | "https") && !relative)
        .then(|| Resource::Remote(parsed.into()))
}

/// The type of remote content, from its first bytes. WebKit decodes an image or a font whatever
/// it's called, but takes a stylesheet only as `text/css`, which is what everything else gets.
fn sniff(bytes: &[u8]) -> &'static str {
    let start = String::from_utf8_lossy(&bytes[..bytes.len().min(256)])
        .trim_start()
        .to_ascii_lowercase();
    match bytes {
        [0x89, b'P', b'N', b'G', ..] => "image/png",
        [0xff, 0xd8, 0xff, ..] => "image/jpeg",
        [b'G', b'I', b'F', b'8', ..] => "image/gif",
        [
            b'R',
            b'I',
            b'F',
            b'F',
            _,
            _,
            _,
            _,
            b'W',
            b'E',
            b'B',
            b'P',
            ..,
        ] => "image/webp",
        [b'w', b'O', b'F', b'2', ..] => "font/woff2",
        [b'w', b'O', b'F', b'F', ..] => "font/woff",
        [0, 1, 0, 0, ..] | [b'O', b'T', b'T', b'O', ..] | [b't', b'r', b'u', b'e', ..] => {
            "font/ttf"
        }
        _ if start.starts_with("<svg") || start.starts_with("<?xml") => "image/svg+xml",
        _ => "text/css",
    }
}

/// The title a print job takes from the tab, `{subject} • {who}` (or just `{who}` without a
/// subject), with a leading emoji and Exo's own name taken off. `who` is the account's address
/// with several accounts signed in, else Exo.
pub fn title(subject: &str, accounts: usize, email: &str) -> String {
    let who = if accounts > 1 { email } else { "Exo" };
    let title = if subject.is_empty() {
        who.to_owned()
    } else {
        format!("{subject} • {who}")
    };
    let title = match title.split_once(' ') {
        Some((first, rest)) if is_leading_emoji(first) => rest.to_owned(),
        _ => title,
    };
    // The first one only.
    title.replacen(" • Exo", "", 1)
}

/// Whether `word` is one of the characters a title loses before a space: a private-use character
/// (U+E000–U+F8FF) or an emoji in U+1F000–U+1F7FF or U+1F910–U+1F9FF.
fn is_leading_emoji(word: &str) -> bool {
    let mut chars = word.chars();
    let (Some(c), None) = (chars.next(), chars.next()) else {
        return false;
    };
    matches!(u32::from(c), 0xE000..=0xF8FF | 0x1F000..=0x1F7FF | 0x1F910..=0x1F9FF)
}

/// The PDF's file name, `{title}.pdf`, in which `/ \ ? % * : | " < >` become `_`, control
/// characters go, and a Windows reserved name (or only dots) gets `Exo – ` in front.
fn file_name(title: &str) -> String {
    let mut name: String = title
        .chars()
        .filter(|c| !matches!(u32::from(*c), 0x00..=0x1f | 0x80..=0x9f))
        .map(|c| if "/\\?%*:|\"<>".contains(c) { '_' } else { c })
        .collect();
    let stem = name
        .split('.')
        .next()
        .unwrap_or_default()
        .to_ascii_lowercase();
    let reserved = matches!(stem.as_str(), "con" | "prn" | "aux" | "nul")
        || (stem.len() == 4
            && (stem.starts_with("com") || stem.starts_with("lpt"))
            && stem.as_bytes()[3].is_ascii_digit());
    if reserved || (!name.is_empty() && name.chars().all(|c| c == '.')) {
        name = format!("Exo – {name}");
    }
    format!("{name}.pdf")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_job_is_titled_as_the_shell_titles_it() {
        assert_eq!(
            title("MSA v3 redlines", 1, "maya@brightmoor.co"),
            "MSA v3 redlines"
        );
        assert_eq!(
            title("MSA v3 redlines", 2, "maya@brightmoor.co"),
            "MSA v3 redlines • maya@brightmoor.co"
        );
        assert_eq!(
            title("", 1, "maya@brightmoor.co"),
            "Exo",
            "no subject: just who"
        );
        assert_eq!(
            title("🦃 Thanksgiving", 1, "maya@brightmoor.co"),
            "Thanksgiving"
        );
        assert_eq!(
            title("Thanksgiving 🦃", 1, "maya@brightmoor.co"),
            "Thanksgiving 🦃"
        );
        assert_eq!(
            title("é is not an emoji", 1, "maya@brightmoor.co"),
            "é is not an emoji"
        );
    }

    #[test]
    fn the_page_loads_only_what_bodies_may() {
        let attachment = Attachment {
            message_id: "m1".into(),
            attachment_id: None,
            part_id: "1.2".into(),
            filename: "image001.png".into(),
            mime_type: "image/png".into(),
            size: 3,
            content_id: Some("image001.png@01D9".into()),
            inline: true,
        };
        let inline = crate::body::fetch::by_content_id(vec![attachment.clone()]);
        let bold = exo_mail::EMAIL_FACES
            .iter()
            .find(|(family, weight, italic, _)| {
                *family == "Exo Sans Email" && *weight == 700 && !italic
            })
            .map(|(_, _, _, font)| font.data)
            .expect("a bold face");
        assert_eq!(
            resource("exo-print://font/ExoSans-Bold.ttf", &inline, true),
            Some(Resource::Font(bold))
        );
        assert_eq!(
            resource("exo-print://font/Missing.ttf", &inline, true),
            None
        );
        assert_eq!(
            resource("cid:image001.png%4001d9", &inline, true),
            Some(Resource::Inline(&attachment))
        );
        assert_eq!(resource("cid:other", &inline, true), None);
        assert_eq!(
            resource("exo-https://x.io/a.png?b=1", &inline, true),
            Some(Resource::Remote("https://x.io/a.png?b=1".into()))
        );
        assert_eq!(
            resource("exo-http://x.io/a.css", &inline, true),
            Some(Resource::Remote("http://x.io/a.css".into()))
        );
        assert_eq!(
            resource("exo-https://exo.invalid/a.png", &inline, true),
            None,
            "a relative URL"
        );
        assert_eq!(resource("exo-blocked://x.io/a.png", &inline, true), None);
        assert_eq!(resource("exo-file:///etc/passwd", &inline, true), None);
        assert_eq!(
            resource("https://x.io/a.png", &inline, true),
            None,
            "never straight to the network"
        );
        // When the active print job blocks all remote content, fonts and cid: still load, while
        // exo-http(s) remote resources are refused at the handler as defense-in-depth.
        assert_eq!(
            resource("exo-print://font/ExoSans-Bold.ttf", &inline, false),
            Some(Resource::Font(bold))
        );
        assert_eq!(
            resource("cid:image001.png%4001d9", &inline, false),
            Some(Resource::Inline(&attachment))
        );
        assert_eq!(resource("exo-https://x.io/a.png?b=1", &inline, false), None);
        assert_eq!(resource("exo-http://x.io/a.css", &inline, false), None);
    }

    #[test]
    fn remote_content_is_typed_from_its_bytes() {
        assert_eq!(sniff(b"\x89PNG\r\n"), "image/png");
        assert_eq!(sniff(b"GIF89a"), "image/gif");
        assert_eq!(sniff(b"  <svg xmlns="), "image/svg+xml");
        assert_eq!(sniff(b"wOF2"), "font/woff2");
        assert_eq!(sniff(b"p { color: red }"), "text/css");
    }

    #[test]
    fn file_names_are_safe_everywhere() {
        assert_eq!(file_name("Q4: plan / draft?"), "Q4_ plan _ draft_.pdf");
        assert_eq!(file_name("a\u{7}b\u{85}c"), "abc.pdf");
        assert_eq!(file_name("con"), "Exo – con.pdf");
        assert_eq!(file_name("LPT1.txt"), "Exo – LPT1.txt.pdf");
        assert_eq!(file_name("..."), "Exo – ....pdf");
        assert_eq!(file_name("console"), "console.pdf");
    }
}

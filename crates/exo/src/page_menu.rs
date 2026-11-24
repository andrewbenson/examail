//! The shell's right-click menu as a native one ([`crate::desktop::context_menu`]): a link's, an
//! image's, an editable field's (its spelling first) or a selection's. Compose's body has its own
//! ([`crate::compose`]); a message body's and the rest of the page's are here. On the rest of the
//! page (whatever has no menu of its own) it's a link's, or the selection's with `Copy`, which is
//! off with nothing selected.

use crate::{
    Mail,
    desktop::context_menu::{self, MenuItem, MenuKey},
};

/// What a right-click landed on, which the menu is made of.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct Target {
    /// The link's `href`.
    pub link: Option<String>,
    /// The `src` of an image with its pixels.
    pub image: Option<String>,
    /// An editable field, with its spelling.
    pub editable: Option<Editable>,
    /// The selected text.
    pub selection: String,
}

/// An editable field's part of [`Target`].
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct Editable {
    /// The misspelled word clicked.
    pub misspelled: Option<String>,
    /// Its corrections.
    pub suggestions: Vec<String>,
}

/// What an item does.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Choice {
    /// `Copy Link`, `Copy Email Address` and `Copy Image URL`: this text to the clipboard.
    CopyText(String),
    /// `Copy Image`.
    CopyImage,
    /// `Open Link`: a web link in the browser, a `mailto:` in compose.
    OpenLink(String),
    /// A suggestion, which replaces the misspelled word.
    Correct(String),
    /// `Add to Dictionary`.
    AddToDictionary(String),
    /// `Ignore Spelling`.
    IgnoreSpelling(String),
    /// `Look Up "…"`.
    LookUp,
    /// `Search with Google`: this URL.
    Search(String),
    /// `Cut`.
    Cut,
    /// `Copy`.
    Copy,
    /// `Paste`.
    Paste,
}

/// One item of the menu, and what it does. A separator does nothing.
pub(crate) type Entry = (MenuItem, Option<Choice>);

/// The menu for `target`, in this order: a link's items (and its image's); or an image's; or an
/// editable field's up to three suggestions, `Add to Dictionary` and `Ignore Spelling` for a
/// misspelled word, the selection's items, `Cut`, `Copy`, `Paste`; or the selection's items and
/// `Copy`.
pub(crate) fn entries(target: &Target) -> Vec<Entry> {
    let mut out = Vec::new();
    let item = |title: &str, choice: Choice| {
        (
            MenuItem::Item {
                title: title.to_owned(),
                key: None,
                symbol: None,
                enabled: true,
            },
            Some(choice),
        )
    };
    let keyed = |title: &str, key: char, enabled: bool, choice: Choice| {
        (
            MenuItem::Item {
                title: title.to_owned(),
                key: Some(MenuKey {
                    key,
                    shift: false,
                    command: true,
                }),
                symbol: None,
                enabled,
            },
            Some(choice),
        )
    };
    let selected = !target.selection.is_empty();
    if let Some(link) = &target.link {
        match strip_mailto(link) {
            // The address alone: no `mailto:` and no query, decoded.
            Some(address) => out.push(item(
                "Copy Email Address",
                Choice::CopyText(decode(address.split('?').next().unwrap_or_default())),
            )),
            None => out.push(item("Copy Link", Choice::CopyText(link.clone()))),
        }
        if is_trusted(link) {
            out.push(item("Open Link", Choice::OpenLink(link.clone())));
        }
        if let Some(src) = &target.image {
            out.push((MenuItem::Separator, None));
            out.extend(image_items(src));
        }
    } else if let Some(src) = &target.image {
        out.extend(image_items(src));
    } else if let Some(editable) = &target.editable {
        for suggestion in editable.suggestions.iter().take(3) {
            out.push(item(suggestion, Choice::Correct(suggestion.clone())));
        }
        if !editable.suggestions.is_empty() {
            out.push((MenuItem::Separator, None));
        }
        if let Some(word) = &editable.misspelled {
            out.push(item(
                "Add to Dictionary",
                Choice::AddToDictionary(word.clone()),
            ));
            // As macOS's own text menus have it.
            out.push(item(
                "Ignore Spelling",
                Choice::IgnoreSpelling(word.clone()),
            ));
        }
        out.extend(search_items(&target.selection));
        out.push(keyed("Cut", 'x', selected, Choice::Cut));
        out.push(keyed("Copy", 'c', selected, Choice::Copy));
        out.push(keyed("Paste", 'v', true, Choice::Paste));
    } else {
        out.extend(search_items(&target.selection));
        out.push(keyed("Copy", 'c', selected, Choice::Copy));
    }
    out
}

/// `Copy Image`, and `Copy Image URL` except for an image that's the message's own (a `cid:`).
fn image_items(src: &str) -> Vec<Entry> {
    let item = |title: &str, choice: Choice| {
        (
            MenuItem::Item {
                title: title.to_owned(),
                key: None,
                symbol: None,
                enabled: true,
            },
            Some(choice),
        )
    };
    let mut out = vec![item("Copy Image", Choice::CopyImage)];
    if !src.starts_with("blob:") && !src.starts_with("cid:") {
        out.push(item("Copy Image URL", Choice::CopyText(src.to_owned())));
    }
    out
}

/// For a selection, `Look Up "…"`, `Search with Google` and a separator.
fn search_items(selection: &str) -> Vec<Entry> {
    if selection.is_empty() {
        return Vec::new();
    }
    let item = |title: String, choice: Choice| {
        (
            MenuItem::Item {
                title,
                key: None,
                symbol: None,
                enabled: true,
            },
            Some(choice),
        )
    };
    vec![
        item(
            format!("Look Up \"{}\"", truncated(selection)),
            Choice::LookUp,
        ),
        item(
            "Search with Google".to_owned(),
            Choice::Search(format!(
                "https://www.google.com/search?q={}",
                crate::format::urls::encode_component(selection)
            )),
        ),
        (MenuItem::Separator, None),
    ]
}

/// The first 25 characters and the rest of the word they end in (`/^.{0,25}[\S]*/`), a trailing
/// space trimmed, `…` if that cut it short.
fn truncated(text: &str) -> String {
    // `.` stops at a line break.
    let line = text
        .split(['\n', '\r', '\u{2028}', '\u{2029}'])
        .next()
        .unwrap_or_default();
    let head: String = line.chars().take(25).collect();
    let rest: String = line
        .chars()
        .skip(25)
        .take_while(|ch| !ch.is_whitespace())
        .collect();
    let matched = head + &rest;
    let length = matched.chars().count();
    let mut out = match matched.chars().last() {
        Some(last) if last.is_whitespace() => matched[..matched.len() - last.len_utf8()].to_owned(),
        _ => matched,
    };
    if length < text.chars().count() {
        out.push('…');
    }
    out
}

/// Strips an ASCII-case-insensitive `mailto:` prefix from `url`.
pub(crate) fn strip_mailto(url: &str) -> Option<&str> {
    url.get(..7)
        .filter(|scheme| scheme.eq_ignore_ascii_case("mailto:"))
        .map(|_| &url[7..])
}

/// Whether `Open Link` opens `url`. Only web links leave Exo, and `mailto:` opens compose.
fn is_trusted(url: &str) -> bool {
    url::Url::parse(url).is_ok_and(|url| matches!(url.scheme(), "http" | "https" | "mailto"))
}

/// Percent-decodes `text`, keeping what doesn't decode as it is.
fn decode(text: &str) -> String {
    String::from_utf8(exo_mail::percent_decode_bytes(text)).unwrap_or_else(|_| text.to_owned())
}

/// A right-click in a message body, waiting to show its menu: where on screen, what it landed
/// on, and the selected text with its baseline's start on screen.
pub(crate) struct Pending {
    pub at: egui::Pos2,
    pub hit: exo_mail::ContextHit,
    pub selection: Option<(String, egui::Pos2)>,
}

/// A right-click on the rest of the page, waiting to show its menu: where on screen, and the link
/// it landed on.
pub(crate) struct PagePending {
    pub at: egui::Pos2,
    pub link: Option<String>,
}

/// The size Look Up shows a message's text at, which is a body's usual 14px.
const BODY_TEXT: f32 = 14.0;

impl Mail {
    /// Notes this frame's right-click in `page` (the window under the tab strip) that nothing with
    /// a menu of its own took, for the shell's menu next frame. On the header's selectable text it
    /// selects the word under it first, as a Mac's right-click does.
    pub(crate) fn page_right_click(&mut self, ctx: &egui::Context, page: egui::Rect) {
        let clicked = ctx.input(|input| {
            input
                .pointer
                .secondary_clicked()
                .then(|| input.pointer.interact_pos())
                .flatten()
        });
        let Some(at) = clicked.filter(|at| page.contains(*at)) else {
            return;
        };
        if exo_ui::right_click_claimed(ctx) {
            return;
        }
        if let Some(thread) = &mut self.thread {
            thread.select_word_at(at);
            if thread.selected_text().is_some() {
                self.bodies.clear_selection();
            }
        }
        self.page_menu = Some(PagePending {
            at,
            link: exo_ui::link_at(ctx, at),
        });
        ctx.request_repaint();
    }

    /// Shows the shell's menu for a right-click on the rest of the page last frame, and carries
    /// out what's chosen in it. The selection is the header's or a body's.
    pub(crate) fn show_page_menu(&mut self, ctx: &egui::Context) {
        let Some(PagePending { at, link }) = self.page_menu.take() else {
            return;
        };
        let header = self.thread.as_ref().and_then(|thread| {
            thread
                .selected_text()
                .map(|text| (text, thread.selection_start()))
        });
        let body = self.bodies.selected_text().map(str::to_owned);
        let target = Target {
            link,
            image: None,
            editable: None,
            selection: header
                .as_ref()
                .map(|(text, _)| text.clone())
                .or_else(|| body.clone())
                .unwrap_or_default(),
        };
        let entries = entries(&target);
        let items: Vec<MenuItem> = entries.iter().map(|(item, _)| item.clone()).collect();
        let Some(choice) = (self.context_menu.borrow_mut())(&items, at)
            .and_then(|index| entries.into_iter().nth(index))
            .and_then(|(_, choice)| choice)
        else {
            return;
        };
        match choice {
            Choice::CopyText(text) => ctx.copy_text(text),
            Choice::OpenLink(url) if strip_mailto(&url).is_some() => {
                self.compose_mailto(&url, false)
            }
            Choice::OpenLink(url) | Choice::Search(url) => self.open_url(&url),
            Choice::LookUp => match (&header, &body) {
                (Some((text, Some((baseline, size)))), _) => {
                    context_menu::look_up(text, *baseline, *size)
                }
                // A body's selection is somewhere else on screen, which the menu doesn't know.
                (_, Some(text)) => context_menu::look_up(text, at, BODY_TEXT),
                _ => {}
            },
            Choice::Copy => match header {
                Some((text, _)) => ctx.copy_text(text),
                None => self.bodies.copy(),
            },
            // An image's or an editable field's, which this menu never has.
            Choice::CopyImage
            | Choice::Correct(_)
            | Choice::AddToDictionary(_)
            | Choice::IgnoreSpelling(_)
            | Choice::Cut
            | Choice::Paste => {}
        }
    }

    /// Shows the shell's menu for a right-click in a message body last frame (the word it
    /// selected is now on screen), and carries out what's chosen in it.
    pub(crate) fn show_message_menu(&mut self, ctx: &egui::Context) {
        let Some(Pending { at, hit, selection }) = self.message_menu.take() else {
            return;
        };
        let target = Target {
            link: hit.link.clone(),
            image: hit.image.as_ref().map(|image| image.src.clone()),
            editable: None,
            selection: selection
                .as_ref()
                .map(|(text, _)| text.clone())
                .unwrap_or_default(),
        };
        let entries = entries(&target);
        let items: Vec<MenuItem> = entries.iter().map(|(item, _)| item.clone()).collect();
        let Some(choice) = (self.context_menu.borrow_mut())(&items, at)
            .and_then(|index| entries.into_iter().nth(index))
            .and_then(|(_, choice)| choice)
        else {
            return;
        };
        match choice {
            Choice::CopyText(text) => ctx.copy_text(text),
            Choice::CopyImage => {
                if let Some(image) = &hit.image {
                    crate::clipboard::write_image(ctx, &image.pixels);
                }
            }
            Choice::OpenLink(url) if strip_mailto(&url).is_some() => {
                self.compose_mailto(&url, false)
            }
            Choice::OpenLink(url) | Choice::Search(url) => self.open_url(&url),
            Choice::LookUp => {
                if let Some((text, baseline)) = &selection {
                    context_menu::look_up(text, *baseline, BODY_TEXT);
                }
            }
            Choice::Copy => self.bodies.copy(),
            // An editable field's, not a message's.
            Choice::Correct(_)
            | Choice::AddToDictionary(_)
            | Choice::IgnoreSpelling(_)
            | Choice::Cut
            | Choice::Paste => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn titles(entries: &[Entry]) -> Vec<String> {
        entries
            .iter()
            .map(|(item, _)| match item {
                MenuItem::Item {
                    title,
                    enabled: true,
                    ..
                } => title.clone(),
                MenuItem::Item { title, .. } => format!("({title})"),
                MenuItem::Separator => "—".to_owned(),
            })
            .collect()
    }

    #[test]
    fn a_misspelled_word_offers_three_corrections_then_the_dictionary() {
        let target = Target {
            editable: Some(Editable {
                misspelled: Some("teh".into()),
                suggestions: ["the", "tea", "ten", "tech"].map(String::from).to_vec(),
            }),
            selection: "teh".into(),
            ..Target::default()
        };
        let entries = entries(&target);
        assert_eq!(
            titles(&entries),
            [
                "the",
                "tea",
                "ten",
                "—",
                "Add to Dictionary",
                "Ignore Spelling",
                "Look Up \"teh\"",
                "Search with Google",
                "—",
                "Cut",
                "Copy",
                "Paste"
            ]
        );
        assert_eq!(entries[0].1, Some(Choice::Correct("the".into())));
        assert_eq!(
            entries[7].1,
            Some(Choice::Search("https://www.google.com/search?q=teh".into()))
        );
        assert!(matches!(
            &entries[9].0,
            MenuItem::Item {
                key: Some(MenuKey {
                    key: 'x',
                    shift: false,
                    command: true
                }),
                ..
            }
        ));
    }

    #[test]
    fn nothing_selected_leaves_cut_and_copy_off_and_paste_on() {
        let target = Target {
            editable: Some(Editable::default()),
            ..Target::default()
        };
        assert_eq!(titles(&entries(&target)), ["(Cut)", "(Copy)", "Paste"]);
        assert_eq!(
            titles(&entries(&Target {
                selection: "hi there".into(),
                ..Target::default()
            })),
            ["Look Up \"hi there\"", "Search with Google", "—", "Copy"]
        );
    }

    #[test]
    fn a_link_is_copied_or_opened() {
        let link = |href: &str| {
            entries(&Target {
                link: Some(href.into()),
                editable: Some(Editable::default()),
                ..Target::default()
            })
        };
        let web = link("https://exo.example/a?b=c");
        assert_eq!(titles(&web), ["Copy Link", "Open Link"]);
        assert_eq!(
            web[0].1,
            Some(Choice::CopyText("https://exo.example/a?b=c".into()))
        );
        let mail = link("mailto:priya%2Bwork@example.com?subject=Hi");
        assert_eq!(titles(&mail), ["Copy Email Address", "Open Link"]);
        assert_eq!(
            mail[0].1,
            Some(Choice::CopyText("priya+work@example.com".into()))
        );
        let upper_mail = link("MAILTO:priya@example.com?subject=Hi");
        assert_eq!(titles(&upper_mail), ["Copy Email Address", "Open Link"]);
        assert_eq!(
            upper_mail[0].1,
            Some(Choice::CopyText("priya@example.com".into()))
        );
        assert!(strip_mailto("MAILTO:priya@example.com").is_some());
        assert_eq!(
            titles(&link("javascript:alert(1)")),
            ["Copy Link"],
            "untrusted: not opened"
        );
    }

    #[test]
    fn an_image_is_copied_and_its_address_unless_its_the_messages_own() {
        let image = |src: &str, link: Option<&str>| {
            titles(&entries(&Target {
                image: Some(src.into()),
                link: link.map(str::to_owned),
                selection: "x".into(),
                ..Target::default()
            }))
        };
        assert_eq!(
            image("https://exo.example/logo.png", None),
            ["Copy Image", "Copy Image URL"]
        );
        assert_eq!(
            image("cid:logo@exo", None),
            ["Copy Image"],
            "an attachment's `cid:`"
        );
        assert_eq!(
            image("https://exo.example/logo.png", Some("https://exo.example")),
            [
                "Copy Link",
                "Open Link",
                "—",
                "Copy Image",
                "Copy Image URL"
            ]
        );
    }

    #[test]
    fn look_up_names_the_selection_cut_after_a_word() {
        assert_eq!(truncated("short"), "short");
        assert_eq!(
            truncated("the quick brown fox jumps over the lazy dog"),
            "the quick brown fox jumps…"
        );
        assert_eq!(
            truncated("the quick brown fox jumpsover the lazy dog"),
            "the quick brown fox jumpsover…"
        );
        assert_eq!(
            truncated("the quick brown fox jump  over"),
            "the quick brown fox jump…",
            "the space the match ends in goes"
        );
    }
}

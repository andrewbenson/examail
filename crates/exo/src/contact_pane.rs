//! The right pane's contact card, from what the account's own mail says (name, address, recent
//! conversations and the company domain) and, for a saved contact or a colleague, what Google
//! says: their photo, company and title. Under the name, what's too tall for the pane scrolls.

use egui::{Color32, Id, Painter, Rect, Sense, Ui, Vec2, pos2, vec2};
use exo_ui::{BoxShadow, Family, Font, IconButton, TextStyle};

use crate::{format::names::is_popular_domain, mailbox::ContactCard};

/// The name: 16px/24px.
const NAME: TextStyle = TextStyle::new(Font::new(Family::Sans, 400), 16.0, 24.0);
/// Section headers, the address and the domain: 12px/16px bold.
const BOLD: TextStyle = TextStyle::new(Font::new(Family::Sans, 700), 12.0, 16.0);
/// A conversation's row: 12px/16px.
const DATA: TextStyle = TextStyle::new(Font::new(Family::Sans, 400), 12.0, 16.0);
/// Conversations listed in the Mail section.
const MAIL_ROWS: usize = 4;
/// Row pitch in the Mail section: 16px lines with collapsed 8px margins.
const MAIL_PITCH: f32 = 24.0;
/// The header's photo.
const PHOTO: f32 = 56.0;
/// The name's row, `padding: 32px 2rem 24px` around its 24px line, over what scrolls.
const NAME_ROW: f32 = 80.0;

/// How far the card has scrolled under its name. It's one pane whoever it shows, so the next
/// contact keeps the scroll as far as it reaches.
#[derive(Clone, Default)]
struct Scroll {
    offset: f32,
    /// How tall what scrolls was last drawn.
    content: f32,
    bar: exo_ui::Scrollbar,
}

/// What the user did in the contact pane.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ContactAction {
    /// Clicked the name: search for everything from or to them.
    Search(String),
    /// Clicked the address: compose to it.
    Compose(String),
    /// Clicked a conversation in the Mail section.
    OpenThread(String),
    /// Clicked the domain link.
    OpenDomain(String),
    /// Copied the name or the address with its copy button.
    Copied(String),
}

/// Context for the contact card on screen.
pub struct PaneContext<'a> {
    /// The signed-in account showing the pane.
    pub account: &'a str,
    /// The conversation the cursor or thread view is on, left out of Mail.
    pub current: Option<&'a str>,
    /// Whether the card is for the account's own address, which hides Mail.
    pub is_me: bool,
    /// Whether the message on screen for this sender allows showing a brand logo.
    pub allow_brand: bool,
    pub now: jiff::Timestamp,
}

/// Paints `card` in `pane` (the right pane inside its border, above the footer), leaving out
/// `cx.current` from its conversations, which say how long ago they were before `cx.now` while
/// hovered.
pub fn show(
    ui: &mut Ui,
    pane: Rect,
    rem: f32,
    card: &ContactCard,
    cx: PaneContext<'_>,
) -> Option<ContactAction> {
    // What's under the name scrolls (`.ContactPane-overlay`), clipped to its box.
    let overlay = Rect::from_min_max(pos2(pane.left(), pane.top() + NAME_ROW), pane.max);
    let mut content_ui = ui.new_child(egui::UiBuilder::new().max_rect(overlay));
    content_ui.set_clip_rect(overlay.intersect(ui.clip_rect()));
    let ui: &Ui = ui;
    let palette = exo_ui::palette(ui.ctx());
    // The name and the section headers: `rgba(0, 0, 0, .8)`, `hsla(0, 0%, 100%, .85)` in Carbon.
    let ink = palette.ink(0.8, 0.85);
    let painter = ui.painter_at(pane);
    let left = pane.left() + 2.0 * rem;
    let width = pane.width() - 4.0 * rem;
    let mut action = None;

    // Name: `padding: 32px 2rem 24px`, with a copy button that shows while it's hovered.
    let name = if card.person.name.is_empty() {
        card.person.email.as_str()
    } else {
        card.person.name.as_str()
    };
    let name_width = NAME.single_line(&painter, name, ink, Some(width)).size().x;
    let name_rect = Rect::from_min_size(pos2(left, pane.top() + 32.0), vec2(name_width, 24.0));
    let name_row = Rect::from_min_max(
        pos2(pane.left(), pane.top()),
        pos2(pane.right(), pane.top() + NAME_ROW),
    );
    if click(ui, exo_ui::id(ui.ctx(), "contact-name"), name_rect) {
        action = Some(ContactAction::Search(name.to_owned()));
    }
    NAME.paint_line(&painter, name_rect.min, name, ink, Some(width));
    if copy_button(
        ui,
        &painter,
        name_row,
        pos2(name_rect.right() + 8.0, pane.top() + 34.0),
        name,
    ) {
        action = Some(ContactAction::Copied(name.to_owned()));
    }

    // The rest scrolls under the name, with the wheel, and at once to a match find moves to.
    let pane_ui = ui;
    let scroll_id = exo_ui::id(ui.ctx(), "contact-pane-scroll");
    let find = scroll_id.with("find");
    let mut scroll = ui
        .ctx()
        .data(|data| data.get_temp::<Scroll>(scroll_id))
        .unwrap_or_default();
    if ui.rect_contains_pointer(overlay) {
        scroll.offset -= ui.input(|input| input.smooth_scroll_delta.y);
    }
    scroll.offset += exo_ui::find::reveal(ui.ctx(), find).map_or(0.0, |delta| delta.y);
    scroll.offset = scroll
        .offset
        .clamp(0.0, (scroll.content - overlay.height()).max(0.0));
    let ui = &content_ui;
    let painter = ui.painter().clone();
    let found = exo_ui::find::Scope::begin(ui.ctx(), find, overlay, scroll.offset);
    // Where the pane's top would be, scrolled.
    let top = pane.top() - scroll.offset;

    // Someone Google knows, or whose brand has a verified logo, has a 60px header: their photo
    // (else the logo), with the address 13px beside it in the pane's ink. Otherwise the contact
    // details are the address alone, in `rgba(0, 0, 0, .55)` (Carbon `hsla(0, 0%, 100%, .65)`).
    // Either way it's 4px below the overlay's top.
    let person = crate::people::person_for(ui.ctx(), cx.account, &card.person.email);
    let brand = cx
        .allow_brand
        .then(|| crate::brands::logo(ui.ctx(), cx.account, &card.person.email))
        .flatten();
    let has_header = person.is_some() || brand.is_some();
    let (email_left, details_ink, details_bottom) = if has_header {
        (left + PHOTO + 13.0, ink, 140.0)
    } else {
        (left, palette.ink(0.55, 0.65), 124.0)
    };
    let details = Rect::from_min_max(
        pos2(pane.left(), top + NAME_ROW),
        pos2(pane.right(), top + details_bottom),
    );
    let email_width = BOLD
        .single_line(
            &painter,
            &card.person.email,
            ink,
            Some(width - (email_left - left)),
        )
        .size()
        .x;
    let email_rect = Rect::from_min_size(pos2(email_left, top + 89.0), vec2(email_width, 16.0));
    if click(ui, exo_ui::id(ui.ctx(), "contact-email"), email_rect) {
        action = Some(ContactAction::Compose(card.person.email.clone()));
    }
    if ui.rect_contains_pointer(email_rect) {
        // `a:hover { text-decoration: underline }`.
        let baseline = email_rect.top() + BOLD.baseline(ui.ctx()) + 1.0;
        painter.hline(
            email_rect.x_range(),
            baseline + 0.5,
            egui::Stroke::new(1.0, details_ink),
        );
    }
    BOLD.paint_line(
        &painter,
        email_rect.min,
        &card.person.email,
        details_ink,
        Some(width - (email_left - left)),
    );
    // The glyph 10px after the address (the button's `padding: 10px`).
    if copy_button(
        ui,
        &painter,
        details,
        pos2(email_rect.right() + 8.5, top + 88.0),
        &card.person.email,
    ) {
        action = Some(ContactAction::Copied(card.person.email.clone()));
    }

    let mut section_top = top + 124.0;
    if has_header {
        // The photo, 4px down, over the default avatar, which shows until it loads.
        let photo = Rect::from_min_size(pos2(left, top + 84.0), Vec2::splat(PHOTO));
        crate::people::paint_avatar_with_brand_for(
            &painter,
            photo,
            cx.account,
            &card.person.email,
            brand.as_ref(),
            false,
        );
        // The header's 60px and its 16px bottom padding.
        section_top = top + 156.0;
    }
    if let Some(person) = &person {
        section_top += company(ui, &painter, left, width, section_top, person);
    }

    // Mail: the latest conversations with them, besides the current one.
    let recent: Vec<_> = card
        .recent
        .iter()
        .filter(|thread| Some(thread.id.as_str()) != cx.current)
        .take(MAIL_ROWS)
        .collect();
    if !cx.is_me && !recent.is_empty() {
        // The header, which spans the section, searches their address.
        let header = Rect::from_min_size(pos2(left, section_top), vec2(width, 16.0));
        section_header(
            ui,
            &painter,
            left,
            section_top,
            "legacy/DEPRECATED_mail",
            "Mail",
            header,
        );
        if click(ui, exo_ui::id(ui.ctx(), "contact-mail"), header) {
            action = Some(ContactAction::Search(card.person.email.clone()));
        }
        for (i, thread) in recent.iter().enumerate() {
            let top = section_top + 25.0 + MAIL_PITCH * i as f32;
            // A row spans the column. While hovered, it shows how long ago the conversation was at
            // its right (1rem clear of the subject, black or white at .5), with the subject cut
            // short of that.
            let row = Rect::from_min_size(pos2(left, top), vec2(width, 16.0));
            let mut title_width = width;
            if ui.rect_contains_pointer(row) {
                let ago = crate::format::dates::relative(thread.date, cx.now);
                let ago_width = DATA.width(ui.ctx(), &ago);
                DATA.paint_line(
                    &painter,
                    pos2(row.right() - ago_width, top),
                    &ago,
                    palette.ink(0.5, 0.5),
                    None,
                );
                title_width = (width - ago_width - rem).max(0.0);
            }
            DATA.paint_line(
                &painter,
                pos2(left, top),
                &thread.subject,
                palette.ink(0.55, 0.65),
                Some(title_width),
            );
            if click(ui, exo_ui::id(ui.ctx(), ("contact-thread", i)), row) {
                action = Some(ContactAction::OpenThread(thread.id.clone()));
            }
        }
        // Header, 8px gap, rows, and the section's 20px bottom padding.
        section_top += 16.0 + 8.0 + MAIL_PITCH * recent.len() as f32 + 20.0;
    }

    // Links: the company domain, unless it's a webmail provider.
    let domain = card
        .person
        .email
        .rsplit_once('@')
        .map(|(_, domain)| domain.to_lowercase());
    if let Some(domain) = domain.filter(|domain| !is_popular_domain(domain)) {
        let area = Rect::from_min_size(
            pos2(left, section_top),
            vec2(24.0 + BOLD.width(ui.ctx(), &domain), 16.0),
        );
        let rect = section_header(
            ui,
            &painter,
            left,
            section_top,
            "legacy/DEPRECATED_homepage",
            &domain,
            area,
        );
        exo_ui::link_box(ui.ctx(), rect, &format!("https://{domain}"));
        if click(ui, exo_ui::id(ui.ctx(), "contact-domain"), rect) {
            action = Some(ContactAction::OpenDomain(domain));
        }
        section_top += 16.0;
    }
    drop(found);

    scroll.content = section_top - (top + NAME_ROW);
    scroll
        .bar
        .show(pane_ui, scroll_id, overlay, scroll.offset, scroll.content);
    if scroll.offset > 0.0 {
        // Scrolled, the overlay has a 1px line inside its top (`.isScrolled`'s inset shadow):
        // black at .04, Carbon's white at .1.
        pane_ui.painter().rect_filled(
            Rect::from_min_size(overlay.min, vec2(overlay.width(), 1.0)),
            0.0,
            palette.ink(0.04, 0.1),
        );
    }
    pane_ui
        .ctx()
        .data_mut(|data| data.insert_temp(scroll_id, scroll));
    action
}

/// The company section: the company (or, without one, the title) as its header, 24px in where
/// an icon would be, and the title as its row, each wrapping. Returns the section's height.
fn company(
    ui: &Ui,
    painter: &Painter,
    left: f32,
    width: f32,
    top: f32,
    person: &crate::people::Person,
) -> f32 {
    let palette = exo_ui::palette(ui.ctx());
    let (header, row) = match (&person.company, &person.title) {
        (Some(company), title) => (company, title.as_ref()),
        (None, Some(title)) => (title, None),
        (None, None) => return 0.0,
    };
    let mut y = top;
    let header_lines = BOLD.wrap(ui.ctx(), header, width - 24.0);
    let header_rect =
        Rect::from_min_size(pos2(left, y), vec2(width, 16.0 * header_lines.len() as f32));
    // A section header takes the pointer, whether or not it goes anywhere.
    ui.interact(
        header_rect,
        exo_ui::id(ui.ctx(), "contact-company"),
        Sense::hover(),
    )
    .on_hover_cursor(egui::CursorIcon::PointingHand);
    for line in &header_lines {
        // `top: 1px`.
        BOLD.paint_line(
            painter,
            pos2(left + 24.0, y + 1.0),
            line,
            palette.ink(0.8, 0.85),
            None,
        );
        y += 16.0;
    }
    // The header's 8px margin, then the row and its 8px margin.
    y += 8.0;
    if let Some(row) = row {
        for line in DATA.wrap(ui.ctx(), row, width) {
            DATA.paint_line(
                painter,
                pos2(left, y + 1.0),
                &line,
                palette.ink(0.55, 0.65),
                None,
            );
            y += 16.0;
        }
        y += 8.0;
    }
    // The section's 20px bottom padding.
    y + 20.0 - top
}

/// A section header: a 16px icon and bold text 24px to its right, 1px lower. The icon is a disc
/// of `#bfc8d2` (`#555b64` in Carbon, `#929dac` while the header, `area`, is hovered) with its
/// glyph cut out in the pane's color. Returns the text's rect.
fn section_header(
    ui: &Ui,
    painter: &Painter,
    left: f32,
    top: f32,
    icon: &str,
    text: &str,
    area: Rect,
) -> Rect {
    let palette = exo_ui::palette(ui.ctx());
    let hovered = ui.rect_contains_pointer(area);
    let (disc, glyph) = match palette.theme {
        exo_ui::Theme::Snow => (Color32::from_rgb(0xbf, 0xc8, 0xd2), Color32::WHITE),
        exo_ui::Theme::Carbon => (
            Color32::from_rgb(0x55, 0x5b, 0x64),
            Color32::from_rgb(0x18, 0x19, 0x1a),
        ),
    };
    let disc = if hovered {
        Color32::from_rgb(0x92, 0x9d, 0xac)
    } else {
        disc
    };
    exo_ui::paint_icon_two_tone(
        ui,
        icon,
        exo_ui::pixel_snap(Rect::from_min_size(pos2(left, top), Vec2::splat(16.0))),
        glyph,
        disc,
    );
    let width = BOLD.paint_line(
        painter,
        pos2(left + 24.0, top + 1.0),
        text,
        palette.ink(0.8, 0.85),
        None,
    );
    Rect::from_min_size(pos2(left + 24.0, top + 1.0), vec2(width, 16.0))
}

fn click(ui: &Ui, id: Id, rect: Rect) -> bool {
    ui.interact(rect, id, Sense::click())
        .on_hover_cursor(egui::CursorIcon::PointingHand)
        .clicked()
}

/// A copy icon in the 14px box at `icon_top_left` that copies `text` and is visible while `area`
/// is hovered. The glyph is the 11×14 `legacy/DEPRECATED_clipboard`, centered. Returns whether it
/// was clicked.
fn copy_button(
    ui: &Ui,
    painter: &Painter,
    area: Rect,
    icon_top_left: egui::Pos2,
    text: &str,
) -> bool {
    if !ui.rect_contains_pointer(area) {
        return false;
    }
    let icon = Rect::from_min_size(icon_top_left, Vec2::splat(14.0));
    let glyph = Rect::from_center_size(icon.center(), vec2(11.0, 14.0));
    // 10px around the icon's box take the click.
    let hit = icon.expand2(vec2(10.0, 10.0));
    if ui.rect_contains_pointer(hit) {
        // A 22px raised disc behind the icon while the button itself is hovered.
        let palette = exo_ui::palette(ui.ctx());
        let disc = Rect::from_center_size(icon.center(), Vec2::splat(22.0));
        let radius = egui::CornerRadius::same(11);
        let shadows: &[BoxShadow] = match palette.theme {
            exo_ui::Theme::Snow => &[
                BoxShadow::new([0.0, 0.0], 6.0, 0.0, Color32::from_black_alpha(26)),
                BoxShadow::new([0.0, 2.0], 6.0, 0.0, Color32::from_black_alpha(38)),
                BoxShadow::new([0.0, 0.0], 0.5, 0.0, Color32::from_black_alpha(26)),
            ],
            exo_ui::Theme::Carbon => &[
                BoxShadow::new([0.0, 0.0], 8.0, 0.0, Color32::from_rgb(0x1b, 0x1c, 0x1e)),
                BoxShadow::new([0.0, 1.0], 8.0, 0.0, Color32::from_rgb(0x14, 0x15, 0x17)),
            ],
        };
        for shadow in shadows {
            shadow.paint(painter, disc, radius);
        }
        // `#fff`, `#363a40` in Carbon with its inset highlight.
        painter.rect_filled(disc, radius, palette.background_elevated_default);
        if palette.theme == exo_ui::Theme::Carbon {
            exo_ui::paint_carbon_highlight(
                painter,
                disc,
                radius,
                palette.background_elevated_default,
            );
        }
    }
    let clicked = IconButton::new("legacy/DEPRECATED_clipboard")
        .show(
            ui,
            exo_ui::id(ui.ctx(), ("contact-copy", icon_top_left.y as i32)),
            hit,
            glyph,
        )
        .clicked();
    if clicked {
        ui.ctx().copy_text(text.to_owned());
    }
    clicked
}

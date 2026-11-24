//! The header's tooltips that stay while the pointer is on them. A participant's tooltip shows
//! `{name} <{address}>` with a copy button, and a click on its address composes to them. The
//! subject has a copy tooltip. Both are tooltip boxes (dark in Snow, light in Carbon), centred 8px
//! under their target and kept inside the message column.

use egui::{Color32, Order, Pos2, Rect, Sense, Ui, pos2, vec2};
use exo_store::Contact;
use exo_ui::{CopyTooltip, Family, Font, HoverIntent, HoverTooltip, Placement, TextStyle};

use crate::format::names::Person;

/// The tooltips' text: 12px/20px bold, `padding: 6px 7px 3px`.
const TEXT: TextStyle = TextStyle::new(Font::new(Family::Sans, 700), 12.0, 20.0);
const PADDING_X: f32 = 7.0;
const PADDING_TOP: f32 = 6.0;
const PADDING_BOTTOM: f32 = 3.0;
/// The gap between a tooltip and its target.
const GAP: f32 = 8.0;
/// A participant's tooltip polls its hover intent every 200ms, and the subject's every 100ms.
const CONTACT_INTERVAL: f64 = 0.2;
const COPY: &str = "legacy/DEPRECATED_clipboard";

/// What a click in a tooltip asks for.
#[derive(Debug, PartialEq, Eq)]
pub(super) enum Clicked {
    /// A new message to the participant, popped out with ⇧.
    ComposeTo(Person, bool),
    /// This went on the clipboard.
    Copied(String),
}

#[derive(Clone, Debug, PartialEq)]
enum Showing {
    Contact(Contact),
    Subject(String),
}

/// The tooltip up, if any, between frames, and the pointer settling on a target.
pub(super) struct Tooltips {
    hover: HoverTooltip<Showing>,
    target: Rect,
    contact_intent: HoverIntent<Contact>,
    subject_intent: HoverIntent<String>,
}

impl Default for Tooltips {
    fn default() -> Self {
        Self {
            hover: HoverTooltip::default(),
            target: Rect::NOTHING,
            contact_intent: HoverIntent::new(CONTACT_INTERVAL),
            subject_intent: HoverIntent::default(),
        }
    }
}

impl Tooltips {
    /// Follows the pointer over this frame's targets: the participants' names (`contacts`) and
    /// the subject. Shows a target's tooltip once hover intent has the pointer settled on it,
    /// keeps it until the pointer has been off the target for 150ms and isn't on the tooltip, and
    /// paints it within `within`.
    pub(super) fn show(
        &mut self,
        ui: &Ui,
        contacts: &[(Rect, Contact)],
        subject: Option<(Rect, &str)>,
        within: egui::Rangef,
    ) -> Option<Clicked> {
        let ctx = ui.ctx().clone();
        let pointer = ctx.pointer_hover_pos();
        // Only what's on top under the pointer counts as hovered (not a target under the
        // palette), but the open tooltip is a layer of its own.
        let on_top = pointer.filter(|at| ctx.layer_id_at(*at) == Some(ui.layer_id()));
        let over = |rect: &Rect| on_top.is_some_and(|at| rect.contains(at));
        let hovered = contacts
            .iter()
            .find(|(rect, _)| over(rect))
            .map(|(rect, contact)| (*rect, Showing::Contact(contact.clone())))
            .or_else(|| {
                subject
                    .filter(|(rect, _)| over(rect))
                    .map(|(rect, text)| (rect, Showing::Subject(text.to_owned())))
            });
        if let Some((target, ref what)) = hovered
            && self.hover.active() == Some(what)
        {
            self.target = target;
        }
        if let Some(open) = self.hover.active().cloned() {
            let box_rect = self.rect(&ctx, self.target, &open, within);
            self.hover.set_box(box_rect, false);
        }
        let (contact, subject_text) = match &hovered {
            Some((_, Showing::Contact(contact))) => (Some(contact.clone()), None),
            Some((_, Showing::Subject(subject))) => (None, Some(subject.clone())),
            None => (None, None),
        };
        let settled = match (
            self.contact_intent.update(&ctx, contact),
            self.subject_intent.update(&ctx, subject_text),
        ) {
            (Some(contact), _) => Some(Showing::Contact(contact.clone())),
            (_, Some(subject)) => Some(Showing::Subject(subject.clone())),
            _ => None,
        };
        if let (Some(what), Some((target, _))) = (&settled, &hovered)
            && self.hover.active() != Some(what)
        {
            self.target = *target;
        }
        let active_target = hovered.map(|(_, what)| what);
        let (showing, opacity) = {
            let (what, opacity) = self
                .hover
                .step_settled(&ctx, active_target, settled, false)?;
            (what.clone(), opacity)
        };
        let rect = self.rect(&ctx, self.target, &showing, within);
        self.hover.set_box(rect, false);
        let mut clicked = None;
        egui::Area::new(exo_ui::id(&ctx, "thread-header-tooltip"))
            .order(Order::Tooltip)
            .fixed_pos(rect.min)
            .constrain(false)
            .show(&ctx, |area| {
                let mut painter = area.painter().clone();
                painter.multiply_opacity(opacity);
                exo_ui::paint_tooltip_box(&painter, rect);
                let line = pos2(rect.left() + PADDING_X, rect.top() + PADDING_TOP);
                let whole = area.interact(
                    rect,
                    exo_ui::id(area.ctx(), "thread-header-tooltip-box"),
                    Sense::click(),
                );
                clicked = match &showing {
                    Showing::Contact(contact) => contact_content(area, &painter, line, contact),
                    Showing::Subject(subject) => CopyTooltip::paint_content(
                        area,
                        &painter,
                        exo_ui::id(area.ctx(), "thread-subject-copy"),
                        rect,
                        subject,
                    )
                    .map(|_| Clicked::Copied(subject.to_owned())),
                };
                // A click anywhere else on it closes it.
                if clicked.is_some() || whole.clicked() {
                    self.hover.close();
                }
            });
        clicked
    }

    /// Where the open tooltip's box goes, centred under its target and inside `within`.
    fn rect(
        &self,
        ctx: &egui::Context,
        target: Rect,
        what: &Showing,
        within: egui::Rangef,
    ) -> Rect {
        let size = match what {
            Showing::Contact(contact) => vec2(
                contact_width(ctx, contact) + 2.0 * PADDING_X,
                PADDING_TOP + TEXT.line_height + PADDING_BOTTOM,
            ),
            Showing::Subject(subject) => CopyTooltip::box_size(ctx, subject),
        };
        Placement::Below {
            gap: GAP,
            shift: 0.0,
        }
        .place(target, size, within)
    }
}

/// The copy button's width, from `padding: 8px 4px 8px 7px` and the 12px icon box less its
/// `margin-right: -3px`. The glyph sits 1px under the line's top (`top: 2px; margin-top: -2px`, on
/// the baseline).
const CONTACT_COPY: f32 = 7.0 + 12.0 - 3.0 + 4.0;

/// The line's width as [`contact_content`] paints it, piece by piece (Chromium shapes each text
/// node on its own).
fn contact_width(ctx: &egui::Context, contact: &Contact) -> f32 {
    let pieces = match contact.name.as_deref().filter(|name| !name.is_empty()) {
        Some(name) => vec![name, " <", &contact.email, ">"],
        None => vec![contact.email.as_str()],
    };
    pieces
        .into_iter()
        .map(|piece| TEXT.width(ctx, piece))
        .sum::<f32>()
        + CONTACT_COPY
}

/// `{name} <{address}>`, or the address alone, as the copy button copies it.
fn contact_text(contact: &Contact) -> String {
    match contact.name.as_deref().filter(|name| !name.is_empty()) {
        Some(name) => format!("{name} <{}>", contact.email),
        None => contact.email.clone(),
    }
}

/// A participant's tooltip line from `line`: the name, ` <address>` at 70%,
/// then the copy button at 40%, 60% hovered.
fn contact_content(
    ui: &Ui,
    painter: &egui::Painter,
    line: Pos2,
    contact: &Contact,
) -> Option<Clicked> {
    let ink = exo_ui::tooltip_text_color(ui.ctx());
    let name = contact.name.as_deref().filter(|name| !name.is_empty());
    let mut x = line.x;
    if let Some(name) = name {
        x += TEXT.paint_line(painter, line, name, ink, None);
    }
    let mut dim = painter.clone();
    dim.multiply_opacity(0.7);
    let open = if name.is_some() { " <" } else { "" };
    x += TEXT.paint_line(&dim, pos2(x, line.y), open, ink, None);
    let email = TEXT.paint_line(&dim, pos2(x, line.y), &contact.email, ink, None);
    let address = Rect::from_min_size(pos2(x, line.y), vec2(email, TEXT.line_height));
    x += email;
    if name.is_some() {
        x += TEXT.paint_line(&dim, pos2(x, line.y), ">", ink, None);
    }
    let button = Rect::from_min_size(pos2(x, line.y - 2.0), vec2(CONTACT_COPY, 24.0));
    let copy = ui
        .interact(
            button,
            exo_ui::id(ui.ctx(), "thread-contact-copy"),
            Sense::click(),
        )
        .on_hover_cursor(egui::CursorIcon::PointingHand);
    let shown = if copy.hovered() { 0.6 } else { 0.4 };
    // `fill: #fff` (`#000` in Carbon) on the button's opacity.
    let fill = if exo_ui::theme(ui.ctx()) == exo_ui::Theme::Snow {
        Color32::WHITE
    } else {
        Color32::BLACK
    };
    let glyph = Rect::from_min_size(pos2(x + 7.0, line.y + 1.0), vec2(11.0, 14.0));
    exo_ui::paint_icon_on(painter, COPY, glyph, fill.gamma_multiply(shown));
    let compose = ui
        .interact(
            address,
            exo_ui::id(ui.ctx(), "thread-contact-compose"),
            Sense::click(),
        )
        .on_hover_cursor(egui::CursorIcon::PointingHand);
    if copy.clicked() {
        return Some(Clicked::Copied(contact_text(contact)));
    }
    if compose.clicked() {
        let pop_out = ui.input(|input| input.modifiers.shift_only());
        return Some(Clicked::ComposeTo(
            Person::new(
                contact.name.clone().unwrap_or_default(),
                contact.email.clone(),
            ),
            pop_out,
        ));
    }
    None
}

#[cfg(test)]
mod tests {
    use egui::{Context, Event, PointerButton, Rangef, RawInput};

    use super::*;

    #[test]
    fn subject_tooltip_settles_bridges_gap_and_copies_on_click() {
        let ctx = Context::default();
        exo_ui::install(&ctx).expect("the system fonts load");
        let mut tooltips = Tooltips::default();
        let target = Rect::from_min_size(pos2(100.0, 40.0), vec2(120.0, 20.0));

        let step = |tooltips: &mut Tooltips, time: f64, events: Vec<Event>| -> Option<Clicked> {
            let mut out = None;
            ctx.run_ui(
                RawInput {
                    time: Some(time),
                    events,
                    ..Default::default()
                },
                |ui| {
                    let _ = ui.allocate_rect(
                        Rect::from_min_size(Pos2::ZERO, vec2(600.0, 400.0)),
                        Sense::hover(),
                    );
                    out = tooltips.show(
                        ui,
                        &[],
                        Some((target, "Quarterly Review")),
                        Rangef::new(0.0, 600.0),
                    );
                },
            )
            .drop_without_applying_deltas();
            out
        };

        step(
            &mut tooltips,
            0.0,
            vec![Event::PointerMoved(pos2(10.0, 10.0))],
        );
        step(
            &mut tooltips,
            1.0,
            vec![Event::PointerMoved(target.center())],
        );
        assert!(!tooltips.hover.is_open(), "not yet settled");
        step(
            &mut tooltips,
            1.12,
            vec![Event::PointerMoved(target.center())],
        );
        assert!(tooltips.hover.is_open(), "settled after 100ms subject poll");

        // Move into the 8px bridge below `target` (y = 64.0) for longer than 150ms: stays open!
        step(
            &mut tooltips,
            1.5,
            vec![Event::PointerMoved(pos2(target.center().x, 64.0))],
        );
        assert!(tooltips.hover.is_open(), "bridge keeps tooltip open");

        // Click the subject text inside the tooltip box (y = 76.0).
        let click_pos = pos2(target.center().x, 76.0);
        step(
            &mut tooltips,
            1.6,
            vec![
                Event::PointerMoved(click_pos),
                Event::PointerButton {
                    pos: click_pos,
                    button: PointerButton::Primary,
                    pressed: true,
                    modifiers: Default::default(),
                },
            ],
        );
        let clicked = step(
            &mut tooltips,
            1.62,
            vec![Event::PointerButton {
                pos: click_pos,
                button: PointerButton::Primary,
                pressed: false,
                modifiers: Default::default(),
            }],
        );
        assert_eq!(clicked, Some(Clicked::Copied("Quarterly Review".into())));
        assert!(!tooltips.hover.is_open(), "clicking copies and closes");
    }
}

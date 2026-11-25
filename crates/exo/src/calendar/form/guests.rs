//! Who field: guest input, parsing, chips, hover controls, and contact suggestions.

use egui::{Color32, CornerRadius, Rect, Sense, Stroke, Ui, Vec2, pos2, vec2};
use exo_store::Address;
use exo_ui::{ContactAcceptKeys, ContactKeySnapshot, ContactSearchOutcome, KeyTooltip};

use super::{
    EventForm, Field, Guest, Mode, ROW, SMALL, TipClick,
    overlays::Overlay,
    rows::{InputSpec, RowContext, input},
};
use crate::{
    calendar::{
        details::paint_avatar,
        event::Rsvp,
        guests::{Marker, paint_marker},
    },
    compose::recipients,
};

/// A guest's line under the input is 24px tall and 322px at most, 4px from the next (`gap: 4px`),
/// with the first 7px under the input. It holds a 20px avatar 2px down, then, 8px on and inside the
/// content's 1px border and 7px of padding, the swatch, and the name 8px after the swatch.
const GUEST: f32 = 24.0;
const GUEST_GAP: f32 = 4.0;
const GUESTS_TOP: f32 = 7.0;
const GUEST_MAX: f32 = 322.0;
const AVATAR: f32 = 20.0;
/// The content's border while hovered (`rgba(0, 0, 0, .09)`, Carbon `#636973`) and while the
/// keyboard is on the guest (`#aeb1dd` in both).
const GUEST_HOVER_CARBON: Color32 = Color32::from_rgb(0x63, 0x69, 0x73);
const GUEST_FOCUS: Color32 = Color32::from_rgb(0xae, 0xb1, 0xdd);
/// The optional toggle and `Remove` are 24px boxes at the content's right. The cross's box sits
/// over the content's padding, and the toggle's sits 4px under the cross's
/// (`margin: -4px; margin-right: -8px`). The toggle's icon is 16px and the cross's 8px, both
/// `#a3a3a3`.
const GUEST_BUTTON: f32 = 24.0;
const GUEST_ICON: Color32 = Color32::from_rgb(0xa3, 0xa3, 0xa3);
/// A guest's swatch: 8px, 2px corners.
const SWATCH: f32 = 8.0;
/// The right pane lists four guests while the field isn't focused, then `+ {n} more`.
const PANE_GUESTS: usize = 4;

/// A suggestion under Who.
#[derive(Clone, Debug)]
pub(super) enum Suggestion {
    Contact(Address),
    /// `Everybody in this conversation`.
    Everybody(Vec<Address>),
}

impl Suggestion {
    /// The guests it adds.
    pub(super) fn guests(&self) -> Vec<Guest> {
        let addresses = match self {
            Suggestion::Contact(address) => std::slice::from_ref(address),
            Suggestion::Everybody(addresses) => addresses.as_slice(),
        };
        addresses
            .iter()
            .map(|address| Guest {
                email: address.email.clone(),
                name: address.name.clone(),
                optional: false,
                response: None,
                me: false,
                organizer: false,
            })
            .collect()
    }
}

impl EventForm {
    /// What's typed in Who and not yet a guest, as guests, and whether it all reads as
    /// addresses.
    pub fn typed_guests(&self) -> (Vec<Guest>, bool) {
        let typed: Vec<Address> = recipients::split_pasted(self.guest_input.trim())
            .iter()
            .map(|text| recipients::parse(text))
            .collect();
        let valid = typed
            .iter()
            .all(|address| recipients::is_valid(&address.email));
        (
            typed
                .into_iter()
                .map(|address| Guest {
                    email: address.email,
                    name: address.name,
                    optional: false,
                    response: None,
                    me: false,
                    organizer: false,
                })
                .collect(),
            valid,
        )
    }

    /// Takes what's typed in Who as guests if it's valid, and marks the field otherwise. Returns
    /// whether it was valid.
    pub fn commit_guests(&mut self) -> bool {
        if self.guest_input.trim().is_empty() {
            return true;
        }
        let (typed, valid) = self.typed_guests();
        if !valid || self.values.guests.len() >= 200 {
            self.guest_error = true;
            return false;
        }
        for guest in typed {
            if !self
                .values
                .guests
                .iter()
                .any(|known| known.email.eq_ignore_ascii_case(&guest.email))
            {
                self.values.guests.push(guest);
            }
        }
        self.guest_input.clear();
        self.guest_error = false;
        true
    }

    /// Marks Who as holding a bad address and focuses it.
    pub fn mark_guests_invalid(&mut self) {
        self.guest_error = true;
        self.focus = Some(Field::Guests);
    }

    pub(super) fn guests_row_height(&self, pane: bool) -> f32 {
        let lines = self.shown_guests(pane).len() + usize::from(self.hidden_guests(pane) > 0);
        ROW + if lines == 0 {
            0.0
        } else {
            GUESTS_TOP + lines as f32 * (GUEST + GUEST_GAP) - GUEST_GAP
        }
    }

    fn visible_guests(&self) -> Vec<(usize, &Guest)> {
        self.values
            .guests
            .iter()
            .enumerate()
            .filter(|(_, guest)| !guest.me)
            .collect()
    }

    fn shown_guests(&self, pane: bool) -> Vec<(usize, &Guest)> {
        let mut visible = self.visible_guests();
        if pane && self.focused != Some(Field::Guests) {
            visible.truncate(PANE_GUESTS);
        }
        visible
    }

    fn hidden_guests(&self, pane: bool) -> usize {
        self.visible_guests().len() - self.shown_guests(pane).len()
    }

    /// Who: the input, the guests under it, suggestions while typing. Returns whether it has the
    /// keyboard.
    pub(super) fn guests_row(
        &mut self,
        ui: &mut Ui,
        value: Rect,
        focus: bool,
        row: &mut RowContext<'_, '_>,
    ) -> bool {
        let shown = row.shown;
        let pane = row.pane;
        let palette = exo_ui::palette(ui.ctx());
        let id = Field::Guests.id(ui.ctx());
        let keys = ContactKeySnapshot::capture(ui, id);
        let backspace = ui.input(|input| input.key_pressed(egui::Key::Backspace));
        let before = self.guest_input.clone();
        let response = input(
            ui,
            id,
            &mut self.guest_input,
            value,
            InputSpec {
                style: &SMALL,
                placeholder: "Enter name or email",
                error: self.guest_error,
                focus,
                spellcheck: false,
            },
        );
        if self.guest_input != before {
            self.guest_error = false;
            self.suggestion = 0;
            self.chip = None;
        }
        let suggestions: Vec<Suggestion> = match shown.contacts {
            Some(book) if !self.guest_input.trim().is_empty() => {
                let limit = if self.mode == Mode::Edit { 2 } else { 3 };
                book.top(self.guest_input.trim(), limit + self.values.guests.len())
                    .into_iter()
                    .filter(|contact| {
                        !contact.email.eq_ignore_ascii_case(shown.me)
                            && !self
                                .values
                                .guests
                                .iter()
                                .any(|guest| guest.email.eq_ignore_ascii_case(&contact.email))
                    })
                    .take(limit)
                    .cloned()
                    .map(Suggestion::Contact)
                    .collect()
            }
            // An empty field for a new event's first guest suggests the conversation's people, up
            // to 200.
            _ if self.guest_input.is_empty()
                && self.values.guests.is_empty()
                && self.mode == Mode::Create
                && (1..=200).contains(&shown.conversation.len()) =>
            {
                vec![Suggestion::Everybody(shown.conversation.to_vec())]
            }
            _ => Vec::new(),
        };
        let guests: Vec<usize> = self
            .visible_guests()
            .iter()
            .map(|(index, _)| *index)
            .collect();
        if keys.had_focus {
            if let Some(chip) = self.chip {
                // ↑/↓ between the guests and the input; Backspace removes, Enter edits.
                if keys.up {
                    self.chip = Some(chip.saturating_sub(1));
                } else if keys.down {
                    self.chip = (chip + 1 < guests.len()).then_some(chip + 1);
                } else if backspace && self.guest_input.is_empty() {
                    if let Some(&index) = guests.get(chip) {
                        self.values.guests.remove(index);
                    }
                    self.chip = None;
                } else if keys.enter
                    && let Some(&index) = guests.get(chip)
                {
                    let guest = self.values.guests.remove(index);
                    self.guest_input = guest.email;
                    self.chip = None;
                }
            } else if keys.up
                && suggestions.is_empty()
                && self.guest_input.is_empty()
                && !guests.is_empty()
            {
                self.chip = Some(guests.len() - 1);
            } else if let Some(outcome) = keys.apply(
                &self.guest_input,
                suggestions.len(),
                &mut self.suggestion,
                ContactAcceptKeys::EnterAndTab,
                true,
            ) {
                match outcome {
                    ContactSearchOutcome::Picked(index) => {
                        self.values.guests.extend(suggestions[index].guests());
                        self.guest_input.clear();
                        self.focus = Some(Field::Guests);
                    }
                    ContactSearchOutcome::SubmittedEmpty { by_enter } => {
                        if by_enter {
                            self.focus = self.next(Field::Guests);
                        }
                    }
                    ContactSearchOutcome::SubmittedTyped { .. } => {
                        if self.commit_guests() {
                            self.focus = Some(Field::Guests);
                        }
                    }
                }
            }
        }
        if response.has_focus() && !suggestions.is_empty() && self.chip.is_none() {
            row.overlay = Some(Overlay::Suggestions {
                below: value,
                items: suggestions,
                form: row.form,
                pane,
            });
        }

        let shown_guests: Vec<(usize, Guest)> = self
            .shown_guests(pane)
            .into_iter()
            .map(|(index, guest)| (index, guest.clone()))
            .collect();
        // From the input's 20px box (`value` is its 16px line).
        let mut top = value.top() + ROW + GUESTS_TOP;
        let mut remove = None;
        let mut toggle = None;
        let mut tip = None;
        let carbon = exo_ui::theme(ui.ctx()) == exo_ui::Theme::Carbon;
        for (position, (index, guest)) in shown_guests.iter().enumerate() {
            let line = Rect::from_min_size(
                pos2(value.left(), top),
                vec2(value.width().min(GUEST_MAX), GUEST),
            );
            let hit = ui.interact(
                line,
                exo_ui::id(ui.ctx(), ("calendar-form-guest", index)),
                Sense::click(),
            );
            let hovered = ui.rect_contains_pointer(line);
            let highlighted = self.chip == Some(position) && response.has_focus();
            let content =
                Rect::from_min_max(pos2(line.left() + AVATAR + 8.0, line.top()), line.max);
            let border = if highlighted {
                Some(GUEST_FOCUS)
            } else if ui.rect_contains_pointer(content) {
                Some(if carbon {
                    GUEST_HOVER_CARBON
                } else {
                    Color32::from_black_alpha(23)
                })
            } else {
                None
            };
            if let Some(border) = border {
                ui.painter().rect_stroke(
                    exo_ui::pixel_snap(content),
                    CornerRadius::same(4),
                    Stroke::new(1.0, border),
                    egui::StrokeKind::Inside,
                );
            }
            // A click puts the keyboard on the guest.
            if hit.clicked() {
                self.chip = Some(position);
                ui.memory_mut(|memory| memory.request_focus(id));
            }
            // The guest's avatar, with their answer's badge in an event's form.
            paint_avatar(
                ui.painter(),
                pos2(line.left(), line.top() + 2.0),
                &guest.email,
                Rsvp::parse_opt(guest.response.as_deref()),
                palette,
            );
            let text = guest.name.clone().unwrap_or_else(|| guest.email.clone());
            // Their calendar's swatch, or its spinner, inside the content's border and padding.
            let swatch_at = content.left() + 8.0;
            let (marker, busy) = shown
                .chips
                .get(&guest.email)
                .copied()
                .unwrap_or((Marker::Empty, None));
            paint_marker(
                ui,
                Rect::from_min_size(pos2(swatch_at, line.center().y - 4.0), Vec2::splat(SWATCH)),
                marker,
            );
            let name_at = pos2(swatch_at + SWATCH + 8.0, line.top() + 5.0);
            let toggle_box = Rect::from_min_size(
                pos2(content.right() - 2.0 * GUEST_BUTTON + 4.0, content.top()),
                Vec2::splat(GUEST_BUTTON),
            );
            let cross_box = Rect::from_min_size(
                pos2(content.right() - GUEST_BUTTON, content.top()),
                Vec2::splat(GUEST_BUTTON),
            );
            // `(optional)`, else `(busy)` / `(OOO)`, at `.35` 4px after the name. The name gives
            // way to the note (`flex-shrink: 0`) and is cut before the toggle.
            let note = if guest.optional {
                Some("(optional)")
            } else {
                busy
            };
            let note_width = note.map_or(0.0, |note| 4.0 + SMALL.width(ui.ctx(), note));
            let used = SMALL.paint_line(
                ui.painter(),
                name_at,
                &text,
                palette.text_base_default,
                Some((toggle_box.left() - name_at.x - note_width).max(0.0)),
            );
            if let Some(note) = note {
                SMALL.paint_line(
                    ui.painter(),
                    name_at + vec2(used + 4.0, 0.0),
                    note,
                    palette.text_base_subdued,
                    None,
                );
            }
            // The swatch, name and note carry `Name <email>`'s tooltip.
            let target = Rect::from_min_max(
                pos2(swatch_at, line.top() + 4.0),
                pos2(
                    name_at.x + used + note_width,
                    line.top() + 4.0 + SMALL.line_height,
                ),
            );
            if ui.rect_contains_pointer(target) {
                let shown = guest.name.as_ref().map_or_else(
                    || guest.email.clone(),
                    |name| format!("{name} <{}>", guest.email),
                );
                tip = Some(exo_ui::CopyTip {
                    target,
                    shown,
                    copied: guest.email.clone(),
                });
            }
            if hovered {
                // Required guests show the filled user, optional ones the outline.
                let (icon, label) = if guest.optional {
                    ("origin/User", "Mark Required")
                } else {
                    ("origin/UserFill", "Mark Optional")
                };
                let marked = exo_ui::IconButton::new(icon)
                    .colors(GUEST_ICON, GUEST_ICON)
                    .show(
                        ui,
                        exo_ui::id(ui.ctx(), ("calendar-form-guest-optional", index)),
                        toggle_box,
                        Rect::from_center_size(toggle_box.center(), Vec2::splat(16.0)),
                    );
                KeyTooltip {
                    text: label,
                    keys: &[],
                }
                .show_below(ui, &marked, 8.0);
                if marked.clicked() {
                    toggle = Some(*index);
                }
                // The guest's cross.
                let removed = exo_ui::IconButton::new("legacy/DEPRECATED_closeV2")
                    .colors(GUEST_ICON, GUEST_ICON)
                    .show(
                        ui,
                        exo_ui::id(ui.ctx(), ("calendar-form-guest-remove", index)),
                        cross_box,
                        Rect::from_center_size(cross_box.center(), Vec2::splat(8.0)),
                    );
                KeyTooltip {
                    text: "Remove",
                    keys: &[],
                }
                .show_below(ui, &removed, 8.0);
                if removed.clicked() {
                    remove = Some(*index);
                }
            }
            top += GUEST + GUEST_GAP;
        }
        if let Some((click, tip)) =
            self.chip_tip
                .show(ui, exo_ui::id(ui.ctx(), "calendar-form-guest-tip"), tip)
        {
            self.tip_clicked = Some(match click {
                exo_ui::CopyClick::Text => {
                    let guest = shown_guests
                        .iter()
                        .find(|(_, guest)| guest.email == tip.copied)
                        .map(|(_, guest)| guest);
                    TipClick::Compose(
                        Address {
                            name: guest.and_then(|guest| guest.name.clone()),
                            email: tip.copied,
                        },
                        ui.input(|input| input.modifiers.shift_only()),
                    )
                }
                exo_ui::CopyClick::Icon => TipClick::Copy(tip.copied),
            });
        }
        let hidden = self.hidden_guests(pane);
        if hidden > 0 {
            SMALL.paint_line(
                ui.painter(),
                pos2(value.left(), top + 4.0),
                &format!("+ {hidden} more"),
                palette.text_base_subdued,
                None,
            );
        }
        if let Some(index) = remove {
            self.values.guests.remove(index);
        }
        if let Some(index) = toggle {
            self.values.guests[index].optional = !self.values.guests[index].optional;
        }
        response.has_focus()
    }
}

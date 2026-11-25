//! The contact picker, a field that suggests people (and domains) from the address book as you
//! type and adds the one picked. The suggestions show in a contact list under it that is Snow in
//! either theme. With nothing to suggest, an address typed in full shows as its own row. A `+` at
//! its right adds what's typed. Split Inbox's VIP definition uses it, without the picker's focus
//! bar.

use egui::{Color32, CornerRadius, Id, Rect, Sense, Ui, pos2, vec2};
use exo_ui::{
    ContactColumns, ContactListStyle, ContactRow, ContactSearchInput, ContactSelectorList,
    DialogInput, Family, Font, RowAvatar, TextStyle, step_list_index,
};

pub(crate) use crate::compose::recipients::is_valid_domain;
use crate::compose::{
    contacts::{self, Suggestion},
    recipients,
};

/// A row's name and address: 12px on the 28px line.
const ROW_TEXT: TextStyle = TextStyle::new(Font::new(Family::Sans, 400), 12.0, 28.0);
/// 36px rows, with the avatar cell 36px, the name 172px and the address the rest of the row, each
/// padded 16px a side.
const ROW: f32 = 36.0;
const AVATAR_CELL: f32 = 36.0;
const NAME_CELL: f32 = 172.0;
const CELL_PADDING: f32 = 16.0;
/// The list: 8px under the field, 8px wider each side.
const LIST_GAP: f32 = 8.0;
const LIST_OUTSET: f32 = 8.0;
/// The field's text 3px low, and its 1px rule.
const NUDGE: f32 = 3.0;
const RULE: f32 = 1.0;
/// The add button's plus: 16px, its circle `#aeb1dd`.
const ADD: f32 = 16.0;
const PERIWINKLE: Color32 = Color32::from_rgb(0xae, 0xb1, 0xdd);

/// A key the picker takes while it has the keyboard.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum PickerKey {
    /// ↓ / ↑ through the suggestions.
    Move(isize),
    /// Enter or Tab: the highlighted suggestion, or the address typed.
    Select,
    /// Space: the address typed, when there are no suggestions.
    Space,
    /// Escape with text, which clears it.
    Clear,
}

/// The picker's state.
#[derive(Debug, Default)]
pub(crate) struct ContactPicker {
    text: String,
    /// The text the suggestions are for.
    query: String,
    suggestions: Vec<Suggestion>,
    highlight: usize,
    /// Whether the typed text shows as its own row, as it does without suggestions. That row adds
    /// it as an address.
    typed_row: bool,
    valid: bool,
    /// Whether it had the keyboard last frame.
    focused: bool,
    /// Whether it asks for the keyboard next frame.
    focus: bool,
}

impl ContactPicker {
    /// Asks for the keyboard.
    pub fn request_focus(&mut self) {
        self.focus = true;
    }

    /// Whether it had the keyboard last frame.
    pub fn is_focused(&self) -> bool {
        self.focused
    }

    /// What's currently typed in the picker.
    pub fn text(&self) -> &str {
        &self.text
    }

    #[cfg(test)]
    pub(crate) fn set_text(&mut self, text: impl Into<String>) {
        self.text = text.into();
        self.query.clone_from(&self.text);
        self.suggest(None);
    }

    /// Whether the picker has non-whitespace text not yet added.
    pub fn has_pending(&self) -> bool {
        !self.text.trim().is_empty()
    }

    /// Commits pending text (or the highlighted suggestion, as `+` does). Returns `Ok(None)` when
    /// empty, `Ok(Some(sender))` when added, or `Err(())` when non-empty and invalid.
    pub fn commit_pending(&mut self) -> Result<Option<String>, ()> {
        if !self.has_pending() {
            return Ok(None);
        }
        self.add_button().map(Some).ok_or(())
    }

    /// The keys it takes now, besides typing, as (command id, key, bindings): ↓ / ↑ and Enter /
    /// Tab over suggestions; Enter, Tab and Space for an address typed in full; Escape while
    /// there's text.
    pub fn keys(&self) -> Vec<(&'static str, PickerKey, &'static [&'static str])> {
        let mut keys = Vec::new();
        if !self.suggestions.is_empty() {
            keys.extend([
                ("CONTACT_PICKER_DOWN", PickerKey::Move(1), &["down"][..]),
                ("CONTACT_PICKER_UP", PickerKey::Move(-1), &["up"][..]),
                (
                    "CONTACT_PICKER_SELECT",
                    PickerKey::Select,
                    &["enter", "tab"][..],
                ),
            ]);
        } else if self.valid {
            keys.extend([
                (
                    "CONTACT_PICKER_SELECT",
                    PickerKey::Select,
                    &["enter", "tab"][..],
                ),
                ("CONTACT_PICKER_SPACE", PickerKey::Space, &["space"][..]),
            ]);
        }
        if !self.text.is_empty() {
            keys.push(("CONTACT_PICKER_CLEAR", PickerKey::Clear, &["esc"][..]));
        }
        keys
    }

    /// Carries out `key`. Returns the sender it picked.
    pub fn key(&mut self, key: PickerKey) -> Option<String> {
        match key {
            PickerKey::Move(delta) => {
                self.highlight =
                    step_list_index(self.highlight, self.suggestions.len().max(1), delta, true);
                None
            }
            PickerKey::Select | PickerKey::Space if self.suggestions.is_empty() => {
                let picked = self.take_typed();
                if picked.is_some() {
                    self.focus = true;
                }
                picked
            }
            PickerKey::Select => {
                let picked = self.take(self.highlight);
                if picked.is_some() {
                    self.focus = true;
                }
                picked
            }
            PickerKey::Space => None,
            PickerKey::Clear => {
                self.clear();
                // Keep focus on the field after Escape clears its text (egui surrenders focus on
                // Escape during begin_pass when focus_lock_filter.escape is false).
                self.focus = true;
                None
            }
        }
    }

    /// The field (`input`) on `line` (its 20px line box, nudged down) with its `+` and list.
    /// Returns the sender picked with the pointer, an address or a domain.
    pub fn show(
        &mut self,
        ui: &mut Ui,
        id: Id,
        input: DialogInput<'_>,
        line: Rect,
        book: Option<&contacts::Index>,
    ) -> Option<String> {
        let ctx = ui.ctx().clone();
        let request_focus = std::mem::take(&mut self.focus);
        let highlight = &mut self.highlight;
        let field = input.show_with(
            ui,
            id,
            &mut self.text,
            line,
            |ui, id, text, line, style, color, selection| {
                ContactSearchInput::new(id, line, style, color, selection)
                    .request_focus(request_focus)
                    .show_input_with(ui, text, highlight, crate::spelling::text_field)
                    .response
            },
        );
        self.focused = field.has_focus();
        if self.text != self.query {
            self.query.clone_from(&self.text);
            self.suggest(book);
        }
        let mut picked = None;
        let container = Rect::from_min_size(
            pos2(line.left(), line.top() - NUDGE),
            vec2(line.width(), line.height() + RULE),
        );
        if !self.text.trim().is_empty() {
            let add = Rect::from_center_size(
                pos2(container.right() - ADD / 2.0, container.center().y),
                vec2(ADD, ADD),
            );
            let response = ui
                .interact(add, id.with("add"), Sense::click())
                .on_hover_cursor(egui::CursorIcon::PointingHand);
            // Classic's hover glow, `0 0 6px 0 #aeb1dd, 0 2px 12px 0 rgba(174,177,221,.5)`.
            let glow = exo_ui::motion::transition(
                &ctx,
                id.with("add-glow"),
                response.hovered(),
                0.1,
                exo_ui::motion::CubicBezier::EASE,
            );
            if glow > 0.0 {
                let round = CornerRadius::same(8);
                exo_ui::BoxShadow::new([0.0, 0.0], 6.0, 0.0, PERIWINKLE)
                    .with_opacity(glow)
                    .paint(ui.painter(), add, round);
                exo_ui::BoxShadow::new([0.0, 2.0], 12.0, 0.0, PERIWINKLE.gamma_multiply(0.5))
                    .with_opacity(glow)
                    .paint(ui.painter(), add, round);
            }
            // `legacy/DEPRECATED_plusCircle` is two colors (its circle tinted, a white plus across
            // the middle 8px), which one tint can't draw.
            let add = exo_ui::pixel_snap(add);
            let painter = ui.painter();
            painter.circle_filled(add.center(), ADD / 2.0, PERIWINKLE);
            let plus = egui::Stroke::new(1.0, Color32::WHITE);
            painter.line_segment(
                [
                    pos2(add.center().x, add.top() + 4.0),
                    pos2(add.center().x, add.bottom() - 4.0),
                ],
                plus,
            );
            painter.line_segment(
                [
                    pos2(add.left() + 4.0, add.center().y),
                    pos2(add.right() - 4.0, add.center().y),
                ],
                plus,
            );
            if response.clicked() {
                picked = self.add_button();
                self.focus = true;
            }
        }
        let at = pos2(
            container.left() - LIST_OUTSET,
            line.bottom() + RULE + LIST_GAP,
        );
        let width = container.width() + 2.0 * LIST_OUTSET;
        if let Some(row) = self.list(&ctx, id.with("list"), at, width) {
            picked = row;
            self.focus = true;
        }
        picked
    }

    /// The list, if any, of the suggestions or the typed address's row. Returns `Some` when a row
    /// was pressed (with what it picked).
    fn list(
        &mut self,
        ctx: &egui::Context,
        id: Id,
        at: egui::Pos2,
        width: f32,
    ) -> Option<Option<String>> {
        let avatar = |name: &str| RowAvatar {
            initial: name
                .chars()
                .next()
                .map(|c| c.to_uppercase().to_string())
                .unwrap_or_default(),
            color: crate::accounts::default_color(name),
        };
        let (rows, query, highlight, columns) = if !self.suggestions.is_empty() {
            let rows: Vec<ContactRow<'_>> = self
                .suggestions
                .iter()
                .map(|suggestion| match suggestion {
                    Suggestion::Contact(contact) => {
                        let name = contact
                            .name
                            .as_deref()
                            .filter(|name| !name.is_empty())
                            .unwrap_or(&contact.email);
                        ContactRow {
                            name,
                            email: &contact.email,
                            disabled: false,
                            avatar: Some(avatar(name)),
                        }
                    }
                    // A domain's row has no avatar.
                    Suggestion::Domain(domain) => ContactRow {
                        name: domain,
                        email: "",
                        disabled: false,
                        avatar: None,
                    },
                })
                .collect();
            let columns = ContactColumns {
                avatar: Some(CELL_PADDING),
                name: (AVATAR_CELL + CELL_PADDING, NAME_CELL - 2.0 * CELL_PADDING),
                email: (
                    AVATAR_CELL + NAME_CELL,
                    width - AVATAR_CELL - NAME_CELL - CELL_PADDING,
                ),
            };
            (rows, self.text.as_str(), Some(self.highlight), columns)
        } else if self.typed_row && !self.text.is_empty() {
            // The typed row has the text in a name cell as wide as the row, dark and filled once
            // it's a whole address. Its avatar's color is its first letter's.
            let first: String = self.text.chars().take(1).collect();
            let rows = vec![ContactRow {
                name: &self.text,
                email: "",
                disabled: false,
                avatar: Some(avatar(&first)),
            }];
            let columns = ContactColumns {
                avatar: Some(CELL_PADDING),
                name: (
                    AVATAR_CELL + CELL_PADDING,
                    width - AVATAR_CELL - 2.0 * CELL_PADDING,
                ),
                email: (width, 0.0),
            };
            (
                rows,
                if self.valid { self.text.as_str() } else { "" },
                self.valid.then_some(0),
                columns,
            )
        } else {
            return None;
        };
        let style = ContactListStyle {
            text: ROW_TEXT,
            row: ROW,
            padding: 0.0,
            columns,
            force_snow: true,
        };
        let response = ContactSelectorList::new(&rows, query, highlight, style).show(
            ctx,
            id,
            at,
            width,
            egui::Order::Tooltip,
        );
        if let Some(hovered) = response.hovered.filter(|_| !self.suggestions.is_empty()) {
            self.highlight = hovered;
        }
        let pressed = response.pressed?;
        Some(if self.suggestions.is_empty() {
            self.take_typed()
        } else {
            self.take(pressed)
        })
    }

    /// The suggestions for the text. With none, a domain typed in full is one, and an address typed
    /// (or its start) shows as its own row.
    fn suggest(&mut self, book: Option<&contacts::Index>) {
        self.highlight = 0;
        let text = self.text.as_str();
        self.suggestions = if text.is_empty() {
            Vec::new()
        } else {
            book.map(|book| contacts::suggest_with_domains(book, text))
                .unwrap_or_default()
        };
        (self.valid, self.typed_row) = (false, false);
        if text.is_empty() || !self.suggestions.is_empty() {
            return;
        }
        if is_valid_domain(text) {
            self.suggestions = vec![Suggestion::Domain(text.to_owned())];
            return;
        }
        self.valid = recipients::is_valid(text);
        self.typed_row = self.valid || recipients::is_address_start(text);
    }

    /// Takes the sender of suggestion `index`, and the field empties.
    fn take(&mut self, index: usize) -> Option<String> {
        let sender = match self.suggestions.get(index)? {
            Suggestion::Contact(contact) => contact.email.clone(),
            Suggestion::Domain(domain) if is_valid_domain(domain) => domain.clone(),
            Suggestion::Domain(_) => return None,
        };
        self.clear();
        Some(sender)
    }

    /// Takes the address typed, when it's one, and the field empties.
    fn take_typed(&mut self) -> Option<String> {
        if !self.valid {
            return None;
        }
        let sender = self.text.trim().to_owned();
        self.clear();
        Some(sender)
    }

    /// What the `+` adds: the highlighted suggestion, or else the domain or address typed.
    fn add_button(&mut self) -> Option<String> {
        if !self.suggestions.is_empty() {
            return self.take(self.highlight);
        }
        let typed = self.text.trim().to_owned();
        if is_valid_domain(&typed) || recipients::is_valid(&typed) {
            self.clear();
            return Some(typed);
        }
        None
    }

    /// Empties the field and its suggestions.
    fn clear(&mut self) {
        self.text.clear();
        self.query.clear();
        self.suggestions.clear();
        (self.valid, self.typed_row, self.highlight) = (false, false, 0);
    }
}

#[cfg(test)]
mod tests {
    use exo_store::ScoredContact;

    use super::*;

    fn book() -> contacts::Index {
        contacts::Index::book(&[
            ScoredContact {
                email: "priya@brightmoor.co".into(),
                name: Some("Priya Raman".into()),
                score: 1.5,
            },
            ScoredContact {
                email: "tom@brightmoor.co".into(),
                name: Some("Tom Becker".into()),
                score: 1.2,
            },
        ])
    }

    fn typed(text: &str) -> ContactPicker {
        let mut picker = ContactPicker {
            text: text.to_owned(),
            ..ContactPicker::default()
        };
        picker.suggest(Some(&book()));
        picker
    }

    #[test]
    fn domains_are_dotted_hostnames() {
        assert!(is_valid_domain("brightmoor.co"));
        assert!(is_valid_domain("Mail.Example-1.com"));
        assert!(!is_valid_domain("brightmoor"));
        assert!(!is_valid_domain("-a.com"));
        assert!(!is_valid_domain("ana@a.co"));
    }

    #[test]
    fn enter_takes_the_highlighted_suggestion_or_a_whole_address() {
        let mut picker = typed("brightmoor");
        assert_eq!(picker.key(PickerKey::Move(1)), None);
        assert_eq!(
            picker.key(PickerKey::Select).as_deref(),
            Some("tom@brightmoor.co")
        );
        assert!(
            picker.text.is_empty() && picker.suggestions.is_empty(),
            "the field empties"
        );
        assert_eq!(
            typed("brightmoor").key(PickerKey::Select).as_deref(),
            Some("brightmoor.co"),
            "the domain comes first"
        );
        assert_eq!(
            typed("ana@x.org").key(PickerKey::Space).as_deref(),
            Some("ana@x.org")
        );
        assert_eq!(
            typed("ana@x").key(PickerKey::Select),
            None,
            "not an address yet"
        );
    }

    #[test]
    fn without_suggestions_a_domain_is_one_and_an_address_its_own_row() {
        assert_eq!(
            typed("acme.io").suggestions,
            [Suggestion::Domain("acme.io".into())]
        );
        let start = typed("ana@");
        assert!(
            start.typed_row
                && !start.valid
                && start
                    .keys()
                    .iter()
                    .all(|(_, key, _)| *key == PickerKey::Clear)
        );
        let words = typed("Ana Smith");
        assert!(!words.typed_row, "not the start of an address");
        assert_eq!(
            typed("ana@x.org").keys().len(),
            3,
            "Enter/Tab, Space and Escape"
        );
    }

    #[test]
    fn commit_pending_and_escape_clear_preserve_picker_behavior() {
        let mut empty = ContactPicker::default();
        assert!(!empty.has_pending());
        assert_eq!(empty.commit_pending(), Ok(None));

        let mut invalid = typed("nope");
        assert!(invalid.has_pending());
        assert_eq!(invalid.commit_pending(), Err(()));

        let mut valid = typed("ana@x.org");
        assert_eq!(valid.commit_pending(), Ok(Some("ana@x.org".into())));
        assert!(!valid.has_pending());

        // Escape clearing text via PickerKey::Clear requests focus back on the same frame so
        // egui's begin_pass focus surrender doesn't lose keyboard focus.
        let ctx = egui::Context::default();
        exo_ui::install(&ctx).expect("fonts");
        let id = Id::new("vip-picker-escape");
        let line = Rect::from_min_size(pos2(20.0, 20.0), vec2(260.0, 20.0));
        let style = TextStyle::new(Font::new(Family::Sans, 400), 12.0, 20.0);
        let mut picker = ContactPicker::default();
        picker.request_focus();

        // Frame 1: gain focus and type "nope".
        ctx.run_ui(egui::RawInput::default(), |ui| {
            let _ = picker.show(
                ui,
                id,
                DialogInput::new(style, "Enter name"),
                line,
                Some(&book()),
            );
        })
        .drop_without_applying_deltas();
        assert!(picker.is_focused());
        picker.text = "nope".into();
        picker.suggest(Some(&book()));

        // Frame 2: Escape hits begin_pass (which drops focus because escape filter is false),
        // then exo_commands invokes PickerKey::Clear before show(). Focus must stay on picker.
        ctx.run_ui(
            egui::RawInput {
                events: vec![egui::Event::Key {
                    key: egui::Key::Escape,
                    physical_key: None,
                    pressed: true,
                    repeat: false,
                    modifiers: egui::Modifiers::NONE,
                }],
                ..Default::default()
            },
            |ui| {
                assert_eq!(picker.key(PickerKey::Clear), None);
                let _ = picker.show(
                    ui,
                    id,
                    DialogInput::new(style, "Enter name"),
                    line,
                    Some(&book()),
                );
            },
        )
        .drop_without_applying_deltas();
        assert!(picker.text().is_empty());
        assert!(
            picker.is_focused(),
            "clearing text with Escape keeps focus in the picker"
        );
    }
}

//! The Timezone input and its search: every place, abbreviation and zone of the time parser's
//! tables under a search key, the common ones first, matched by prefix.

use std::{collections::HashMap, sync::OnceLock};

use egui::{Color32, Id, Rect, Ui, pos2, vec2};
use exo_ui::{Family, Font, TextStyle};
use jiff::{Timestamp, tz::TimeZone};
use unicode_normalization::UnicodeNormalization as _;

use crate::time_suggest::zone_tables;

/// A zone the search found.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Found {
    pub iana: String,
    /// `New York`.
    pub name: String,
    /// `GMT+00:00`, the zone's own offset rather than the one Exo is in.
    pub offset: String,
    /// `EST`, or a bare offset written as a GMT offset (`GMT+05:30`).
    pub abbreviation: String,
}

impl Found {
    /// `iana`, as the input shows it at `at`.
    pub fn new(iana: &str, name: String, at: Timestamp) -> Option<Self> {
        let tz = TimeZone::get(iana).ok()?;
        Some(Self {
            iana: iana.to_owned(),
            name,
            offset: offset(&tz, at),
            abbreviation: super::availability::abbreviation(&tz, at),
        })
    }

    /// The input's text for it: the offset padded to 16 characters, then the name.
    pub fn text(&self) -> String {
        format!("{:<16}{}", self.offset, self.name)
    }
}

/// The zone `iana` as the input shows it at `at`, named by its last segment.
pub fn current(iana: &str, at: Timestamp) -> Option<Found> {
    Found::new(iana, last_segment(iana), at)
}

/// The first `limit` zones whose search key starts with `text`'s, the common ones first.
pub fn search(text: &str, at: Timestamp, limit: usize) -> Vec<Found> {
    let key = search_key(text);
    let (keys, entries) = index();
    keys.iter()
        .filter(|candidate| candidate.starts_with(&key))
        .filter_map(|candidate| entries.get(candidate))
        .filter_map(|(iana, name)| Found::new(iana, name.clone(), at))
        .take(limit)
        .collect()
}

/// What the input lists when it takes the keyboard: `current` over the common zones, `limit` in
/// all.
pub fn suggested(current: Option<&Found>, at: Timestamp, limit: usize) -> Vec<Found> {
    let common = search("", at, limit);
    current
        .into_iter()
        .cloned()
        .chain(
            common
                .into_iter()
                .filter(|found| Some(&found.name) != current.map(|current| &current.name)),
        )
        .take(limit)
        .collect()
}

/// The input's text, 12px/16px over `padding-bottom: 2px` and a 2px rule.
const TEXT: TextStyle = TextStyle::new(Font::new(Family::Sans, 400), 12.0, 16.0);
const RULE: f32 = 2.0;
/// Its list hangs 16px left of its box and 4px under it by default.
pub const DEFAULT_LIST_OFFSET: egui::Vec2 = vec2(-16.0, 4.0);
/// `#aeb1dd`, a Classic literal both themes keep.
const RULE_INK: Color32 = Color32::from_rgb(0xae, 0xb1, 0xdd);

/// How a Timezone input's list lays out its rows: their height, the padding over the first and
/// under the last, where each row's 12px/16px text sits, and the list's Carbon fill.
#[derive(Clone, Copy)]
pub struct ZoneRows {
    pub row: f32,
    pub pad: f32,
    pub bottom: f32,
    pub text_top: f32,
    pub carbon: Color32,
}

impl ZoneRows {
    /// Share Availability's and Calendar Settings': 32px rows from 4px down, on `#27292d`.
    pub const SIDEBAR: Self = Self {
        row: 32.0,
        pad: 4.0,
        bottom: 5.0,
        text_top: 8.0,
        carbon: Color32::from_rgb(0x27, 0x29, 0x2d),
    };
    /// The event form's When (12px lines nudged `top: 3px`): 28px rows from 6px down, on the
    /// form's `#27292d`.
    pub const EDIT_EVENT: Self = Self {
        row: 28.0,
        pad: 6.0,
        bottom: 3.0,
        text_top: 6.0,
        carbon: Color32::from_rgb(0x27, 0x29, 0x2d),
    };
}

/// A Timezone input's list at `rect`: `rows` of a zone's offset (80px wide), name and, on the
/// right, abbreviation (left out when it's an offset). Returns the row under a moving pointer and
/// the row pressed.
pub fn zone_list(
    ui: &Ui,
    id: Id,
    rect: Rect,
    zones: &[Found],
    highlighted: usize,
    rows: &ZoneRows,
) -> (Option<usize>, Option<usize>) {
    let ctx = ui.ctx().clone();
    let palette = exo_ui::palette(&ctx);
    super::share::dropdown(
        ui,
        id,
        rect,
        Some(rows.carbon),
        zones.len(),
        rows.row,
        rows.pad,
        highlighted,
        |painter, row, index| {
            let zone = &zones[index];
            let line = row.top() + rows.text_top;
            TEXT.paint_line(
                painter,
                pos2(row.left() + 15.0, line),
                &zone.offset,
                palette.ink(0.4, 0.4),
                None,
            );
            TEXT.paint_line(
                painter,
                pos2(row.left() + 95.0, line),
                &zone.name,
                palette.ink(0.9, 0.9),
                Some(row.width() - 95.0 - 60.0),
            );
            if !zone.abbreviation.chars().any(|ch| ch.is_ascii_digit()) {
                let width = TEXT.width(&ctx, &zone.abbreviation);
                TEXT.paint_line(
                    painter,
                    pos2(row.right() - 15.0 - width, line),
                    &zone.abbreviation,
                    palette.ink(0.9, 0.9),
                    None,
                );
            }
        },
    )
}

/// A Timezone input's state: what's typed, and its list.
#[derive(Default)]
pub struct ZoneInput {
    text: String,
    typed: bool,
    zones: Vec<Found>,
    pick: usize,
}

/// How a Timezone input draws where it's used.
pub struct ZoneField<'a> {
    pub id: Id,
    /// The text's line box.
    pub line: Rect,
    /// The zone it shows, by IANA id.
    pub zone: Option<&'a str>,
    pub now: Timestamp,
    /// How many zones its list shows.
    pub limit: usize,
    /// The text's and the placeholder's colours.
    pub ink: Color32,
    pub placeholder: Color32,
    /// Whether `ZoneInput::show` paints its own 2px underline.
    pub show_rule: bool,
    /// Its rule always shows (in Calendar Settings), or only while it's focused.
    pub always_ruled: bool,
    /// A pick gives the keyboard up.
    pub blur_on_pick: bool,
    pub list_width: f32,
    /// Offset from `(field_box.left(), field_box.bottom())` to the list's top-left corner.
    pub list_offset: egui::Vec2,
    pub rows: ZoneRows,
}

/// What a Timezone input did this frame.
pub struct ZoneResponse {
    /// The zone picked, by IANA id.
    pub picked: Option<String>,
    pub focused: bool,
}

impl ZoneInput {
    /// Whether the suggestion dropdown currently holds items.
    #[must_use]
    pub fn has_suggestions(&self) -> bool {
        !self.zones.is_empty()
    }

    /// Resets typed query text (used when Escape clears edited text in Share Availability).
    pub fn clear_typed(&mut self) -> bool {
        if self.typed {
            self.typed = false;
            self.zones.clear();
            self.text.clear();
            true
        } else {
            false
        }
    }

    /// Paints the input: the zone (`GMT+00:00       London`), or typed text with the zones it
    /// finds, picked with the pointer or ↑/↓ and Enter or Tab.
    pub fn show(&mut self, ui: &mut Ui, field: &ZoneField<'_>) -> ZoneResponse {
        let palette = exo_ui::palette(ui.ctx());
        let id = field.id;
        let current = field.zone.and_then(|iana| current(iana, field.now));
        if !self.typed {
            self.text = current.as_ref().map(Found::text).unwrap_or_default();
        }
        let had_focus = ui.memory(|memory| memory.has_focus(id));
        let keys = ui.input(|input| {
            (
                input.key_pressed(egui::Key::ArrowUp),
                input.key_pressed(egui::Key::ArrowDown),
                input.key_pressed(egui::Key::Enter) || input.key_pressed(egui::Key::Tab),
            )
        });
        let length = self.text.chars().count();
        let response = exo_ui::text_field(
            ui,
            id,
            &mut self.text,
            field.line,
            &TEXT,
            field.ink,
            palette.text_selection_default,
        );
        if self.text.is_empty() && !response.has_focus() {
            TEXT.paint_line(
                ui.painter(),
                field.line.min,
                "Enter timezone or location",
                field.placeholder,
                Some(field.line.width()),
            );
        }
        let field_box = Rect::from_min_size(
            field.line.min,
            vec2(field.line.width(), TEXT.line_height + 2.0 + RULE),
        );
        if field.show_rule && (field.always_ruled || response.has_focus()) {
            ui.painter().rect_filled(
                Rect::from_min_max(
                    pos2(field_box.left(), field_box.bottom() - RULE),
                    field_box.max,
                ),
                0.0,
                RULE_INK,
            );
        }
        if response.gained_focus() {
            // With its text selected, the list shows what was typed, or else this zone over the
            // common ones.
            exo_ui::focus_field(
                ui.ctx(),
                id,
                egui::text::CCursorRange::two(
                    egui::text::CCursor::new(0),
                    egui::text::CCursor::new(length),
                ),
            );
            self.zones = if self.typed {
                search(&self.text, field.now, field.limit)
            } else {
                suggested(current.as_ref(), field.now, field.limit)
            };
            self.pick = 0;
        }
        if response.changed() {
            self.typed = true;
            self.zones = search(&self.text, field.now, field.limit);
            self.pick = 0;
        }
        let mut picked = None;
        if had_focus && !self.zones.is_empty() {
            match keys {
                (true, _, _) => self.pick = self.pick.saturating_sub(1),
                (_, true, _) => self.pick = (self.pick + 1).min(self.zones.len() - 1),
                (_, _, true) => picked = self.zones.get(self.pick).cloned(),
                _ => {}
            }
        }
        if response.has_focus() && !self.zones.is_empty() {
            let rows = &field.rows;
            let list = Rect::from_min_size(
                pos2(
                    field_box.left() + field.list_offset.x,
                    field_box.bottom() + field.list_offset.y,
                ),
                vec2(
                    field.list_width,
                    self.zones.len() as f32 * rows.row + rows.pad + rows.bottom,
                ),
            );
            let (hovered, clicked) =
                zone_list(ui, id.with("list"), list, &self.zones, self.pick, rows);
            self.pick = hovered.unwrap_or(self.pick);
            picked = picked.or_else(|| clicked.and_then(|index| self.zones.get(index).cloned()));
        }
        if response.lost_focus() && picked.is_none() {
            self.zones.clear();
            // Losing the keyboard, an emptied field takes the zone back.
            if self.text.trim().is_empty() {
                self.typed = false;
            }
        }
        let kept = picked.is_some() && !field.blur_on_pick;
        let picked = picked.map(|picked| {
            self.typed = false;
            self.zones.clear();
            if field.blur_on_pick {
                response.surrender_focus();
            } else {
                // Tab takes the pick and stays, as Enter does.
                ui.memory_mut(|memory| memory.request_focus(id));
            }
            picked.iana
        });
        ZoneResponse {
            picked,
            focused: response.has_focus() || kept,
        }
    }
}

/// The zone's offset at `at`: `GMT−05:00`, with a real minus sign.
fn offset(tz: &TimeZone, at: Timestamp) -> String {
    let seconds = tz.to_offset(at).seconds();
    let sign = if seconds < 0 { "−" } else { "+" };
    let minutes = seconds.abs() / 60;
    format!("GMT{sign}{:02}:{:02}", minutes / 60, minutes % 60)
}

/// A zone's name from its IANA id: `America/New_York` is `New York`.
fn last_segment(iana: &str) -> String {
    iana.rsplit('/').next().unwrap_or(iana).replace('_', " ")
}

/// A search key: lower-case ASCII letters, with diacritics stripped and Latin ligatures folded.
fn search_key(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for ch in text.chars().flat_map(char::to_lowercase).nfd() {
        match ch {
            'a'..='z' => out.push(ch),
            'ø' => out.push('o'),
            'æ' => out.push_str("ae"),
            'ß' => out.push_str("ss"),
            'œ' => out.push_str("oe"),
            'ł' => out.push('l'),
            'đ' | 'ð' => out.push('d'),
            'þ' => out.push_str("th"),
            'ı' => out.push('i'),
            _ => {}
        }
    }
    out
}

type Entry = (&'static str, String);

/// The search keys in their order and what each finds: places, then abbreviations (named by
/// their zone), places' short names (as the place), then every remaining zone by its last segment.
fn index() -> &'static (Vec<String>, HashMap<String, Entry>) {
    static INDEX: OnceLock<(Vec<String>, HashMap<String, Entry>)> = OnceLock::new();
    INDEX.get_or_init(|| {
        let (places, abbreviations, place_abbreviations, zones) = zone_tables();
        let mut entries: HashMap<String, Entry> = HashMap::new();
        for (name, iana) in places {
            entries.insert(search_key(name), (iana, (*name).to_owned()));
        }
        for (abbreviation, iana) in abbreviations
            .iter()
            .filter(|(_, iana)| !iana.starts_with("Etc/GMT"))
        {
            entries.insert(search_key(abbreviation), (iana, last_segment(iana)));
        }
        for (short, place) in place_abbreviations {
            if let Some(entry) = entries.get(&search_key(place)).cloned() {
                entries.insert(search_key(short), entry);
            }
        }
        let mut ordered_zones: Vec<&'static str> = zones
            .iter()
            .map(String::as_str)
            .filter(|iana| !iana.starts_with("Etc/GMT"))
            .collect();
        ordered_zones.sort_by(|a, b| {
            b.matches('/')
                .count()
                .cmp(&a.matches('/').count())
                .then_with(|| a.cmp(b))
        });
        for iana in ordered_zones {
            let name = last_segment(iana);
            entries.entry(search_key(&name)).or_insert((iana, name));
        }
        const FIRST: [&str; 10] = [
            "newyork",
            "losangeles",
            "chicago",
            "london",
            "denver",
            "sanfrancisco",
            "easterntime",
            "centraltime",
            "mountaintime",
            "pacifictime",
        ];
        let mut keys: Vec<String> = entries.keys().cloned().collect();
        keys.sort_by(|a, b| {
            let rank = |key: &String| {
                FIRST
                    .iter()
                    .position(|first| first == key)
                    .unwrap_or(FIRST.len())
            };
            rank(a).cmp(&rank(b)).then_with(|| a.cmp(b))
        });
        (keys, entries)
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn at() -> Timestamp {
        "2026-11-17T14:00:00Z".parse().expect("timestamp")
    }

    #[test]
    fn the_common_zones_come_first() {
        let names: Vec<String> = search("", at(), 4)
            .into_iter()
            .map(|found| found.name)
            .collect();
        assert_eq!(names, ["New York", "Los Angeles", "Chicago", "London"]);
    }

    #[test]
    fn a_prefix_finds_places_abbreviations_and_zones() {
        let found = search("lon", at(), 4);
        // Each with its own offset, not the one Exo is in.
        assert_eq!(
            found.first().map(|found| (
                found.iana.as_str(),
                found.text(),
                found.abbreviation.as_str()
            )),
            Some(("Europe/London", "GMT+00:00       London".to_owned(), "GMT"))
        );
        let pst = search("pst", at(), 4);
        assert_eq!(
            pst.first().map(|found| (
                found.iana.as_str(),
                found.name.as_str(),
                found.abbreviation.as_str()
            )),
            Some(("America/Los_Angeles", "Los Angeles", "PST"))
        );
        // Places' short names stand for the place.
        assert_eq!(
            search("nyc", at(), 1)
                .first()
                .map(|found| found.name.as_str()),
            Some("New York")
        );
    }

    #[test]
    fn the_input_writes_the_offset_padded() {
        let new_york = current("America/New_York", at()).expect("zone");
        assert_eq!(new_york.text(), "GMT−05:00       New York");
        assert_eq!(new_york.abbreviation, "EST");
    }

    #[test]
    fn plain_ascii_finds_accented_places() {
        for (query, expected) in [
            ("curacao", "Curaçao"),
            ("reunion", "Réunion"),
            ("aland", "Åland Islands"),
        ] {
            assert_eq!(
                search(query, at(), 1)
                    .first()
                    .map(|found| found.name.as_str()),
                Some(expected),
                "searching {query:?}"
            );
        }
        assert_eq!(search_key("Tromsø Ærø Straße"), "tromsoaerostrasse");
    }
}

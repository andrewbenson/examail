//! The alert, a small centered card with a bold title, text and its actions in a row at the bottom
//! right.
//!
//! The card is 340px wide, padded `12px 24px 20px 16px`, with 4px corners, and white in both
//! themes. The title and text are 14px/20px Exo Sans nudged down 3px, with the text 8px below the
//! title. The actions sit in a 28px row 8px below the text, floated right past the padding
//! (`margin-right: -12px`), with the second one 8px left of the first.

use egui::{Area, Color32, CornerRadius, Order, Rect, Sense, Ui, pos2, vec2};

use crate::{BoxShadow, Family, Font, TextStyle, WBR, form, motion, pixel_snap};

const WIDTH: f32 = 340.0;
/// The modal backdrop's `rgba(0, 0, 0, .5)`.
const BACKDROP: Color32 = Color32::from_rgba_premultiplied(0, 0, 0, 128);
const PADDING_TOP: f32 = 12.0;
const PADDING_RIGHT: f32 = 24.0;
const PADDING_BOTTOM: f32 = 20.0;
const PADDING_LEFT: f32 = 16.0;
/// The right padding of an alert without actions.
const PADDING_RIGHT_NO_ACTIONS: f32 = 16.0;
const RADIUS: u8 = 4;
const TITLE: TextStyle = TextStyle::new(Font::new(Family::Sans, 700), 14.0, 20.0);
const TEXT: TextStyle = ALERT_TEXT;
/// The text: 14px/20px Exo Sans.
pub const ALERT_TEXT: TextStyle = TextStyle::new(Font::new(Family::Sans, 400), 14.0, 20.0);
/// The text box's width in an alert with actions: the card's less its padding.
pub const ALERT_TEXT_WIDTH: f32 = WIDTH - PADDING_LEFT - PADDING_RIGHT;
/// The title, text and actions all carry `position: relative; top: 3px`.
const NUDGE: f32 = 3.0;
/// The text's margin above and below, and between its paragraphs.
const TEXT_MARGIN: f32 = ALERT_PARAGRAPH_GAP;
/// How far apart an alert's `<p>`s are.
pub const ALERT_PARAGRAPH_GAP: f32 = 8.0;
/// The action row's height.
const ACTIONS_HEIGHT: f32 = 28.0;
/// An action's padding, `8px 12px 4px 12px`, and its `margin-right: -12px`.
const ACTION_PADDING: [f32; 3] = [8.0, 12.0, 4.0];
/// The second action's gap before the first.
const ACTION_GAP: f32 = 8.0;
/// The title is `rgba(0, 0, 0, .9)` and the text `rgba(0, 0, 0, .75)`, in both themes.
const TITLE_INK: Color32 = Color32::from_rgba_premultiplied(0, 0, 0, 230);
const TEXT_INK: Color32 = ALERT_TEXT_INK;
/// The text's `rgba(0, 0, 0, .75)`.
pub const ALERT_TEXT_INK: Color32 = Color32::from_rgba_premultiplied(0, 0, 0, 191);
/// `0 15px 20px rgba(0,0,0,.04), 0 8px 20px rgba(0,0,0,.1)`.
const SHADOWS: [BoxShadow; 2] = [
    BoxShadow::new(
        [0.0, 15.0],
        20.0,
        0.0,
        Color32::from_rgba_premultiplied(0, 0, 0, 10),
    ),
    BoxShadow::new(
        [0.0, 8.0],
        20.0,
        0.0,
        Color32::from_rgba_premultiplied(0, 0, 0, 26),
    ),
];
/// A radio: 12px, 7px before its label.
const RADIO: f32 = 12.0;
const RADIO_GAP: f32 = 7.0;
/// A keycap in an alert's text, an 18px cap at least 18px wide, with `padding: 0 5px`,
/// `margin: 0 1px`, and 3px corners in `#abb7c5` (`#555b64` in Carbon). Its label is 12px semibold
/// (13px only on a 1× screen) in a 15px line, white (at `.8` in Carbon), 2.5px down the cap
/// (centered in its 12px content box, then `translateY(3px)`).
const KEY_HEIGHT: f32 = 18.0;
const KEY_PADDING: f32 = 5.0;
const KEY_FILL: Color32 = Color32::from_rgb(0xab, 0xb7, 0xc5);
const KEY_FILL_CARBON: Color32 = Color32::from_rgb(0x55, 0x5b, 0x64);
const KEY: Cap = Cap {
    min_width: 18.0,
    margin: 1.0,
    top: 0.0,
    label: TextStyle::new(Font::new(Family::Sans, 600), 12.0, 15.0),
    label_top: 2.5,
    shift: 0.0,
    snap: false,
};
/// The Undo Send alert's keycap, a plain inline keycap: as tall, at least 19px wide, with no
/// margin, `top: 1px`, and a bold label 2px down. Its line's box is a pixel taller, which takes
/// the line's text, and the text after it, a pixel down.
const CONFIRM_KEY: Cap = Cap {
    min_width: 19.0,
    margin: 0.0,
    top: 1.0,
    label: TextStyle::new(Font::new(Family::Sans, 700), 12.0, 15.0),
    label_top: 2.0,
    shift: 1.0,
    snap: true,
};

/// How a keycap in an alert's text is drawn.
struct Cap {
    min_width: f32,
    /// Its margin either side.
    margin: f32,
    /// How far it's down its line.
    top: f32,
    label: TextStyle,
    /// How far its label is down the cap.
    label_top: f32,
    /// How far its line, and the lines after it, are down from where they'd be without it.
    shift: f32,
    /// Its box starts on a whole CSS px, as Chromium paints an inline block's background.
    snap: bool,
}
/// Separates `<p>` paragraphs in [`Alert::text`].
pub const PARAGRAPH: char = '\u{2029}';
/// Bold text in an alert's text: `<strong>`, weight 700.
const STRONG: TextStyle = TextStyle::new(Font::new(Family::Sans, 700), 14.0, 20.0);
/// A link in an alert's text: `#54acdc` in both themes, not underlined.
const LINK_INK: Color32 = Color32::from_rgb(0x54, 0xac, 0xdc);

/// A run of an alert's text ([`Alert::show_runs`]): plain, bold, or a link. A `\n` is a `<br>`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AlertRun<'a> {
    Text(&'a str),
    Strong(&'a str),
    Link(&'a str),
}

impl AlertRun<'_> {
    /// The run's text.
    pub fn text(&self) -> &str {
        match self {
            Self::Text(text) | Self::Strong(text) | Self::Link(text) => text,
        }
    }

    fn style(&self) -> &'static TextStyle {
        match self {
            Self::Strong(_) => &STRONG,
            Self::Text(_) | Self::Link(_) => &TEXT,
        }
    }
}

/// What a click on an alert with runs did: an action, or a link by its place among the links.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AlertClick {
    Action(usize),
    Link(usize),
}

/// `text` for an address or domain in an alert, with a `<wbr>` before each word that follows a
/// word and punctuation, so `daniel@brightmoor.co` can break after its `@` and `.`. Chromium lays a
/// `<wbr>` out as a zero-width space ([`WBR`]), and its glyphs either side don't kern. The alert
/// does the same.
pub fn wbrise(text: &str) -> String {
    #[derive(Clone, Copy, PartialEq, Eq)]
    enum Kind {
        Whitespace,
        Punctuation,
        Other,
        Word,
    }
    const PUNCTUATION: &str = "!#$%&)*+,-./:=>?@\\]^_|}~";
    let is = |kind: Kind, c: char| match kind {
        Kind::Whitespace => c.is_whitespace(),
        Kind::Punctuation => PUNCTUATION.contains(c),
        Kind::Other => "({[\"'`".contains(c),
        Kind::Word => !c.is_whitespace() && !PUNCTUATION.contains(c),
    };
    let (mut pieces, mut current, mut kinds) = (Vec::new(), String::new(), Vec::<Kind>::new());
    let mut rest = text;
    while !rest.is_empty() {
        // Each round takes a run of each kind in turn.
        for kind in [Kind::Whitespace, Kind::Punctuation, Kind::Other, Kind::Word] {
            let end = rest.find(|c| !is(kind, c)).unwrap_or(rest.len());
            if end == 0 {
                continue;
            }
            let (run, after) = rest.split_at(end);
            if kind == Kind::Word && kinds.ends_with(&[Kind::Word, Kind::Punctuation]) {
                pieces.push(std::mem::replace(&mut current, run.to_owned()));
                kinds.push(kind);
            } else {
                current.push_str(run);
                if kinds.last() != Some(&kind) {
                    kinds.push(kind);
                }
            }
            rest = after;
        }
    }
    pieces.push(current);
    pieces.join(&WBR.to_string())
}

/// An alert with up to two actions, one of them selected (filled). Enter, Escape and the arrow
/// keys are the caller's to bind.
#[derive(Clone, Copy, Debug)]
pub struct Alert<'a> {
    /// The title (`Almost there…`).
    pub title: &'a str,
    /// The text. Each `\n` is a `<br>` (so a blank line is an empty line), and runs of spaces
    /// collapse, leading ones included. [`PARAGRAPH`] separates `<p>`s, which sit 8px apart.
    pub text: &'a str,
    /// The actions' labels, right to left, so the first is rightmost.
    pub actions: &'a [&'a str],
    /// The selected action (usually the first).
    pub selected: usize,
}

impl Alert<'_> {
    /// Moves `selected` by `step` across `count` right-to-left actions (`+1` moves visually left,
    /// `-1` moves visually right), wrapping with `rem_euclid` when `wrap` is set and clamping
    /// otherwise.
    pub fn move_selection(selected: usize, count: usize, step: isize, wrap: bool) -> usize {
        if count == 0 {
            return 0;
        }
        let next = selected as isize + step;
        let bound = count as isize;
        if wrap {
            next.rem_euclid(bound) as usize
        } else {
            next.clamp(0, bound - 1) as usize
        }
    }

    /// Draws `content` in a modal [`Order::Tooltip`] layer over `area` dimmed to `rgba(0,0,0,.5)`,
    /// swallowing clicks behind it. Returns `(inner, backdrop_clicked)`.
    pub fn modal<R>(
        ctx: &egui::Context,
        id: impl egui::AsId,
        area: Rect,
        content: impl FnOnce(&mut Ui) -> R,
    ) -> (R, bool) {
        let id = crate::id(ctx, ("exo_ui::alert", id));
        Area::new(id)
            .order(Order::Tooltip)
            .fixed_pos(area.min)
            .show(ctx, |ui| {
                let backdrop = ui.allocate_rect(area, Sense::click());
                ui.painter().rect_filled(area, 0.0, BACKDROP);
                let inner = content(ui);
                (inner, backdrop.clicked())
            })
            .inner
    }

    /// [`Alert::show`] over a dimmed modal backdrop in `area`. Returns the action clicked and
    /// whether the backdrop outside the card was clicked.
    pub fn show_modal(
        &self,
        ctx: &egui::Context,
        id: impl egui::AsId,
        area: Rect,
    ) -> (Option<usize>, bool) {
        Self::modal(ctx, id, area, |ui| self.show(ui, area))
    }

    /// [`Alert::show_options`] over a dimmed modal backdrop in `area`.
    pub fn show_options_modal(
        &self,
        ctx: &egui::Context,
        id: impl egui::AsId,
        area: Rect,
        options: &[&str],
        chosen: &mut usize,
    ) -> (Option<usize>, bool) {
        Self::modal(ctx, id, area, |ui| {
            self.show_options(ui, area, options, chosen)
        })
    }

    /// [`Alert::show_runs`] over a dimmed modal backdrop in `area`.
    pub fn show_runs_modal(
        &self,
        ctx: &egui::Context,
        id: impl egui::AsId,
        area: Rect,
        runs: &[AlertRun<'_>],
    ) -> (Option<AlertClick>, bool) {
        Self::modal(ctx, id, area, |ui| self.show_runs(ui, area, runs))
    }

    /// [`Alert::show_runs_with`] over a dimmed modal backdrop in `area`.
    pub fn show_runs_modal_with(
        &self,
        ctx: &egui::Context,
        id: impl egui::AsId,
        area: Rect,
        runs: &[AlertRun<'_>],
        extra_height: f32,
        extra: impl FnOnce(&mut Ui, Rect),
    ) -> (Option<AlertClick>, bool) {
        Self::modal(ctx, id, area, |ui| {
            self.show_runs_with(ui, area, runs, extra_height, extra)
        })
    }

    /// [`Alert::show_body`] over a dimmed modal backdrop in `area`.
    pub fn show_body_modal(
        &self,
        ctx: &egui::Context,
        id: impl egui::AsId,
        area: Rect,
        height: f32,
        body: impl FnOnce(&mut Ui, Rect),
    ) -> (Option<usize>, bool) {
        Self::modal(ctx, id, area, |ui| self.show_body(ui, area, height, body))
    }

    /// Paints the alert centered in `area` (the window below the tab bar). Returns the action
    /// clicked, if any.
    pub fn show(&self, ui: &mut Ui, area: Rect) -> Option<usize> {
        let ctx = ui.ctx().clone();
        let paragraphs: Vec<Vec<String>> = self
            .text
            .split(PARAGRAPH)
            .map(|paragraph| pretty_wrap(&ctx, paragraph, self.content_width()))
            .collect();
        let lines: usize = paragraphs.iter().map(Vec::len).sum();
        let height = lines as f32 * TEXT.line_height
            + (paragraphs.len().saturating_sub(1)) as f32 * TEXT_MARGIN;
        self.paint(ui, area, height, |ui, text| {
            paint_paragraphs(ui.painter(), text.min, &paragraphs);
        })
    }

    /// [`Alert::show`] with a radio button per option in place of the text, one per line, as a
    /// recurring event's RSVP puts them (12px, `top: 2px`, 7px before its label). A click on an
    /// option's line selects it into `chosen`.
    pub fn show_options(
        &self,
        ui: &mut Ui,
        area: Rect,
        options: &[&str],
        chosen: &mut usize,
    ) -> Option<usize> {
        let height = options.len() as f32 * TEXT.line_height;
        let width = self.content_width();
        self.paint(ui, area, height, |ui, text| {
            for (index, label) in options.iter().enumerate() {
                let y = text.top() + index as f32 * TEXT.line_height;
                let line = Rect::from_min_size(pos2(text.left(), y), vec2(width, TEXT.line_height));
                if ui
                    .interact(
                        line,
                        ui.id().with(("alert-option", self.title, index)),
                        Sense::click(),
                    )
                    .on_hover_cursor(egui::CursorIcon::PointingHand)
                    .clicked()
                {
                    *chosen = index;
                }
                paint_radio(ui.painter(), pos2(text.left(), y + 2.0), index == *chosen);
                TEXT.paint_line(
                    ui.painter(),
                    pos2(text.left() + RADIO + RADIO_GAP, y),
                    label,
                    TEXT_INK,
                    None,
                );
            }
        })
    }

    /// [`Alert::show`] whose text is one paragraph of `before`, an inline keycap labelled `key`
    /// (the follow-up hint's `H`) and `after`, wrapped at its spaces.
    pub fn show_with_key(
        &self,
        ui: &mut Ui,
        area: Rect,
        before: &str,
        key: &str,
        after: &str,
    ) -> Option<usize> {
        self.show_with_keys(ui, area, before, &[key], after)
    }

    /// [`Alert::show`] whose text is `before`, laid out as [`Alert::show`] lays text out, then a
    /// keycap for each of `keys` on its last line after a space (inline, side by side: Get Me To
    /// Zero's `⌘ K`), then a space and `after`, wrapped at its spaces.
    pub fn show_with_keys(
        &self,
        ui: &mut Ui,
        area: Rect,
        before: &str,
        keys: &[&str],
        after: &str,
    ) -> Option<usize> {
        self.show_with_caps(ui, area, before, keys, after, &KEY)
    }

    /// [`Alert::show_with_key`] with a plain inline keycap, a bold label on it (the Undo Send
    /// alert's `Z`).
    pub fn show_with_confirm_key(
        &self,
        ui: &mut Ui,
        area: Rect,
        before: &str,
        key: &str,
        after: &str,
    ) -> Option<usize> {
        self.show_with_caps(ui, area, before, &[key], after, &CONFIRM_KEY)
    }

    /// [`Alert::show_with_keys`] with keycaps drawn as `cap` says.
    fn show_with_caps(
        &self,
        ui: &mut Ui,
        area: Rect,
        before: &str,
        keys: &[&str],
        after: &str,
        cap: &Cap,
    ) -> Option<usize> {
        let ctx = ui.ctx().clone();
        let width = self.content_width();
        let paragraphs: Vec<Vec<String>> = before
            .split(PARAGRAPH)
            .map(|paragraph| pretty_wrap(&ctx, paragraph, width))
            .collect();
        let last = paragraphs
            .last()
            .and_then(|lines| lines.last())
            .map_or(0.0, |line| TEXT.width(&ctx, line));
        let space = TEXT.width(&ctx, "a a") - TEXT.width(&ctx, "aa");
        let key_widths: Vec<f32> = keys
            .iter()
            .map(|key| (cap.label.width(&ctx, key) + 2.0 * KEY_PADDING).max(cap.min_width))
            .collect();
        let keys_left = last + space;
        let prefix = keys_left
            + key_widths
                .iter()
                .map(|width| width + 2.0 * cap.margin)
                .sum::<f32>()
            + space;
        // Greedy, as Chromium breaks a paragraph whose last line keeps more than a word.
        let mut lines: Vec<String> = vec![String::new()];
        for word in after.split_whitespace() {
            let first = lines.len() == 1;
            let limit = if first { width - prefix } else { width };
            let line = lines.last_mut().expect("a line");
            let candidate = if line.is_empty() {
                word.to_owned()
            } else {
                format!("{line} {word}")
            };
            if line.is_empty() && !first || TEXT.width(&ctx, &candidate) <= limit {
                *line = candidate;
            } else {
                lines.push(word.to_owned());
            }
        }
        let before_lines: usize = paragraphs.iter().map(Vec::len).sum();
        let gaps = paragraphs.len().saturating_sub(1) as f32 * TEXT_MARGIN;
        let height = (before_lines + lines.len() - 1) as f32 * TEXT.line_height + gaps + cap.shift;
        let (fill, ink) = match crate::app_theme(&ctx) {
            crate::Theme::Snow => (KEY_FILL, Color32::WHITE),
            crate::Theme::Carbon => (KEY_FILL_CARBON, Color32::WHITE.gamma_multiply(0.8)),
        };
        // The keys and `after` continue the last line, which is painted with them.
        let mut paragraphs = paragraphs;
        let key_line = paragraphs.last_mut().and_then(Vec::pop).unwrap_or_default();
        self.paint(ui, area, height, |ui, text| {
            let painter = ui.painter();
            let y = paint_paragraphs(painter, text.min, &paragraphs);
            paint_paragraphs(
                painter,
                pos2(text.left(), y + cap.shift),
                &[vec![key_line.clone()]],
            );
            let mut x = text.left() + keys_left;
            for (key, key_width) in keys.iter().zip(&key_widths) {
                let left = x + cap.margin;
                let rect = Rect::from_min_size(
                    pos2(if cap.snap { left.round() } else { left }, y + cap.top),
                    vec2(*key_width, KEY_HEIGHT),
                );
                painter.rect_filled(rect, CornerRadius::same(3), fill);
                // The label stays where layout put it, unsnapped.
                let label = cap.label.width(&ctx, key);
                cap.label.paint_line(
                    painter,
                    pos2(
                        left + key_width / 2.0 - label / 2.0,
                        rect.top() + cap.label_top,
                    ),
                    key,
                    ink,
                    None,
                );
                x += key_width + 2.0 * cap.margin;
            }
            for (index, line) in lines.iter().enumerate() {
                let x = if index == 0 {
                    text.left() + prefix
                } else {
                    text.left()
                };
                TEXT.paint_line(
                    painter,
                    pos2(x, y + cap.shift + index as f32 * TEXT.line_height),
                    line,
                    TEXT_INK,
                    None,
                );
            }
        })
    }

    /// [`Alert::show`] with its text as `runs`, wrapped as the alert's own text wraps (with
    /// [`PARAGRAPH`] separating `<p>`s by 8px and `\n` as `<br>`), the bold runs bold and the
    /// links in their colour. Returns the action clicked, or the link.
    pub fn show_runs(&self, ui: &mut Ui, area: Rect, runs: &[AlertRun<'_>]) -> Option<AlertClick> {
        self.show_runs_with(ui, area, runs, 0.0, |_, _| {})
    }

    /// [`Alert::show_runs`] with `extra` painting an additional `extra_height`-tall block directly
    /// under the last paragraph, before the actions row (for an opt-out checkbox or subtext).
    pub fn show_runs_with(
        &self,
        ui: &mut Ui,
        area: Rect,
        runs: &[AlertRun<'_>],
        extra_height: f32,
        extra: impl FnOnce(&mut Ui, Rect),
    ) -> Option<AlertClick> {
        let paragraphs = wrap_runs_paragraphs(ui.ctx(), runs, self.content_width());
        let lines: usize = paragraphs.iter().map(Vec::len).sum();
        let paragraphs_height = lines as f32 * TEXT.line_height
            + paragraphs.len().saturating_sub(1) as f32 * TEXT_MARGIN;
        let height = paragraphs_height + extra_height;
        let mut link = None;
        let action = self.paint(ui, area, height, |ui, text| {
            let mut y = text.top();
            for (paragraph_index, paragraph) in paragraphs.iter().enumerate() {
                if paragraph_index > 0 {
                    y += TEXT_MARGIN;
                }
                for (index, line) in paragraph.iter().enumerate() {
                    let mut x = text.left();
                    for (run, segment) in line {
                        let ink = if matches!(runs[*run], AlertRun::Link(_)) {
                            LINK_INK
                        } else {
                            TEXT_INK
                        };
                        let width = runs[*run].style().paint_line(
                            ui.painter(),
                            pos2(x, y),
                            segment,
                            ink,
                            None,
                        );
                        if matches!(runs[*run], AlertRun::Link(_)) {
                            let place = runs[..*run]
                                .iter()
                                .filter(|run| matches!(run, AlertRun::Link(_)))
                                .count();
                            let rect =
                                Rect::from_min_size(pos2(x, y), vec2(width, TEXT.line_height));
                            let response = ui
                                .interact(
                                    rect,
                                    ui.id().with((
                                        "alert-link",
                                        self.title,
                                        paragraph_index,
                                        index,
                                        *run,
                                    )),
                                    Sense::click(),
                                )
                                .on_hover_cursor(egui::CursorIcon::PointingHand);
                            if response.clicked() {
                                link = Some(place);
                            }
                        }
                        x += width;
                    }
                    y += TEXT.line_height;
                }
            }
            extra(
                ui,
                Rect::from_min_size(pos2(text.left(), y), vec2(text.width(), extra_height)),
            );
        });
        action
            .map(AlertClick::Action)
            .or(link.map(AlertClick::Link))
    }

    /// [`Alert::show`] with `body` painting the text box itself ([`ALERT_TEXT_WIDTH`] wide with
    /// actions and `height` tall, its top already nudged down), for an alert whose children aren't
    /// plain text (bold runs, links, a checkbox). Paint in [`ALERT_TEXT`] and [`ALERT_TEXT_INK`] to
    /// match the plain text.
    pub fn show_body(
        &self,
        ui: &mut Ui,
        area: Rect,
        height: f32,
        body: impl FnOnce(&mut Ui, Rect),
    ) -> Option<usize> {
        self.paint(ui, area, height, body)
    }

    /// Where the actions go: each one's row, left edge from the text box's, and width. They float
    /// right, the first 12px into the padding and each next 8px left of the one before, and one
    /// that doesn't fit beside the one before starts a row below, 8px in from the right. (A
    /// float that wraps that way spills out of the card in Chromium, whose card doesn't grow for
    /// it. The card holds it here.)
    fn place_actions(&self, ctx: &egui::Context) -> Vec<(usize, f32, f32)> {
        let content = self.content_width();
        let mut row = 0;
        let mut right = content + ACTION_PADDING[1];
        self.actions
            .iter()
            .enumerate()
            .map(|(index, label)| {
                let width = form::action_width(ctx, label);
                if index > 0 && right - width < 0.0 {
                    row += 1;
                    right = content - ACTION_GAP;
                }
                let placed = (row, right - width, width);
                right -= width + ACTION_GAP;
                placed
            })
            .collect()
    }

    /// The text box's width. Without actions, there's no action row and a narrower padding.
    fn content_width(&self) -> f32 {
        let padding_right = if self.actions.is_empty() {
            PADDING_RIGHT_NO_ACTIONS
        } else {
            PADDING_RIGHT
        };
        WIDTH - PADDING_LEFT - padding_right
    }

    /// [`Alert::show_body`] with `emoji` drawn as an image `size` big after the title and a space
    /// (`Welcome to Exo <img>`). The image stands on the title's baseline, and when it's taller
    /// than the baseline is low it raises the title's line box, pushing the rest down.
    pub fn show_body_with_title_emoji(
        &self,
        ui: &mut Ui,
        area: Rect,
        emoji: &str,
        size: egui::Vec2,
        height: f32,
        body: impl FnOnce(&mut Ui, Rect),
    ) -> Option<usize> {
        let ctx = ui.ctx().clone();
        let raised = (size.y - TITLE.baseline(&ctx)).max(0.0);
        let title = |painter: &egui::Painter, top_left: egui::Pos2| {
            let width = TITLE.paint_line(
                painter,
                top_left + vec2(0.0, raised),
                &format!("{} ", self.title),
                TITLE_INK,
                None,
            );
            let image = Rect::from_min_size(
                top_left + vec2(width, raised + TITLE.baseline(&ctx) - size.y),
                size,
            );
            crate::paint_emoji(painter, emoji, image);
        };
        self.paint_titled(ui, area, TITLE.line_height + raised, title, height, body)
    }

    fn paint(
        &self,
        ui: &mut Ui,
        area: Rect,
        text_height: f32,
        body: impl FnOnce(&mut Ui, Rect),
    ) -> Option<usize> {
        let title = |painter: &egui::Painter, top_left: egui::Pos2| {
            TITLE.paint_line(painter, top_left, self.title, TITLE_INK, None);
        };
        self.paint_titled(ui, area, TITLE.line_height, title, text_height, body)
    }

    /// The alert with its title's line box `title_height` tall, painted by `title` from its top
    /// left (nudged down already).
    fn paint_titled(
        &self,
        ui: &mut Ui,
        area: Rect,
        title_height: f32,
        title: impl FnOnce(&egui::Painter, egui::Pos2),
        text_height: f32,
        body: impl FnOnce(&mut Ui, Rect),
    ) -> Option<usize> {
        let ctx = ui.ctx().clone();
        let placed = self.place_actions(&ctx);
        let rows = placed.last().map_or(0, |(row, _, _)| row + 1);
        let actions_height = rows as f32 * ACTIONS_HEIGHT;
        let height = PADDING_TOP
            + title_height
            + TEXT_MARGIN
            + text_height
            + TEXT_MARGIN
            + actions_height
            + PADDING_BOTTOM;
        // Centering can land on a half pixel, and Chromium paints the alert's layer at the whole
        // pixel below it, text and all.
        let card = pixel_snap(Rect::from_center_size(area.center(), vec2(WIDTH, height)));
        // The card swallows clicks so they don't reach a modal alert's backdrop.
        let _ = ui.interact(
            card,
            ui.id().with(("alert-card", self.title)),
            Sense::click(),
        );

        let painter = ui.painter();
        for shadow in SHADOWS {
            shadow.paint(painter, card, CornerRadius::same(RADIUS));
        }
        painter.rect_filled(card, CornerRadius::same(RADIUS), Color32::WHITE);
        let left = card.left() + PADDING_LEFT;
        let title_top = card.top() + PADDING_TOP;
        title(painter, pos2(left, title_top + NUDGE));
        let text_top = title_top + title_height + TEXT_MARGIN;
        body(
            ui,
            Rect::from_min_size(
                pos2(left, text_top + NUDGE),
                vec2(self.content_width(), text_height),
            ),
        );

        let [pad_top, _, pad_bottom] = ACTION_PADDING;
        let rows_top = text_top + text_height + TEXT_MARGIN;
        let mut clicked = None;
        for (index, (label, (row, offset, width))) in self.actions.iter().zip(placed).enumerate() {
            let size = vec2(width, pad_top + TEXT.line_height + pad_bottom);
            let button = Rect::from_min_size(
                pos2(
                    card.left() + PADDING_LEFT + offset,
                    rows_top + row as f32 * ACTIONS_HEIGHT + NUDGE,
                ),
                size,
            );
            let response = ui.interact(
                button,
                ui.id().with(("alert-action", self.title, index)),
                Sense::click(),
            );
            // Selected, hovered and pressed are filled; selected and hovered glow. Hover and press
            // transition 150ms ease.
            let selected = if index == self.selected { 1.0 } else { 0.0 };
            let hover = motion::transition(
                &ctx,
                response.id.with("hover"),
                response.hovered(),
                0.15,
                motion::CubicBezier::EASE,
            );
            let pressed = motion::transition(
                &ctx,
                response.id.with("pressed"),
                response.is_pointer_button_down_on(),
                0.15,
                motion::CubicBezier::EASE,
            );
            let (fill, ink) = form::filled(f32::max(selected, f32::max(hover, pressed)));
            form::paint_action(
                ui.painter(),
                button,
                label,
                fill,
                f32::max(selected, hover),
                ink,
            );
            if response.clicked() {
                clicked = Some(index);
            }
        }
        clicked
    }
}

/// A 12px radio at `at`: a ring of black at 40% 5.5px out, and when selected a `#6FBEEA` dot of
/// 3px.
pub fn paint_radio(painter: &egui::Painter, at: egui::Pos2, selected: bool) {
    let center = at + vec2(RADIO / 2.0, RADIO / 2.0);
    painter.circle_stroke(
        center,
        5.5,
        egui::Stroke::new(1.0, Color32::from_black_alpha(102)),
    );
    if selected {
        painter.circle_filled(center, 3.0, Color32::from_rgb(0x6f, 0xbe, 0xea));
    }
}

/// Paints `paragraphs`' lines from `at` down, 8px between paragraphs, and returns the bottom of
/// the last line. Each piece between `<wbr>`s ([`wbrise`]) is shaped on its own, as Chromium's
/// are, with no kerning across them.
fn paint_paragraphs(painter: &egui::Painter, at: egui::Pos2, paragraphs: &[Vec<String>]) -> f32 {
    let mut y = at.y;
    for (index, paragraph) in paragraphs.iter().enumerate() {
        if index > 0 {
            y += TEXT_MARGIN;
        }
        for line in paragraph {
            let mut x = at.x;
            for piece in line.split(WBR) {
                x += TEXT.paint_line(painter, pos2(x, y), piece, TEXT_INK, None);
            }
            y += TEXT.line_height;
        }
    }
    y
}

/// Wraps `paragraph` as an alert's text wraps: at `<br>`s and spaces, with whitespace collapsed
/// (`white-space: normal`), and with `text-wrap: pretty`'s rule against a last line of one word,
/// under which the line above gives up its last word when it can.
fn pretty_wrap(ctx: &egui::Context, paragraph: &str, width: f32) -> Vec<String> {
    let mut lines = Vec::new();
    for line in paragraph.split('\n') {
        let collapsed = line.split_whitespace().collect::<Vec<_>>().join(" ");
        lines.extend(TEXT.wrap_pretty(ctx, &collapsed, width));
    }
    lines
}

/// A word of an alert's runs that can't be broken, maybe across runs (`Settings` and `,`): its
/// pieces by run, and the run of the space before it, if there's one.
struct Word {
    space: Option<usize>,
    pieces: Vec<(usize, String)>,
}

/// `runs` split into `<p>` paragraphs at [`PARAGRAPH`] and wrapped as [`pretty_wrap`] wraps plain
/// text: at `<br>`s (`\n`) and spaces, whitespace collapsed, with `text-wrap: pretty`'s rule for
/// each part between `<br>`s. Each line is its segments, by run, with the spaces between its words
/// in the run they're in.
fn wrap_runs_paragraphs(
    ctx: &egui::Context,
    runs: &[AlertRun<'_>],
    width: f32,
) -> Vec<Vec<Vec<(usize, String)>>> {
    if runs.is_empty() {
        return Vec::new();
    }
    let mut paragraphs: Vec<Vec<Vec<Word>>> = vec![vec![Vec::new()]];
    let mut space = None;
    let mut glued = false;
    for (run, text) in runs.iter().enumerate() {
        for (paragraph_index, paragraph) in text.text().split(PARAGRAPH).enumerate() {
            if paragraph_index > 0 {
                paragraphs.push(vec![Vec::new()]);
                (space, glued) = (None, false);
            }
            for (line, part) in paragraph.split('\n').enumerate() {
                if line > 0 {
                    paragraphs.last_mut().expect("a paragraph").push(Vec::new());
                    (space, glued) = (None, false);
                }
                for (at, piece) in part.split(' ').enumerate() {
                    if at > 0 {
                        space = Some(run);
                        glued = false;
                    }
                    if piece.is_empty() {
                        continue;
                    }
                    let words = paragraphs
                        .last_mut()
                        .expect("a paragraph")
                        .last_mut()
                        .expect("a part");
                    match words.last_mut() {
                        Some(word) if glued => word.pieces.push((run, piece.to_owned())),
                        _ => words.push(Word {
                            space: space.take(),
                            pieces: vec![(run, piece.to_owned())],
                        }),
                    }
                    glued = true;
                }
            }
        }
    }
    let segments = |words: &[&Word]| {
        let mut line: Vec<(usize, String)> = Vec::new();
        let mut push = |run: usize, text: &str| match line.last_mut() {
            Some((last, segment)) if *last == run => segment.push_str(text),
            _ => line.push((run, text.to_owned())),
        };
        for (at, word) in words.iter().enumerate() {
            if let Some(run) = word.space.filter(|_| at > 0) {
                push(run, " ");
            }
            for (run, piece) in &word.pieces {
                push(*run, piece);
            }
        }
        line
    };
    let measure = |words: &[&Word]| -> f32 {
        segments(words)
            .iter()
            .map(|(run, segment)| runs[*run].style().width(ctx, segment))
            .sum()
    };
    paragraphs
        .iter()
        .map(|parts| {
            let mut lines = Vec::new();
            for part in parts {
                let mut wrapped: Vec<Vec<&Word>> = vec![Vec::new()];
                for word in part {
                    let line = wrapped.last_mut().expect("a line");
                    line.push(word);
                    if line.len() > 1 && measure(line) > width {
                        line.pop();
                        wrapped.push(vec![word]);
                    }
                }
                // `text-wrap: pretty`: a last line of one word takes the word before it, when they fit.
                if let [.., before, last] = wrapped.as_mut_slice()
                    && last.len() == 1
                    && before.len() > 1
                {
                    let mut joined = vec![before[before.len() - 1]];
                    joined.extend(last.iter());
                    if measure(&joined) <= width {
                        before.pop();
                        *last = joined;
                    }
                }
                lines.extend(wrapped.iter().map(|words| segments(words)));
            }
            lines
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::{ALERT_TEXT_WIDTH, Alert, AlertRun, PARAGRAPH, wbrise, wrap_runs_paragraphs};

    #[test]
    fn runs_wrap_as_one_text_with_words_across_runs_kept_whole() {
        let ctx = egui::Context::default();
        crate::install(&ctx).expect("the system fonts load");
        let runs = [
            AlertRun::Text(
                "To RSVP faster next time, please update your Google Calendar settings. \n\n Under ",
            ),
            AlertRun::Link("Event Settings"),
            AlertRun::Text(", change "),
            AlertRun::Strong("Add invitations to my calendar"),
            AlertRun::Text(" to "),
            AlertRun::Strong("From everyone"),
            AlertRun::Text("."),
        ];
        let mut lines = Vec::new();
        let mut output = ctx.run_ui(egui::RawInput::default(), |ui| {
            lines = wrap_runs_paragraphs(ui.ctx(), &runs, ALERT_TEXT_WIDTH)
                .into_iter()
                .next()
                .unwrap_or_default();
        });
        output.textures_delta.clear();
        let text: Vec<String> = lines
            .iter()
            .map(|line| line.iter().map(|(_, segment)| segment.as_str()).collect())
            .collect();
        assert_eq!(
            text,
            [
                "To RSVP faster next time, please update your",
                "Google Calendar settings.",
                "",
                "Under Event Settings, change Add invitations to",
                "my calendar to From everyone.",
            ]
        );
        // `Settings` and `,` stay together, each in its own run.
        assert_eq!(
            lines[3],
            [
                (0, "Under ".to_owned()),
                (1, "Event Settings".to_owned()),
                (2, ", change ".to_owned()),
                (3, "Add invitations to".to_owned()),
            ]
        );
    }

    #[test]
    fn runs_split_into_paragraphs_and_selection_wraps_or_clamps() {
        let ctx = egui::Context::default();
        crate::install(&ctx).expect("the system fonts load");
        let separator = PARAGRAPH.to_string();
        let runs = [
            AlertRun::Text("Out on "),
            AlertRun::Strong("Friday, November 20."),
            AlertRun::Text(&separator),
            AlertRun::Text("Send automatic replies?"),
        ];
        let mut paragraphs = Vec::new();
        let mut output = ctx.run_ui(egui::RawInput::default(), |ui| {
            paragraphs = wrap_runs_paragraphs(ui.ctx(), &runs, ALERT_TEXT_WIDTH);
        });
        output.textures_delta.clear();
        assert_eq!(paragraphs.len(), 2);
        assert_eq!(Alert::move_selection(0, 2, -1, true), 1);
        assert_eq!(Alert::move_selection(0, 2, -1, false), 0);
        assert_eq!(Alert::move_selection(0, 3, 1, true), 1);
        assert_eq!(Alert::move_selection(0, 3, -1, true), 2);
    }

    #[test]
    fn wbrise_breaks_before_words_that_follow_punctuation() {
        assert_eq!(
            wbrise("daniel@brightmoor.co"),
            "daniel@\u{200B}brightmoor.\u{200B}co"
        );
        assert_eq!(wbrise("shop.example"), "shop.\u{200B}example");
        assert_eq!(wbrise("a.b-c d"), "a.\u{200B}b-\u{200B}c d");
        assert_eq!(wbrise("plain"), "plain");
    }
}

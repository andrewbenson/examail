//! The window chrome drawn over the page: a tab strip with its own traffic lights (the native
//! ones are hidden).
//!
//! The strip is the window's, not the page's, so zooming leaves it as it is. It's drawn at the
//! window's own scale whatever the zoom ([`scale`]), and the page starts under it
//! ([`tab_bar_height`]).

use std::sync::Arc;

use egui::{
    Color32, CornerRadius, Galley, Pos2, Rect, Sense, Stroke, StrokeKind, Ui, Vec2,
    ViewportCommand, pos2,
    text::{CCursor, LayoutJob},
    vec2,
};
use exo_ui::{
    Family, Font, TextStyle, Theme,
    motion::{self, CubicBezier},
    paint_icon, paint_text,
};
use icu_segmenter::{
    LineSegmenter,
    options::{LineBreakOptions, LineBreakWordOption},
};

/// The bar is 40px including its bottom border. The tabs' 1px top margin collapses through it, so
/// it sits 1px down and fills the 41px strip the page leaves for it ([`tab_bar_height`]).
const BAR_TOP: f32 = 1.0;
const BAR_HEIGHT: f32 = 40.0;
/// The strip's height: the bar and the 1px above it.
const TAB_BAR_HEIGHT: f32 = 41.0;
/// What starts a See Your Week title (`📆 {week} - {who}`), which the pill draws as today's date
/// on a calendar in its account's colour.
const CALENDAR: &str = "📆";

/// Where the page starts, below the strip. That's the strip's 41px at the window's own scale, in
/// the zoomed page's points, rounded to whole points.
pub(crate) fn tab_bar_height(ctx: &egui::Context) -> f32 {
    (TAB_BAR_HEIGHT / ctx.zoom_factor()).round()
}

/// Converts the strip's points to the zoomed page's, since the strip draws at the window's own
/// scale.
fn scale(ctx: &egui::Context) -> f32 {
    1.0 / ctx.zoom_factor()
}

/// A tab as the strip shows it.
#[derive(Clone, Copy)]
pub struct Tab<'a> {
    /// The tab's title.
    pub title: &'a str,
    /// Its account's address, which the title's tooltip shows (else the title). None on the
    /// sign-in screen.
    pub email: Option<&'a str>,
    /// Whether the tab is on the sign-in screen, which has no link to copy.
    pub login_screen: bool,
    /// Its account's colour, by name. None on the sign-in screen.
    pub account_color: Option<&'a str>,
}

/// A tab as the app keeps it between frames, for the strip to draw ([`set_tabs`]).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct Pill {
    pub title: String,
    pub email: Option<String>,
    pub login_screen: bool,
    pub account_color: Option<String>,
}

impl Pill {
    fn tab(&self) -> Tab<'_> {
        Tab {
            title: &self.title,
            email: self.email.as_deref(),
            login_screen: self.login_screen,
            account_color: self.account_color.as_deref(),
        }
    }
}

impl From<&Tab<'_>> for Pill {
    fn from(tab: &Tab<'_>) -> Self {
        Self {
            title: tab.title.to_owned(),
            email: tab.email.map(str::to_owned),
            login_screen: tab.login_screen,
            account_color: tab.account_color.map(str::to_owned),
        }
    }
}

/// The window's tabs for this frame's strip, and which one is on screen.
#[derive(Clone, Default)]
struct Strip {
    tabs: Vec<Pill>,
    active: usize,
}

/// What the strip did this frame: what was clicked, and what the tab on screen showed.
#[derive(Clone, Default)]
struct Drawn {
    action: Option<Action>,
    shown: Option<Pill>,
}

fn strip_id(ctx: &egui::Context) -> egui::Id {
    exo_ui::id(ctx, "chrome-strip")
}

fn drawn_id(ctx: &egui::Context) -> egui::Id {
    exo_ui::id(ctx, "chrome-drawn")
}

/// Hands the strip the window's `tabs` and the place of the `active` one, for the screen on
/// screen to draw this frame. That screen draws its own tab in that place. Without this call, a
/// screen's strip has its tab alone.
pub(crate) fn set_tabs(ctx: &egui::Context, tabs: Vec<Pill>, active: usize) {
    let id = strip_id(ctx);
    ctx.data_mut(|data| data.insert_temp(id, Strip { tabs, active }));
}

/// What was clicked in the strip this frame, and what the tab on screen showed, if a screen drew
/// the strip since the last call.
pub(crate) fn take(ctx: &egui::Context) -> (Option<Action>, Option<Pill>) {
    let id = drawn_id(ctx);
    let drawn = ctx
        .data_mut(|data| data.remove_temp::<Drawn>(id))
        .unwrap_or_default();
    (drawn.action, drawn.shown)
}

/// What the strip is drawn over, which sets its colors.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Backdrop {
    /// The page, in the theme's colors.
    Page,
    /// A photo, behind the sign-in screen or a full-screen alert.
    Photo,
}

/// The strip's colors over a [`Backdrop`]: in Snow, in Carbon and over a photo.
struct Colors {
    bar_border: Color32,
    pill_border: Color32,
    text: Color32,
    text_emphasized: Color32,
    icon: Color32,
    icon_hovered: Color32,
    icon_hover_fill: Color32,
    add: Color32,
    light_border: Color32,
    blurred_fill: Color32,
    /// Whether the strip is Carbon's, which greys the traffic lights for Graphite its own way.
    carbon: bool,
}

impl Colors {
    fn new(ui: &Ui, backdrop: Backdrop) -> Self {
        // `hsla(0,0%,100%,.1)`: the bar's, pills' and traffic lights' borders in Carbon and over a
        // photo.
        let faint = Color32::from_white_alpha(26);
        match (backdrop, exo_ui::theme(ui.ctx())) {
            // The bar's, pills' and traffic lights' borders at 10–15% black; text at 40%, 85%
            // hovered or active; the icons `#81868d`, hovered `rgb(79.8, 83.3, 88.2)` on
            // `#e1e5ea`; an unfocused window's traffic lights 6% black.
            (Backdrop::Page, Theme::Snow) => Self {
                bar_border: Color32::from_black_alpha(26),
                pill_border: Color32::from_black_alpha(26),
                text: Color32::from_black_alpha(102),
                text_emphasized: Color32::from_black_alpha(217),
                icon: Color32::from_rgb(0x81, 0x86, 0x8d),
                icon_hovered: Color32::from_rgb(80, 83, 88),
                icon_hover_fill: Color32::from_rgb(0xe1, 0xe5, 0xea),
                add: Color32::from_rgb(0x81, 0x86, 0x8d),
                light_border: Color32::from_black_alpha(38),
                blurred_fill: Color32::from_black_alpha(15),
                carbon: false,
            },
            // Carbon: white at 40%/85%, icons `#959ba4`, hovered `hsl(216, 7.6%, 81.4%)` on
            // `#464b53`, blurred traffic lights 6% white.
            (Backdrop::Page, Theme::Carbon) => Self {
                bar_border: faint,
                pill_border: faint,
                text: Color32::from_white_alpha(102),
                text_emphasized: Color32::from_white_alpha(217),
                icon: Color32::from_rgb(0x95, 0x9b, 0xa4),
                icon_hovered: Color32::from_rgb(204, 207, 211),
                icon_hover_fill: Color32::from_rgb(0x46, 0x4b, 0x53),
                add: Color32::from_rgb(0x95, 0x9b, 0xa4),
                light_border: faint,
                blurred_fill: Color32::from_white_alpha(15),
                carbon: true,
            },
            // Over a photo: white at .4 (.85 emphasized), icons white at .6 on a white hover fill,
            // the + in `#81868d`.
            (Backdrop::Photo, _) => Self {
                bar_border: Color32::TRANSPARENT,
                pill_border: faint,
                text: Color32::from_white_alpha(102),
                text_emphasized: Color32::from_white_alpha(217),
                icon: Color32::from_white_alpha(153),
                icon_hovered: Color32::from_white_alpha(153),
                icon_hover_fill: Color32::WHITE,
                add: Color32::from_rgb(0x81, 0x86, 0x8d),
                light_border: faint,
                blurred_fill: Color32::from_white_alpha(20),
                carbon: false,
            },
        }
    }
}

/// What the user did in the strip this frame.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Action {
    /// Clicked tab `n`.
    Select(usize),
    /// Clicked tab `n`'s ×.
    Close(usize),
    /// Clicked tab `n`'s copy-link button.
    CopyLink(usize),
    /// Clicked +.
    NewTab,
}

/// The window state the chrome reacts to.
struct Window {
    focused: bool,
    fullscreen: bool,
    maximized: bool,
    alt: bool,
    /// Whether macOS's accent colour is Graphite, which greys the traffic lights.
    graphite: bool,
}

/// Whether macOS's accent colour is Graphite, as read when the window last came to the front.
/// `AppleAccentColor` is read again each time the window is focused, and the window starts out
/// focused.
fn graphite(ctx: &egui::Context, focused: bool) -> bool {
    let id = exo_ui::id(ctx, "chrome-graphite");
    let (was_focused, graphite) = ctx.data(|data| data.get_temp::<(bool, bool)>(id)).unzip();
    let graphite = match graphite {
        Some(graphite) if was_focused == Some(true) || !focused => graphite,
        _ => crate::desktop::graphite_accent(),
    };
    ctx.data_mut(|data| data.insert_temp(id, (focused, graphite)));
    graphite
}

/// Draws the strip across the top of `ui`, over `backdrop`, with the window's tabs
/// ([`set_tabs`]) and `tab` for the one on screen. What's clicked is for [`take`].
pub fn show(ui: &mut Ui, tab: Tab<'_>, backdrop: Backdrop) {
    let id = strip_id(ui.ctx());
    let strip = ui
        .ctx()
        .data(|data| data.get_temp::<Strip>(id))
        .unwrap_or_default();
    let active = strip.active.min(strip.tabs.len().saturating_sub(1));
    let tabs: Vec<Tab<'_>> = match strip.tabs.len() {
        0 => vec![tab],
        _ => strip
            .tabs
            .iter()
            .enumerate()
            .map(|(i, pill)| if i == active { tab } else { pill.tab() })
            .collect(),
    };
    let action = draw(ui, &tabs, active, backdrop);
    let shown = Some(Pill::from(&tab));
    let id = drawn_id(ui.ctx());
    ui.ctx()
        .data_mut(|data| data.insert_temp(id, Drawn { action, shown }));
}

/// Draws the strip of `tabs`, with `active` the one on screen.
fn draw(ui: &mut Ui, tabs: &[Tab<'_>], active: usize, backdrop: Backdrop) -> Option<Action> {
    let s = scale(ui.ctx());
    let bar = Rect::from_min_size(
        ui.max_rect().min + vec2(0.0, BAR_TOP * s),
        vec2(ui.max_rect().width(), BAR_HEIGHT * s),
    );
    let focused = ui.input(|input| input.viewport().focused.unwrap_or(true));
    let graphite = graphite(ui.ctx(), focused);
    let window = ui.input(|input| Window {
        focused,
        fullscreen: input.viewport().fullscreen.unwrap_or(false),
        maximized: input.viewport().maximized.unwrap_or(false),
        alt: input.modifiers.alt,
        graphite,
    });
    let colors = Colors::new(ui, backdrop);

    // The whole bar moves the window, and the controls drawn after it take their own clicks.
    fastframe_titlebar::drag(ui, bar);
    ui.painter().hline(
        bar.x_range(),
        bar.bottom() - 0.5 * s,
        Stroke::new(s, colors.bar_border),
    );
    traffic_lights(ui, bar.min + vec2(12.0, 14.0) * s, &window, &colors, s);
    // The strip is the window's, not the page's, so find doesn't search it.
    let ctx = ui.ctx().clone();
    exo_ui::find::unsearchable(&ctx, || {
        tab_strip(ui, bar, tabs, active, &window, &colors, s)
    })
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Control {
    Close,
    Minimize,
    Fullscreen,
}

impl Control {
    /// Fill and border while the group is hovered (macOS's own colors), and its glyph's opacity.
    /// With the Graphite accent all three go grey: `#8e8e92` on a `#838488` border, glyphs at
    /// .72; Carbon's `#999`, glyphs at .6.
    fn lit(self, window: &Window, carbon: bool) -> (Color32, Color32, f32) {
        match (window.graphite, carbon) {
            (true, true) => (Color32::from_gray(0x99), Color32::from_gray(0x99), 0.6),
            (true, false) => (
                Color32::from_rgb(0x8e, 0x8e, 0x92),
                Color32::from_rgb(0x83, 0x84, 0x88),
                0.72,
            ),
            (false, _) => {
                let color = match self {
                    Self::Close => Color32::from_rgb(0xef, 0x6c, 0x60),
                    Self::Minimize => Color32::from_rgb(0xf7, 0xbf, 0x50),
                    Self::Fullscreen => Color32::from_rgb(0x61, 0xc8, 0x57),
                };
                (color, color, 0.55)
            }
        }
    }

    /// The glyph's icon, its offset inside the 1px border, and its size.
    fn glyph(self, window: &Window) -> (&'static str, Vec2, Vec2) {
        match self {
            Self::Close => ("shell/window-close", vec2(2.0, 2.0), Vec2::splat(13.0)),
            Self::Minimize => ("shell/window-minimize", vec2(1.0, 4.0), vec2(8.0, 2.0)),
            Self::Fullscreen if window.fullscreen => (
                "shell/window-exit-fullscreen",
                Vec2::ZERO,
                Vec2::splat(10.0),
            ),
            Self::Fullscreen if window.alt => {
                ("shell/window-zoom", vec2(1.0, 1.0), Vec2::splat(8.0))
            }
            Self::Fullscreen => (
                "shell/window-fullscreen",
                vec2(-1.0, 2.0),
                Vec2::splat(12.0),
            ),
        }
    }

    fn command(self, window: &Window) -> ViewportCommand {
        match self {
            Self::Close => ViewportCommand::Close,
            Self::Minimize => ViewportCommand::Minimized(true),
            // ⌥ turns the green button into zoom, as in native windows.
            Self::Fullscreen if window.alt && !window.fullscreen => {
                ViewportCommand::Maximized(!window.maximized)
            }
            Self::Fullscreen => ViewportCommand::Fullscreen(!window.fullscreen),
        }
    }
}

fn traffic_lights(ui: &mut Ui, origin: Pos2, window: &Window, colors: &Colors, s: f32) {
    let size = 12.0 * s;
    let pitch = size + 8.0 * s;
    let carbon = colors.carbon;
    // The group's hover spans the three circles and their right margins.
    let group_hovered =
        ui.rect_contains_pointer(Rect::from_min_size(origin, vec2(3.0 * pitch, size)));
    let (rest_fill, rest_border) = if window.focused {
        (Color32::TRANSPARENT, colors.light_border)
    } else {
        (colors.blurred_fill, Color32::TRANSPARENT)
    };

    for (i, control) in [Control::Close, Control::Minimize, Control::Fullscreen]
        .into_iter()
        .enumerate()
    {
        let rect = Rect::from_min_size(origin + vec2(i as f32 * pitch, 0.0), Vec2::splat(size));
        let enabled = !(control == Control::Minimize && window.fullscreen);
        let sense = if enabled {
            Sense::click()
        } else {
            Sense::hover()
        };
        let response = ui.interact(rect, ui.id().with(("traffic-light", i)), sense);
        let hovered = group_hovered && enabled;

        // Fill and border declare their transition under `:hover`, so they animate in and snap
        // back. The glyph's opacity transition is declared on the base rule and runs both ways.
        let lit = motion::transition_in(
            ui.ctx(),
            response.id.with("lit"),
            hovered,
            0.15,
            CubicBezier::EASE,
        );
        let glyph = motion::transition(
            ui.ctx(),
            response.id.with("glyph"),
            hovered,
            0.15,
            CubicBezier::EASE,
        );
        let (lit_fill, lit_border, glyph_opacity) = control.lit(window, carbon);
        let mut fill = motion::mix(rest_fill, lit_fill, lit);
        let mut border = motion::mix(rest_border, lit_border, lit);
        let mut glyph_color =
            Color32::from_black_alpha((glyph_opacity * glyph * 255.0).round() as u8);
        if enabled && response.is_pointer_button_down_on() {
            // `filter: brightness(75%)` while pressed.
            [fill, border, glyph_color] =
                [fill, border, glyph_color].map(|color| darken(color, 0.75));
        }

        let painter = ui.painter();
        painter.circle_filled(rect.center(), size / 2.0, fill);
        // egui strokes a circle outward from its radius. This puts the 1px border at 5–6px.
        painter.circle_stroke(rect.center(), size / 2.0 - s, Stroke::new(s, border));
        if glyph_color.a() > 0 {
            let (icon, offset, glyph) = control.glyph(window);
            let glyph_rect =
                Rect::from_min_size(rect.min + (Vec2::splat(1.0) + offset) * s, glyph * s);
            paint_icon(ui, icon, glyph_rect, glyph_color);
        }
        if response.clicked() {
            ui.ctx().send_viewport_cmd(control.command(window));
        }
    }
}

/// CSS `brightness()`, which scales the color channels and leaves alpha alone.
fn darken(color: Color32, factor: f32) -> Color32 {
    let [r, g, b, a] = color.to_array();
    let scale = |channel: u8| (f32::from(channel) * factor).round() as u8;
    Color32::from_rgba_premultiplied(scale(r), scale(g), scale(b), a)
}

const PILL_HEIGHT: f32 = 24.0;
const PILL_GAP: f32 = 8.0;
/// The active pill's least width, border included.
const ACTIVE_MIN_WIDTH: f32 = 125.0;
const ADD_SIZE: f32 = 24.0;
/// A tab's icon button: 16px with 4px padding, 3px in from the top right.
const ICON_BUTTON: f32 = 16.0;

fn tab_strip(
    ui: &mut Ui,
    bar: Rect,
    tabs: &[Tab<'_>],
    active: usize,
    window: &Window,
    colors: &Colors,
    s: f32,
) -> Option<Action> {
    // The tabs' `margin: 1px 75px 0` (the 1px collapsed into the bar); the pills' `7px 0 8px 8px`.
    let left = bar.left() + 75.0 * s;
    let width = bar.width() - 150.0 * s;
    let top = bar.top() + 7.0 * s;
    // Pills grow into what's left after their left margins, the find field, the + button and its
    // 8px right margin. A tab only shows as active while the window is focused.
    let count = tabs.len() as f32;
    let (find_margin, find_width) = crate::find::layout(ui.ctx());
    let room =
        (width - (count * PILL_GAP + find_margin + find_width + ADD_SIZE + PILL_GAP) * s).max(0.0);
    let shown_active = window.focused.then_some(active);
    let widths = pill_widths(room, tabs.len(), shown_active, ACTIVE_MIN_WIDTH * s);

    let mut action = None;
    let mut x = left;
    for (i, (tab, width)) in tabs.iter().zip(widths).enumerate() {
        x += PILL_GAP * s;
        let rect = Rect::from_min_size(pos2(x, top), vec2(width, PILL_HEIGHT * s));
        x += width;
        let state = PillState {
            active: shown_active == Some(i),
            closable: tabs.len() > 1,
            window_focused: window.focused,
        };
        action = pill(ui, rect, i, tab, &state, colors, s).or(action);
    }
    let find = Rect::from_min_size(
        pos2(x + find_margin * s, top),
        vec2(find_width * s, PILL_HEIGHT * s),
    );
    crate::find::show(ui, find, s, colors.pill_border, |ui, button, visibility| {
        icon_button(
            ui,
            button,
            "shell/tab-close",
            vec2(8.0, 8.0 * 8.0 / 9.0) * s,
            visibility,
            colors.icon,
            colors,
            s,
        )
    });
    let add = Rect::from_min_size(pos2(find.right(), top), Vec2::splat(ADD_SIZE * s));
    add_button(ui, add, colors, s).or(action)
}

/// The pills' widths in `room`: equal shares (`flex: 1 1 100%`), except that the `active` one
/// keeps at least `min` (`min-width`) and the others share the rest.
fn pill_widths(room: f32, count: usize, active: Option<usize>, min: f32) -> Vec<f32> {
    let share = room / count as f32;
    match active {
        Some(active) if share < min => {
            let rest = ((room - min) / count.saturating_sub(1).max(1) as f32).max(0.0);
            (0..count)
                .map(|i| if i == active { min } else { rest })
                .collect()
        }
        _ => vec![share; count],
    }
}

struct PillState {
    active: bool,
    closable: bool,
    window_focused: bool,
}

fn pill(
    ui: &mut Ui,
    rect: Rect,
    index: usize,
    tab: &Tab<'_>,
    state: &PillState,
    colors: &Colors,
    s: f32,
) -> Option<Action> {
    let response = ui.interact(rect, ui.id().with(("tab", index)), Sense::click());
    // Hovered, its × and copy buttons included, which take the pointer from it.
    let hovered = response.contains_pointer();
    let ctx = ui.ctx().clone();

    // Unfocused windows dim every tab to 80%, except the hovered one.
    let dim = motion::transition(
        &ctx,
        response.id.with("dim"),
        !state.window_focused && !hovered,
        0.05,
        CubicBezier::EASE_OUT,
    );
    let opacity = 1.0 - 0.2 * dim;
    let emphasis = motion::transition(
        &ctx,
        response.id.with("text"),
        hovered || state.active,
        0.15,
        CubicBezier::EASE,
    );
    let text_color = motion::mix(colors.text, colors.text_emphasized, emphasis);

    let painter = ui.painter();
    painter.rect_stroke(
        rect,
        // Whole points, so a zoomed strip's corners are within a third of a point of 4px.
        CornerRadius::from(4.0 * s),
        Stroke::new(s, colors.pill_border.gamma_multiply(opacity)),
        StrokeKind::Inside,
    );
    // The title is centered between `padding: 0 1em` (12.8px) inside the 1px border.
    let unzoomed = TextStyle::new(Font::new(Family::System, 400), 12.8, 22.0);
    let style = TextStyle::new(unzoomed.font, 12.8 * s, 22.0 * s);
    let content = rect.shrink2(vec2(1.0 + 12.8, 1.0) * s);
    // The line box rounds its metrics to whole points. A zoomed strip's baseline goes where the
    // unzoomed one's does.
    let baseline = unzoomed.baseline(ui.ctx()) * s;
    let top = content.top() + baseline - style.baseline(ui.ctx());
    let painter = painter.with_clip_rect(content);
    // A See Your Week title shows today's date on a calendar where it has 📆, and the rest after
    // a space.
    let (calendar, title) = match tab.title.strip_prefix(CALENDAR) {
        Some(rest) => (true, rest.trim_start()),
        None => (false, tab.title),
    };
    // The account's dot leads the title, off the sign-in screen and See Your Week.
    let dot = tab
        .account_color
        .filter(|_| !calendar && !tab.login_screen)
        .and_then(|name| {
            crate::accounts::COLORS
                .iter()
                .find(|(known, _)| *known == name)
        })
        .map(|(_, color)| *color);
    let lead = match (calendar, dot) {
        (true, _) => CALENDAR_DAY * s + style.width(ui.ctx(), " "),
        (false, Some(_)) => (ACCOUNT_DOT + ACCOUNT_DOT_GAP) * s,
        (false, None) => 0.0,
    };
    let layout = |text: &str| {
        ui.fonts_mut(|fonts| {
            fonts.layout_job(LayoutJob::single_section(
                text.to_owned(),
                style.format(text_color.gamma_multiply(opacity)),
            ))
        })
    };
    // A title too long for the pill breaks anywhere (`word-break: break-all`), and only its first
    // line shows, centred.
    let galley = layout(first_line(&layout(title), title, content.width() - lead));
    let line = pos2(content.center().x - (lead + galley.size().x) / 2.0, top);
    // Hovered, the title fades out under the × and copy buttons (`mask-image`): transparent for
    // 6px from its right edge and opaque from 26px, or 25px and 45px with both buttons.
    let buttons = usize::from(state.closable) + usize::from(!tab.login_screen);
    let fade = match buttons {
        1 if hovered => Some((6.0, 26.0)),
        2 if hovered => Some((25.0, 45.0)),
        _ => None,
    };
    let mask = |x: f32| {
        fade.map_or(1.0, |(clear, opaque)| {
            ((content.right() - x - clear * s) / ((opaque - clear) * s)).clamp(0.0, 1.0)
        })
    };
    if let Some(color) = dot {
        // An inline-block on the baseline, .45 opaque and .9 on the hovered or active tab
        // (`transition: opacity 50ms`).
        let lit = motion::transition(
            &ctx,
            response.id.with("dot"),
            hovered || state.active,
            0.05,
            CubicBezier::EASE,
        );
        let bottom = content.top() + baseline;
        let rect = Rect::from_min_size(
            pos2(line.x, bottom - ACCOUNT_DOT * s),
            Vec2::splat(ACCOUNT_DOT * s),
        );
        painter.rect_filled(
            rect,
            CornerRadius::from(2.0 * s),
            color.gamma_multiply((0.45 + 0.45 * lit) * opacity * mask(rect.center().x)),
        );
    }
    if calendar {
        // `vertical-align: -3px` puts its bottom 3px under the text's baseline. It's .45 opaque,
        // and .9 on the hovered or active tab.
        let bottom = content.top() + baseline + 3.0 * s;
        let day = Rect::from_min_size(
            pos2(line.x, bottom - CALENDAR_DAY * s),
            Vec2::splat(CALENDAR_DAY * s),
        );
        let shown = if hovered || state.active { 0.9 } else { 0.45 };
        let color = calendar_color(tab.account_color);
        paint_calendar_day(
            &painter,
            day,
            color,
            jiff::Zoned::now().day(),
            shown * opacity * mask(day.center().x),
        );
    }
    let origin = line + vec2(lead, 0.0);
    // Its text, calendar included, is titled with the tab's address, else the title itself.
    let titled = Rect::from_min_max(
        pos2(if calendar { line.x } else { origin.x }, origin.y),
        origin + galley.size(),
    )
    .intersect(content);
    let galley = if fade.is_some() {
        masked(&galley, origin.x, mask)
    } else {
        galley
    };
    paint_text(&painter, origin, galley, &style);

    // Copy link (always, off the login screen) and × (with more than one tab) appear on hover.
    let icons_visible = motion::transition(
        &ctx,
        response.id.with("icons"),
        hovered,
        0.05,
        CubicBezier::EASE,
    );
    let mut action = response.clicked().then_some(Action::Select(index));
    let mut on_button = false;
    let mut right = rect.right() - (1.0 + 3.0) * s;
    for (shown, icon, size, clicked) in [
        (
            state.closable,
            "shell/tab-close",
            vec2(8.0, 8.0 * 8.0 / 9.0),
            Action::Close(index),
        ),
        (
            !tab.login_screen,
            "shell/tab-copy",
            Vec2::splat(8.0),
            Action::CopyLink(index),
        ),
    ] {
        if !shown {
            continue;
        }
        let button = Rect::from_min_size(
            pos2(right - ICON_BUTTON * s, rect.top() + (1.0 + 3.0) * s),
            Vec2::splat(ICON_BUTTON * s),
        );
        right -= (ICON_BUTTON + 3.0) * s;
        on_button |= ui.rect_contains_pointer(button);
        if icon_button(
            ui,
            button,
            icon,
            size * s,
            icons_visible,
            colors.icon,
            colors,
            s,
        ) {
            action = Some(clicked);
        }
    }
    // The buttons are over the title, and keep its tooltip away.
    let over_title = !on_button && ui.rect_contains_pointer(titled);
    exo_ui::title(&ctx, titled, tab.email.unwrap_or(tab.title), over_title);
    action
}

/// As much of `text` (laid out in `galley`) as fits `room` on a line that breaks anywhere the
/// line-breaking rules allow with `word-break: break-all`, without the spaces it breaks after,
/// which hang.
fn first_line<'a>(galley: &Galley, text: &'a str, room: f32) -> &'a str {
    let mut options = LineBreakOptions::default();
    options.word_option = Some(LineBreakWordOption::BreakAll);
    LineSegmenter::new_for_non_complex_scripts(options)
        .segment_str(text)
        .map(|at| text[..at].trim_end())
        .take_while(|line| {
            let end = CCursor::new(line.chars().count());
            galley.pos_from_cursor(end).min.x <= room + 0.01
        })
        .last()
        .unwrap_or_default()
}

/// `galley`, painted with its left edge at `left`, with each glyph's vertices faded by `mask`
/// at where they are.
fn masked(galley: &Arc<Galley>, left: f32, mask: impl Fn(f32) -> f32) -> Arc<Galley> {
    let mut faded = (**galley).clone();
    for placed in &mut faded.rows {
        let row_left = left + placed.pos.x;
        for vertex in &mut Arc::make_mut(&mut placed.row).visuals.mesh.vertices {
            vertex.color = vertex.color.gamma_multiply(mask(row_left + vertex.pos.x));
        }
    }
    Arc::new(faded)
}

/// A 16px icon button in `color`, at `visibility` opacity. Returns whether it was clicked.
#[expect(clippy::too_many_arguments, reason = "the scale is one more")]
fn icon_button(
    ui: &mut Ui,
    button: Rect,
    icon: &str,
    glyph: Vec2,
    visibility: f32,
    color: Color32,
    colors: &Colors,
    s: f32,
) -> bool {
    let response = ui.interact(
        button,
        ui.id().with(("tab-icon", icon, button.min.x as i32)),
        Sense::click(),
    );
    let hover = motion::transition(
        ui.ctx(),
        response.id.with("hover"),
        response.hovered(),
        0.15,
        CubicBezier::EASE,
    );
    let painter = ui.painter();
    painter.rect_filled(
        button,
        CornerRadius::from(4.0 * s),
        colors.icon_hover_fill.gamma_multiply(hover * visibility),
    );
    let glyph_rect = Rect::from_center_size(button.center(), glyph);
    let color = motion::mix(color, colors.icon_hovered, hover);
    paint_icon(ui, icon, glyph_rect, color.gamma_multiply(visibility));
    visibility > 0.0 && response.clicked()
}

fn add_button(ui: &mut Ui, rect: Rect, colors: &Colors, s: f32) -> Option<Action> {
    let response = ui.interact(rect, ui.id().with("new-tab"), Sense::click());
    // The + sits at 45% until its 24px box is hovered.
    // The glyph's button over the box takes the pointer from the box.
    let hover = motion::transition(
        ui.ctx(),
        response.id.with("hover"),
        response.contains_pointer(),
        0.05,
        CubicBezier::EASE,
    );
    let button = Rect::from_min_size(
        pos2(
            rect.right() - (1.0 + 3.0 + ICON_BUTTON) * s,
            rect.top() + (1.0 + 3.0) * s,
        ),
        Vec2::splat(ICON_BUTTON * s),
    );
    let visibility = 0.45 + 0.55 * hover;
    // The glyph's button takes the clicks over it, which are the +'s too.
    let glyph = icon_button(
        ui,
        button,
        "shell/tab-add",
        Vec2::splat(8.0 * s),
        visibility,
        colors.add,
        colors,
        s,
    );
    (response.clicked() || glyph).then_some(Action::NewTab)
}

/// The account's dot: 8px with 2px corners, 6px before the title, in the account's Classic
/// colour (`accounts::COLORS`).
const ACCOUNT_DOT: f32 = 8.0;
const ACCOUNT_DOT_GAP: f32 = 6.0;

/// The tab's 16px calendar, drawn as on a 32px canvas ([`paint_calendar_day`]).
const CALENDAR_DAY: f32 = 16.0;

/// The calendar's colour for an account's colour, in the order of `accounts::COLORS`. `sea` when
/// the tab has none.
fn calendar_color(account: Option<&str>) -> Color32 {
    const CANVAS: [(&str, Color32); 12] = [
        ("thistle", Color32::from_rgb(0xbf, 0x9e, 0xfd)),
        ("orchid", Color32::from_rgb(0xf6, 0x6c, 0xdf)),
        ("bubblegum", Color32::from_rgb(0xff, 0xa4, 0xed)),
        ("sea", Color32::from_rgb(0x4a, 0x96, 0xea)),
        ("water", Color32::from_rgb(0x73, 0xc2, 0xef)),
        ("shell", Color32::from_rgb(0x2f, 0xba, 0xba)),
        ("woods", Color32::from_rgb(0x45, 0xa5, 0x87)),
        ("grass", Color32::from_rgb(0x34, 0xbb, 0x5a)),
        ("leaf", Color32::from_rgb(0x96, 0xe5, 0x31)),
        ("wine", Color32::from_rgb(0xde, 0x53, 0x53)),
        ("blush", Color32::from_rgb(0xff, 0x6d, 0x4e)),
        ("honey", Color32::from_rgb(0xff, 0x99, 0x25)),
    ];
    let named = |name: &str| {
        CANVAS
            .iter()
            .find(|(known, _)| *known == name)
            .map(|(_, color)| *color)
    };
    account
        .and_then(named)
        .or_else(|| named("sea"))
        .unwrap_or(Color32::WHITE)
}

/// Today's date, `day`, on a calendar in `color` filling `rect`, at `opacity`: the canvas's
/// rounded square (4px corners of 32), a near-black page inset 3.5px at the sides, 8px from the
/// top and 3.5px from the bottom, and the date centered on the page in white, 20px for one digit
/// and 17px for two, weight 500, centred on its em box.
fn paint_calendar_day(painter: &egui::Painter, rect: Rect, color: Color32, day: i8, opacity: f32) {
    // Canvas pixels to points.
    let unit = rect.width() / 32.0;
    painter.rect_filled(
        rect,
        CornerRadius::from(4.0 * unit),
        color.gamma_multiply(opacity),
    );
    let page = Rect::from_min_size(
        rect.min + vec2(3.5, 8.0) * unit,
        vec2(32.0 - 7.0, 32.0 - 3.0 - 0.5 - 8.0) * unit,
    );
    painter.rect_filled(
        page,
        CornerRadius::ZERO,
        Color32::from_rgb(14, 14, 14).gamma_multiply(opacity),
    );
    let digits = day.to_string();
    let size = if digits.len() == 1 { 20.0 } else { 17.0 } * unit;
    // With no line height, the line box's baseline sits half the font's ascent less its descent
    // below its top. That's the em box's middle, where the canvas puts `y`.
    let style = TextStyle::new(Font::new(Family::System, 500), size, 0.0);
    let middle = page.top() + page.height() / 2.0 + 0.5 * unit;
    let galley = exo_ui::layout(
        painter.ctx(),
        LayoutJob::single_section(digits, style.format(Color32::WHITE.gamma_multiply(opacity))),
    );
    paint_text(
        painter,
        pos2(rect.center().x - galley.size().x / 2.0, middle),
        galley,
        &style,
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_active_pill_keeps_125px_and_the_others_share_the_rest() {
        assert_eq!(pill_widths(600.0, 3, Some(1), 125.0), vec![200.0; 3]);
        assert_eq!(
            pill_widths(325.0, 5, Some(4), 125.0),
            vec![50.0, 50.0, 50.0, 50.0, 125.0]
        );
        assert_eq!(
            pill_widths(325.0, 5, None, 125.0),
            vec![65.0; 5],
            "an unfocused window has no active pill"
        );
        assert_eq!(pill_widths(100.0, 1, Some(0), 125.0), vec![125.0]);
    }

    #[test]
    fn a_long_title_shows_the_first_line_it_breaks_into() {
        let ctx = egui::Context::default();
        exo_ui::install(&ctx).expect("the system fonts load");
        // Fonts load with the first frame.
        ctx.run_ui(egui::RawInput::default(), |_| {})
            .drop_without_applying_deltas();
        let style = TextStyle::new(Font::new(Family::System, 400), 12.8, 22.0);
        let title = "Important (16) - Examail";
        let galley = exo_ui::layout(
            &ctx,
            LayoutJob::single_section(title.to_owned(), style.format(Color32::BLACK)),
        );
        let x = |text: &str| style.width(&ctx, text);
        assert_eq!(first_line(&galley, title, 1000.0), title);
        assert_eq!(
            first_line(&galley, title, x("Important (16") + 1.0),
            "Important (1",
            "no break before the `)` that follows"
        );
        assert_eq!(
            first_line(&galley, title, x("Important ") + 1.0),
            "Important",
            "the space it breaks after hangs"
        );
        assert_eq!(first_line(&galley, title, 1.0), "");
    }

    #[test]
    fn a_pills_title_is_titled_with_its_address_else_its_title() {
        let ctx = egui::Context::default();
        exo_ui::install(&ctx).expect("the system fonts load");
        let screen = Rect::from_min_size(Pos2::ZERO, vec2(1280.0, 800.0));
        let frame = |pointer: Pos2| {
            let input = egui::RawInput {
                screen_rect: Some(screen),
                events: vec![egui::Event::PointerMoved(pointer)],
                ..Default::default()
            };
            ctx.run_ui(input, |ui| {
                set_tabs(
                    ui.ctx(),
                    vec![
                        Pill {
                            title: "Examail".into(),
                            login_screen: true,
                            ..Pill::default()
                        },
                        Pill::default(),
                    ],
                    1,
                );
                let tab = Tab {
                    title: "Important (16) - Examail",
                    email: Some("maya@brightmoor.co"),
                    login_screen: false,
                    account_color: Some("sea"),
                };
                show(ui, tab, Backdrop::Page);
            })
            .drop_without_applying_deltas();
            exo_ui::take_titles(&ctx)
        };
        frame(Pos2::ZERO);
        let title = frame(Pos2::ZERO)[1].rect;
        let titles = frame(title.center());
        let texts: Vec<(&str, bool)> = titles
            .iter()
            .map(|title| (title.text.as_str(), title.hovered))
            .collect();
        assert_eq!(texts, [("Examail", false), ("maya@brightmoor.co", true)]);
    }
}

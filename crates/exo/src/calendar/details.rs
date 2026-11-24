//! The event details popover: the event's colour and title with a menu, then When, Where, Who,
//! the description, the calendar, the organizer and Private rows, and the RSVP bar.
//!
//! Everything shown comes from the event, which anyone who can invite the user can write. So the
//! description goes through exo-mail's sanitizer, and links open only as web links
//! (`format::urls::web`).

use egui::{
    Color32, CornerRadius, Id, Order, Pos2, Rect, Sense, Stroke, Ui, UiBuilder, Vec2, pos2,
    text::LayoutJob, vec2,
};
use exo_ui::{BoxShadow, Family, Font, Menu, Scrollbar, TextStyle, motion};
use gmail::calendar::{Event as ApiEvent, EventAttendee};
use jiff::Zoned;

use super::{
    description::{Line, Run, interact_link, linkify},
    event::{Event, Rsvp, conference_of},
    form::Field,
    popup::{self, Placement},
};
use crate::rsvp::{RsvpButtonMetrics, show_rsvp_buttons};

/// The popover: 464px wide.
const WIDTH: f32 = 464.0;

/// The header: `padding: 14px 20px 12px; gap: 6px`.
const HEADER_TOP: f32 = 14.0;
const HEADER_BOTTOM: f32 = 12.0;
const PADDING_X: f32 = 20.0;
/// The colour square: 10px, `margin: 7px 3px`, 2px corners.
const INDICATOR: f32 = 10.0;
const INDICATOR_AT: Vec2 = vec2(PADDING_X + 3.0, HEADER_TOP + 7.0);
/// The summary: 16px/20px in a box `padding: 2px 8px; top: 1px`, `width: calc(100% - 50px)`.
const SUMMARY: TextStyle = TextStyle::new(Font::new(Family::Sans, 400), 16.0, 20.0);
const SUMMARY_BOX: Vec2 = vec2(INDICATOR_AT.x + INDICATOR + 3.0 + 6.0, HEADER_TOP + 1.0);
const SUMMARY_PADDING: Vec2 = vec2(8.0, 2.0);
const SUMMARY_WIDTH: f32 = WIDTH - 2.0 * PADDING_X - 50.0;
/// The menu button: a 25px circle at `top: 14px; right: 15px`, with Classic's legacy dots at
/// `left: 5px; top: 11px`.
const MENU: f32 = 25.0;
const MENU_RIGHT: f32 = 15.0;
const MENU_ICON_AT: Vec2 = vec2(5.0, 11.0);
/// The menu's list hangs 8px under the button, 18px left of it.
const MENU_LIST_OFFSET: Vec2 = vec2(-18.0, 8.0);

/// A row is a 16px icon 4px down, then the value (`padding: 4px 8px; top: 1px`) in 12px/16px.
/// Rows are 8px apart, and the last is 20px above the bottom.
const GUEST_BOTTOM: f32 = 20.0;
const ROW_TEXT: TextStyle = TextStyle::new(Font::new(Family::Sans, 400), 12.0, 16.0);
const ICON: f32 = 16.0;
const ICON_TOP: f32 = 4.0;
const VALUE_LEFT: f32 = PADDING_X + ICON + 6.0 + 8.0;
const VALUE_TOP: f32 = 5.0;
const VALUE_PADDING_Y: f32 = 8.0;
/// A series' rule under When: `margin-top: 8px`.
const RULE_TOP: f32 = 8.0;
const VALUE_WIDTH: f32 = WIDTH - VALUE_LEFT - PADDING_X - 8.0;
const ROW_GAP: f32 = 8.0;
const LAST_GAP: f32 = 20.0;
/// The Who row's guests are 24px lines 32px apart, with `margin-left: 1px`. Each has a 20px avatar
/// 2px down, the name 10px after the avatar (`padding: 4px 0`), and the answer badge, 12px at
/// (12, 9).
const GUEST: f32 = 32.0;
const GUEST_LEFT: f32 = VALUE_LEFT + 1.0;
const AVATAR: f32 = 20.0;
const NAME_LEFT: f32 = GUEST_LEFT + AVATAR + 10.0;
const BADGE: f32 = 12.0;
/// The organizer row's creator line: 10px.
const CREATOR: TextStyle = TextStyle::new(Font::new(Family::Sans, 400), 10.0, 16.0);
/// The join button: 27px tall, `padding: 7px 10px 5px`, 12px weight 600, at the row's right.
const JOIN: TextStyle = TextStyle::new(Font::new(Family::Sans, 600), 12.0, 15.0);
const JOIN_HEIGHT: f32 = 27.0;
const JOIN_PADDING: [f32; 2] = [7.0, 10.0];
/// The attachments: small cards in two columns 8px apart, each 28px tall with `padding: 6px` and
/// the name 8px after the type.
const FILE_HEIGHT: f32 = 28.0;
const FILE_GAP: f32 = 8.0;
const FILE_TYPE: TextStyle = TextStyle::new(Font::new(Family::Sans, 700), 9.0, 16.0);

/// The RSVP bar has `padding: 16px 20px 16px 50px` and a 1px top border. `RSVP` in 12px bold sits
/// 34px before 26px buttons (14px bold uppercase, `padding: 0 12px`, 16px apart, `top: 1px`), all
/// centred in the bar.
const RSVP_HEIGHT: f32 = 1.0 + 16.0 + 26.0 + 16.0;
const RSVP_LEFT: f32 = 50.0;
const RSVP_LABEL: TextStyle = TextStyle::new(Font::new(Family::Sans, 700), 12.0, 16.0);
const RSVP_LABEL_GAP: f32 = 34.0;
const BUTTON: TextStyle = TextStyle::new(Font::new(Family::Sans, 700), 14.0, 24.0);
const BUTTON_HEIGHT: f32 = 26.0;
const BUTTON_PADDING: f32 = 12.0;
const BUTTON_GAP: f32 = 16.0;
/// An RSVP button's label: `padding-top: 2.5px`, and half a pixel more, as Chromium lays a
/// `<button>`'s label that much lower.
const BUTTON_TEXT_TOP: f32 = 3.0;
/// The RSVP bar's `border-top: 1px solid #ededed`, Carbon's `#5f6165`.
const RSVP_BORDER: Color32 = Color32::from_rgb(0xed, 0xed, 0xed);
const RSVP_BORDER_CARBON: Color32 = Color32::from_rgb(0x5f, 0x61, 0x65);

/// The menu's items, top to bottom.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MenuItem {
    /// `Edit`, which opens the event's form.
    Edit,
    /// `Delete`.
    Delete,
    /// `Email guest` / `Email guests`.
    EmailGuests,
    /// `Open in Google Calendar`.
    OpenInCalendar,
}

/// What the popover asked for.
#[derive(Clone, Debug, PartialEq)]
pub enum DetailsAction {
    /// My answer, to this event or (`all`) to every event of its series.
    Respond {
        answer: Rsvp,
        all: bool,
    },
    Menu(MenuItem),
    /// A link in the event, already checked to be a web link.
    Open(String),
    /// The title or a row was clicked, which opens the form with that field focused.
    Edit(Field),
    /// When's date or rule was clicked, which opens the form on the start's date.
    EditDate,
    /// A copy tooltip's text, clicked.
    Copy(String),
}

/// An answer waiting on `RSVP {answer} to recurring event`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct RecurringAlert {
    pub answer: Rsvp,
    /// 0: `This event`, 1: `All events`.
    pub choice: usize,
}

/// The open popover.
pub struct Details {
    /// The event's key ([`Event::key`]).
    pub key: String,
    /// What it's placed against: the event's day column at the block's height, or an all-day block.
    anchor: Rect,
    /// It belongs to the right pane's day view.
    pane: bool,
    placement: Placement,
    scroll: f32,
    scrollbar: Scrollbar,
    /// The open menu's highlighted row.
    menu: Option<usize>,
    /// The recurring-event alert, while it's up.
    pub alert: Option<RecurringAlert>,
    /// Opened by this frame's click, whose release mustn't close it.
    fresh: bool,
    /// Where the open menu's list is.
    menu_rect: Option<Rect>,
    /// The copy tooltip (a guest's name, the conference link), and whether its text opens what
    /// it copies (the link's does) rather than copying it.
    tip: exo_ui::CopyTooltip,
    tip_opens: bool,
}

/// The event and what the popover needs to know about it.
pub struct Shown<'a> {
    pub event: &'a Event,
    pub source: &'a ApiEvent,
    pub me: &'a str,
    /// The `Calendar` row (its name, when more than one calendar shows) or the `Label` row (`via
    /// {calendar}` for an event with its own colour).
    pub calendar: Option<&'a str>,
    pub now: &'a Zoned,
    /// I can answer (`RSVP` bar).
    pub can_respond: bool,
    pub menu: &'a [MenuItem],
    /// I can change it, so the title and rows open the form.
    pub editable: bool,
    /// A series' line under When, with its rule in words.
    pub rule: Option<String>,
    /// A guest's event, which takes 20px more under the rows (`padding-bottom: 20px`).
    pub guest: bool,
    /// Whether Exo is online, which a new call's link waits for.
    pub online: bool,
}

impl Details {
    /// Details of event `key`, placed against `anchor`, with `pane` set for the right pane's day
    /// view.
    pub fn new(key: String, anchor: Rect, pane: bool) -> Self {
        Self {
            key,
            anchor,
            pane,
            placement: Placement::default(),
            scroll: 0.0,
            scrollbar: Scrollbar::default(),
            menu: None,
            alert: None,
            fresh: true,
            menu_rect: None,
            tip: exo_ui::CopyTooltip::default(),
            tip_opens: false,
        }
    }

    /// Whether the menu is open.
    pub fn menu_open(&self) -> bool {
        self.menu.is_some()
    }

    /// Closes the menu.
    pub fn close_menu(&mut self) {
        self.menu = None;
    }

    /// Whether the pointer was released this frame outside the popover and its menu, which closes
    /// the popover.
    pub fn dismissed(&mut self, ui: &Ui, popup: Rect) -> bool {
        let fresh = std::mem::take(&mut self.fresh);
        let (released, at) =
            ui.input(|input| (input.pointer.any_released(), input.pointer.interact_pos()));
        !fresh
            && self.alert.is_none()
            && released
            && at.is_some_and(|at| {
                !popup.contains(at) && !self.menu_rect.is_some_and(|menu| menu.contains(at))
            })
    }

    /// Paints the popover on its own layer over everything but menus. `area` is the window.
    /// Returns what was asked for and where the popover is.
    pub fn show(&mut self, ui: &Ui, area: Rect, shown: &Shown<'_>) -> (Vec<DetailsAction>, Rect) {
        let ctx = ui.ctx().clone();
        let palette = exo_ui::palette(&ctx);
        let rows = rows(shown);
        let summary = SUMMARY.wrap(
            &ctx,
            &shown.event.title,
            SUMMARY_WIDTH - 2.0 * SUMMARY_PADDING.x,
        );
        let header = HEADER_TOP
            + summary.len().max(1) as f32 * SUMMARY.line_height
            + 2.0 * SUMMARY_PADDING.y
            + HEADER_BOTTOM;
        let heights: Vec<f32> = rows.iter().map(|row| row.height(&ctx)).collect();
        let body: f32 = heights.iter().sum::<f32>()
            + ROW_GAP * heights.len().saturating_sub(1) as f32
            + LAST_GAP
            + if shown.guest { GUEST_BOTTOM } else { 0.0 };
        let bar = if shown.can_respond { RSVP_HEIGHT } else { 0.0 };
        let popup = self
            .placement
            .place(area, self.anchor, WIDTH, header + body + bar, self.pane);
        let viewport = Rect::from_min_max(
            pos2(popup.left(), popup.top() + header),
            pos2(popup.right(), popup.bottom() - bar),
        );
        let mut actions = Vec::new();

        egui::Area::new(exo_ui::id(&ctx, "calendar-details"))
            .order(Order::Foreground)
            .fixed_pos(popup.min)
            .constrain(false)
            .show(&ctx, |area_ui| {
                let ui = area_ui.new_child(UiBuilder::new().max_rect(popup));
                let card = exo_ui::pixel_snap(popup);
                let painter = ui.painter().clone();
                popup::paint_card(&painter, card);
                // The popup takes the pointer.
                let _ = ui.interact(
                    popup,
                    exo_ui::id(ui.ctx(), "calendar-details-card"),
                    Sense::click_and_drag(),
                );

                // The body scrolls under the header, with the wheel and at once to a match find
                // moves to.
                let find = exo_ui::id(ui.ctx(), "calendar-details-find");
                let mut wheel = exo_ui::find::reveal(ui.ctx(), find).map_or(0.0, |delta| -delta.y);
                if ui.rect_contains_pointer(viewport) {
                    wheel += ui.input(|input| input.smooth_scroll_delta.y);
                }
                if wheel != 0.0 {
                    self.scroll =
                        (self.scroll - wheel).clamp(0.0, (body - viewport.height()).max(0.0));
                }
                let found = exo_ui::find::Scope::begin(ui.ctx(), find, viewport, self.scroll);
                let body_painter = painter.with_clip_rect(viewport);
                let mut top = viewport.top() - self.scroll;
                let mut hovered_tip = None;
                for (index, (row, height)) in rows.iter().zip(&heights).enumerate() {
                    // An editable row highlights and opens its field.
                    if shown.editable
                        && let Some(field) = row.field()
                    {
                        let value = Rect::from_min_size(
                            pos2(popup.left() + VALUE_LEFT - 8.0, top),
                            vec2(WIDTH - VALUE_LEFT + 8.0 - PADDING_X, *height),
                        )
                        .intersect(viewport);
                        let hit = ui
                            .interact(
                                value,
                                exo_ui::id(ui.ctx(), ("calendar-details-edit", index)),
                                Sense::click(),
                            )
                            .on_hover_cursor(egui::CursorIcon::PointingHand);
                        if hit.hovered() {
                            body_painter.rect_filled(
                                value,
                                CornerRadius::same(4),
                                palette.background_elevated_accent,
                            );
                        }
                        if hit.clicked() {
                            let at = hit.interact_pointer_pos().unwrap_or(value.min);
                            actions.push(
                                if row.on_date(
                                    ui.ctx(),
                                    at - pos2(popup.left() + VALUE_LEFT, top + VALUE_TOP),
                                ) {
                                    DetailsAction::EditDate
                                } else {
                                    DetailsAction::Edit(field)
                                },
                            );
                        }
                    }
                    for event in row.paint(
                        &ui,
                        &body_painter,
                        pos2(popup.left(), top),
                        viewport,
                        exo_ui::id(&ctx, ("calendar-details-row", index)),
                    ) {
                        match event {
                            RowEvent::Open(url) => actions.push(DetailsAction::Open(url)),
                            RowEvent::Hover(tip, opens) => hovered_tip = Some((tip, opens)),
                        }
                    }
                    top += height + ROW_GAP;
                }
                drop(found);
                self.scrollbar.show(
                    &ui,
                    exo_ui::id(ui.ctx(), "calendar-details-scrollbar"),
                    viewport,
                    self.scroll,
                    body,
                );
                if let Some((_, opens)) = &hovered_tip {
                    self.tip_opens = *opens;
                }
                match self.tip.show(
                    &ui,
                    exo_ui::id(ui.ctx(), "calendar-details-tip"),
                    hovered_tip.map(|(tip, _)| tip),
                ) {
                    Some((exo_ui::CopyClick::Text, tip)) if self.tip_opens => actions
                        .extend(crate::format::urls::web(&tip.copied).map(DetailsAction::Open)),
                    Some((_, tip)) => actions.push(DetailsAction::Copy(tip.copied)),
                    None => {}
                }
                exo_ui::scroll_shadow(
                    &painter,
                    exo_ui::id(painter.ctx(), "calendar-details-shadow"),
                    pos2(popup.left(), viewport.top()),
                    WIDTH,
                    self.scroll > 0.0,
                );

                // Header.
                let indicator =
                    Rect::from_min_size(popup.min + INDICATOR_AT, Vec2::splat(INDICATOR));
                // The colour square keeps Snow's fill in Carbon too.
                painter.rect_filled(
                    indicator,
                    CornerRadius::same(2),
                    shown
                        .event
                        .color
                        .block(exo_ui::Theme::Snow, exo_ui::BlockPalette::default())
                        .fill,
                );
                if shown.editable {
                    // An editable title highlights while hovered, and a click edits it.
                    let lines = summary.len().max(1) as f32;
                    let title = Rect::from_min_size(
                        popup.min + SUMMARY_BOX,
                        vec2(
                            SUMMARY_WIDTH,
                            lines * SUMMARY.line_height + 2.0 * SUMMARY_PADDING.y,
                        ),
                    );
                    let hit = ui
                        .interact(
                            title,
                            exo_ui::id(ui.ctx(), "calendar-details-title"),
                            Sense::click(),
                        )
                        .on_hover_cursor(egui::CursorIcon::Text);
                    if hit.hovered() {
                        painter.rect_filled(
                            title,
                            CornerRadius::same(4),
                            palette.background_elevated_accent,
                        );
                    }
                    if hit.clicked() {
                        actions.push(DetailsAction::Edit(Field::Title));
                    }
                }
                let mut line = popup.top() + SUMMARY_BOX.y + SUMMARY_PADDING.y;
                for text in &summary {
                    SUMMARY.paint_line(
                        &painter,
                        pos2(popup.left() + SUMMARY_BOX.x + SUMMARY_PADDING.x, line),
                        text,
                        palette.text_base_default,
                        None,
                    );
                    line += SUMMARY.line_height;
                }
                if !shown.menu.is_empty() {
                    self.menu_button(&ui, popup, shown, &mut actions);
                }

                if shown.can_respond {
                    let bar = Rect::from_min_max(
                        pos2(popup.left(), popup.bottom() - RSVP_HEIGHT),
                        popup.max,
                    );
                    if let Some(answer) = rsvp_bar(&ui, bar, shown.event.rsvp) {
                        if shown.source.recurring_event_id.is_some() {
                            self.alert = Some(RecurringAlert { answer, choice: 0 });
                        } else {
                            actions.push(DetailsAction::Respond { answer, all: false });
                        }
                    }
                }
            });
        (actions, popup)
    }

    /// The `…` button and, while open, its list.
    fn menu_button(
        &mut self,
        ui: &Ui,
        popup: Rect,
        shown: &Shown<'_>,
        actions: &mut Vec<DetailsAction>,
    ) {
        let button = Rect::from_min_size(
            pos2(popup.right() - MENU_RIGHT - MENU, popup.top() + HEADER_TOP),
            Vec2::splat(MENU),
        );
        let response = ui
            .interact(
                button,
                exo_ui::id(ui.ctx(), "calendar-details-menu"),
                Sense::click(),
            )
            .on_hover_cursor(egui::CursorIcon::PointingHand);
        let palette = exo_ui::palette(ui.ctx());
        let hover = motion::transition(
            ui.ctx(),
            response.id,
            response.hovered(),
            0.15,
            motion::CubicBezier::EASE,
        );
        if hover > 0.0 {
            for shadow in menu_shadows(palette, hover) {
                shadow.paint(ui.painter(), button, CornerRadius::same(13));
            }
            ui.painter()
                .circle_filled(button.center(), MENU / 2.0, popup::fill(palette.theme));
        }
        // The dots are black at `.2` (white in Carbon).
        let name = "legacy/DEPRECATED_ellipses";
        let size = exo_ui::icon_size(ui.ctx(), name).unwrap_or(Vec2::splat(ICON));
        exo_ui::paint_icon(
            ui,
            name,
            Rect::from_min_size(button.min + MENU_ICON_AT, size),
            palette.ink(0.2, 0.2),
        );
        if response.clicked() {
            self.menu = if self.menu.is_some() { None } else { Some(0) };
        }
        self.menu_rect = None;
        let Some(highlighted) = self.menu else { return };
        let guests = shown
            .source
            .attendees
            .iter()
            .filter(|attendee| !is_me(attendee, shown.me))
            .count();
        let labels: Vec<&str> = shown
            .menu
            .iter()
            .map(|item| match item {
                MenuItem::Edit => "Edit",
                MenuItem::Delete => "Delete",
                MenuItem::EmailGuests if guests == 1 => "Email guest",
                MenuItem::EmailGuests => "Email guests",
                MenuItem::OpenInCalendar => "Open in Google Calendar",
            })
            .collect();
        let list = Menu {
            items: &labels,
            highlighted,
        }
        .show(
            ui,
            exo_ui::id(ui.ctx(), "calendar-details-menu-list"),
            button.left_bottom() + MENU_LIST_OFFSET,
        );
        self.menu_rect = Some(list.rect);
        if let Some(hovered) = list.hovered {
            self.menu = Some(hovered);
        }
        if let Some(clicked) = list.clicked {
            self.menu = None;
            actions.push(DetailsAction::Menu(shown.menu[clicked]));
        } else if list.outside_released(ui, response.hovered()) {
            self.menu = None;
        }
    }
}

/// The menu button's hover shadow at `opacity` of the way in (`transition: box-shadow 150ms`):
/// `0 0 .5px rgba(0,0,0,.1), 0 2px 6px rgba(0,0,0,.15), 0 0 6px rgba(0,0,0,.1)` (Carbon white).
fn menu_shadows(palette: &exo_ui::Palette, opacity: f32) -> [BoxShadow; 3] {
    let black = |alpha: f32| palette.ink(alpha * opacity, alpha * opacity);
    [
        BoxShadow::new([0.0, 0.0], 0.5, 0.0, black(0.1)),
        BoxShadow::new([0.0, 2.0], 6.0, 0.0, black(0.15)),
        BoxShadow::new([0.0, 0.0], 6.0, 0.0, black(0.1)),
    ]
}

/// The RSVP bar at `bar`. Returns the answer clicked, unless it's the current one.
fn rsvp_bar(ui: &Ui, bar: Rect, current: Option<Rsvp>) -> Option<Rsvp> {
    let palette = exo_ui::palette(ui.ctx());
    let carbon = palette.theme == exo_ui::Theme::Carbon;
    let painter = ui.painter();
    painter.hline(
        bar.x_range(),
        bar.top() + 0.5,
        Stroke::new(
            1.0,
            if carbon {
                RSVP_BORDER_CARBON
            } else {
                RSVP_BORDER
            },
        ),
    );
    let middle = bar.top() + 1.0 + (bar.height() - 1.0) / 2.0;
    let label_width = RSVP_LABEL.width(ui.ctx(), "RSVP");
    // `RSVP`: `top: 3px; color: rgba(0, 0, 0, .75)`, Carbon's white at `.85`.
    RSVP_LABEL.paint_line(
        painter,
        pos2(
            bar.left() + RSVP_LEFT,
            middle - RSVP_LABEL.line_height / 2.0 + 3.0,
        ),
        "RSVP",
        palette.ink(0.75, 0.85),
        None,
    );
    let x = bar.left() + RSVP_LEFT + label_width + RSVP_LABEL_GAP;
    show_rsvp_buttons(
        ui,
        palette,
        exo_ui::id(ui.ctx(), "calendar-details-rsvp"),
        pos2(x, middle - BUTTON_HEIGHT / 2.0 + 1.0),
        current,
        None,
        RsvpButtonMetrics {
            font: BUTTON,
            height: BUTTON_HEIGHT,
            padding_x: BUTTON_PADDING,
            gap: BUTTON_GAP,
            text_top: BUTTON_TEXT_TOP,
            animated: true,
        },
    )
}

/// A row of the body.
enum Row {
    /// The times, and a series' rule on a line of its own.
    When(String, Option<String>),
    /// The call's link, its copy text, and its join button's label.
    Conference {
        label: String,
        uri: String,
        copy: String,
        join: &'static str,
    },
    /// While Calendar makes the call, the conference row has its message alone, which says
    /// whether Exo is online.
    ConferencePending(&'static str),
    Where(Vec<Run>),
    Who(Vec<Guest>),
    About(Vec<Line>),
    /// The attachments: their names and links.
    Files(Vec<(String, String)>),
    Calendar(String),
    Organizer {
        name: String,
        creator: Option<String>,
    },
    Private,
}

/// What the pointer did in a row.
enum RowEvent {
    /// Clicked a web link.
    Open(String),
    /// Over something with a copy tooltip, whose text opens what it copies.
    Hover(exo_ui::CopyTip, bool),
}

/// An attendee as the Who row lists them.
struct Guest {
    /// `Name <email>`, or the address alone.
    tip: String,
    email: String,
    name: String,
    /// `(organizer)` or `(optional)`.
    note: Option<&'static str>,
    answer: Option<Rsvp>,
}

impl Row {
    /// Whether `at` (from the value's top left) is on When's start date or its rule line,
    /// which open the form on the start's date. The times open it on the start's time.
    fn on_date(&self, ctx: &egui::Context, at: Vec2) -> bool {
        let Row::When(text, _) = self else {
            return false;
        };
        let lines = ROW_TEXT.wrap(ctx, text, VALUE_WIDTH).len() as f32;
        let date = text.split('\u{2004}').next().unwrap_or(text);
        at.y >= lines * ROW_TEXT.line_height
            || (at.y < ROW_TEXT.line_height && at.x < ROW_TEXT.width(ctx, date.trim_end()))
    }

    /// The form's field a click on the row edits.
    fn field(&self) -> Option<Field> {
        match self {
            Row::When(..) => Some(Field::When),
            Row::Where(_) => Some(Field::Location),
            Row::Who(_) => Some(Field::Guests),
            Row::About(_) => Some(Field::About),
            Row::Conference { .. } | Row::ConferencePending(_) => Some(Field::Meeting),
            Row::Files(_) | Row::Calendar(_) | Row::Organizer { .. } | Row::Private => None,
        }
    }

    fn icon(&self) -> &'static str {
        match self {
            Row::When(..) => "legacy/DEPRECATED_snooze",
            Row::Where(_) => "legacy/DEPRECATED_locationPin",
            Row::Who(_) | Row::Organizer { .. } => "legacy/DEPRECATED_guest",
            Row::About(_) => "legacy/DEPRECATED_description",
            Row::Calendar(_) => "legacy/DEPRECATED_calendar",
            Row::Conference { .. } | Row::ConferencePending(_) => "legacy/DEPRECATED_conference",
            Row::Files(_) => "legacy/DEPRECATED_paperclip",
            Row::Private => "legacy/DEPRECATED_padlock",
        }
    }

    fn height(&self, ctx: &egui::Context) -> f32 {
        let lines = match self {
            Row::Who(guests) => return guests.len() as f32 * GUEST,
            Row::Conference { .. } | Row::ConferencePending(_) => 1,
            Row::Files(files) => {
                return files.len().div_ceil(2) as f32 * (FILE_HEIGHT + FILE_GAP) - FILE_GAP + 2.0;
            }
            Row::When(text, Some(rule)) => {
                return (ROW_TEXT.wrap(ctx, text, VALUE_WIDTH).len()
                    + ROW_TEXT.wrap(ctx, rule, VALUE_WIDTH).len()) as f32
                    * ROW_TEXT.line_height
                    + RULE_TOP
                    + VALUE_PADDING_Y;
            }
            Row::When(text, None) | Row::Calendar(text) => {
                ROW_TEXT.wrap(ctx, text, VALUE_WIDTH).len()
            }
            Row::Where(runs) => layout_runs(
                ctx,
                runs,
                VALUE_WIDTH,
                Color32::PLACEHOLDER,
                Color32::PLACEHOLDER,
            )
            .0
            .rows
            .len(),
            Row::About(lines) => lines
                .iter()
                .map(|line| {
                    layout_runs(
                        ctx,
                        &line.runs,
                        VALUE_WIDTH - line.indent,
                        Color32::PLACEHOLDER,
                        Color32::PLACEHOLDER,
                    )
                    .0
                    .rows
                    .len()
                })
                .sum(),
            Row::Organizer { creator, .. } => 1 + usize::from(creator.is_some()),
            Row::Private => 1,
        };
        lines.max(1) as f32 * ROW_TEXT.line_height + VALUE_PADDING_Y
    }

    /// Paints the row with its top at `origin.y`, where `origin.x` is the card's left. Returns a
    /// link clicked.
    fn paint(
        &self,
        ui: &Ui,
        painter: &egui::Painter,
        origin: Pos2,
        clip: Rect,
        id: Id,
    ) -> Vec<RowEvent> {
        let ctx = ui.ctx();
        let palette = exo_ui::palette(ctx);
        let icon = Rect::from_min_size(origin + vec2(PADDING_X, ICON_TOP), Vec2::splat(ICON));
        if clip.intersects(icon) {
            exo_ui::paint_icon_on(
                painter,
                self.icon(),
                exo_ui::icon_rect(ctx, self.icon(), icon),
                palette.icon_base_default,
            );
        }
        let text = origin + vec2(VALUE_LEFT, VALUE_TOP);
        // The values are Classic's black (Carbon's white) at `.75`.
        let color = palette.ink(0.75, 0.75);
        match self {
            Row::When(value, _) | Row::Calendar(value) => {
                let left = if matches!(self, Row::Calendar(_)) {
                    text.x + 1.0
                } else {
                    text.x
                };
                let lines = ROW_TEXT.wrap(ctx, value, VALUE_WIDTH);
                if let Row::When(_, Some(rule)) = self {
                    // The rule: `margin-top: 8px`.
                    let top = text.y + lines.len() as f32 * ROW_TEXT.line_height + RULE_TOP;
                    for (index, line) in ROW_TEXT.wrap(ctx, rule, VALUE_WIDTH).iter().enumerate() {
                        ROW_TEXT.paint_line(
                            painter,
                            pos2(left, top + index as f32 * ROW_TEXT.line_height),
                            line,
                            color,
                            None,
                        );
                    }
                }
                for (index, line) in lines.iter().enumerate() {
                    // The font has no three-per-em space (U+2004), so it's a third of an em, as
                    // Chromium's fallback spaces it.
                    let mut x = left;
                    for (part, segment) in line.split('\u{2004}').enumerate() {
                        if part > 0 {
                            x += ROW_TEXT.size / 3.0;
                        }
                        x += ROW_TEXT.paint_line(
                            painter,
                            pos2(x, text.y + index as f32 * ROW_TEXT.line_height),
                            segment,
                            color,
                            None,
                        );
                    }
                }
                Vec::new()
            }
            Row::Conference {
                label,
                uri,
                copy,
                join,
            } => {
                let mut events = Vec::new();
                let width = ROW_TEXT.width(ctx, label).min(VALUE_WIDTH - 120.0);
                let link = Rect::from_min_size(text, vec2(width, ROW_TEXT.line_height));
                exo_ui::link_box(ctx, link.intersect(clip), uri);
                let hit = ui
                    .interact(link.intersect(clip), id.with("link"), Sense::click())
                    .on_hover_cursor(egui::CursorIcon::PointingHand);
                ROW_TEXT.paint_line(painter, text, label, palette.link_text_primary, Some(width));
                if hit.hovered() {
                    painter.hline(
                        link.x_range(),
                        link.top() + ROW_TEXT.baseline(ctx) + 1.5,
                        Stroke::new(1.0, palette.link_text_primary),
                    );
                    events.push(RowEvent::Hover(
                        exo_ui::CopyTip {
                            target: link,
                            shown: copy.clone(),
                            copied: uri.clone(),
                        },
                        true,
                    ));
                }
                if hit.clicked()
                    && let Some(url) = crate::format::urls::web(uri)
                {
                    events.push(RowEvent::Open(url));
                }
                // The join button, at the row's right.
                let button_width = JOIN.width(ctx, join) + 2.0 * JOIN_PADDING[1];
                let button = Rect::from_min_size(
                    pos2(origin.x + WIDTH - PADDING_X - button_width, origin.y),
                    vec2(button_width, JOIN_HEIGHT),
                );
                let pressed = ui
                    .interact(button.intersect(clip), id.with("join"), Sense::click())
                    .on_hover_cursor(egui::CursorIcon::PointingHand);
                let hover = motion::transition(
                    ctx,
                    pressed.id,
                    pressed.hovered(),
                    0.2,
                    motion::CubicBezier::EASE,
                );
                painter.rect_filled(
                    exo_ui::pixel_snap(button),
                    CornerRadius::same(8),
                    motion::mix(
                        palette.button_primary_fill_default,
                        palette.button_primary_fill_hover,
                        hover,
                    ),
                );
                JOIN.paint_line(
                    painter,
                    pos2(
                        button.left() + JOIN_PADDING[1],
                        button.top() + JOIN_PADDING[0],
                    ),
                    join,
                    palette.button_primary_content_default,
                    None,
                );
                if pressed.clicked()
                    && let Some(url) = crate::format::urls::web(uri)
                {
                    events.push(RowEvent::Open(url));
                }
                events
            }
            // Held changes go out once online.
            Row::ConferencePending(message) => {
                ROW_TEXT.paint_line(painter, text, message, color, None);
                Vec::new()
            }
            Row::Files(files) => {
                let mut events = Vec::new();
                let width = (VALUE_WIDTH + 8.0 - FILE_GAP) / 2.0;
                for (index, (name, url)) in files.iter().enumerate() {
                    let at = pos2(
                        origin.x + VALUE_LEFT - 8.0 + (index % 2) as f32 * (width + FILE_GAP),
                        origin.y + 1.0 + (index / 2) as f32 * (FILE_HEIGHT + FILE_GAP),
                    );
                    let card = Rect::from_min_size(at, vec2(width, FILE_HEIGHT));
                    let hit = ui
                        .interact(
                            card.intersect(clip),
                            id.with(("file", index)),
                            Sense::click(),
                        )
                        .on_hover_cursor(egui::CursorIcon::PointingHand);
                    painter.rect_filled(
                        exo_ui::pixel_snap(card),
                        CornerRadius::same(4),
                        palette.background_elevated_accent,
                    );
                    let kind = name
                        .rsplit_once('.')
                        .map(|(_, extension)| extension.to_uppercase())
                        .unwrap_or_default();
                    let mut x = card.left() + 6.0;
                    if !kind.is_empty() {
                        let badge = Rect::from_min_size(
                            pos2(x, card.top() + 6.0),
                            vec2(FILE_TYPE.width(ctx, &kind) + 8.0, 16.0),
                        );
                        painter.rect_filled(
                            exo_ui::pixel_snap(badge),
                            CornerRadius::same(2),
                            palette.background_base_default,
                        );
                        FILE_TYPE.paint_line(
                            painter,
                            pos2(badge.left() + 4.0, badge.top()),
                            &kind,
                            palette.text_base_subdued,
                            None,
                        );
                        x = badge.right() + 8.0;
                    }
                    let stem = name
                        .rsplit_once('.')
                        .map_or(name.as_str(), |(stem, _)| stem);
                    ROW_TEXT.paint_line(
                        painter,
                        pos2(x, card.top() + 6.0 + 1.0),
                        stem,
                        palette.text_base_subdued,
                        Some(card.right() - 6.0 - x),
                    );
                    if hit.clicked()
                        && let Some(url) = crate::format::urls::web(url)
                    {
                        events.push(RowEvent::Open(url));
                    }
                }
                events
            }
            Row::Where(runs) => paint_runs(ui, painter, text, VALUE_WIDTH, runs, clip, id)
                .map(RowEvent::Open)
                .into_iter()
                .collect(),
            Row::About(lines) => {
                let mut top = text.y;
                let mut clicked = None;
                for (index, line) in lines.iter().enumerate() {
                    let at = pos2(text.x + line.indent, top);
                    clicked = clicked.or(paint_runs(
                        ui,
                        painter,
                        at,
                        VALUE_WIDTH - line.indent,
                        &line.runs,
                        clip,
                        id.with(index),
                    ));
                    top += layout_runs(ctx, &line.runs, VALUE_WIDTH - line.indent, color, color)
                        .0
                        .rows
                        .len()
                        .max(1) as f32
                        * ROW_TEXT.line_height;
                }
                clicked.map(RowEvent::Open).into_iter().collect()
            }
            Row::Who(guests) => {
                let mut events = Vec::new();
                for (index, guest) in guests.iter().enumerate() {
                    let top = text.y + index as f32 * GUEST;
                    paint_avatar(
                        painter,
                        pos2(origin.x + GUEST_LEFT, top + 2.0),
                        &guest.email,
                        guest.answer,
                        palette,
                    );
                    let name = pos2(origin.x + NAME_LEFT, top + 4.0);
                    let width = ROW_TEXT.paint_line(
                        painter,
                        name,
                        &guest.name,
                        color,
                        Some(WIDTH - NAME_LEFT - PADDING_X - 8.0),
                    );
                    let mut end = width;
                    if let Some(note) = guest.note {
                        end += 4.0
                            + ROW_TEXT.paint_line(
                                painter,
                                name + vec2(width + 4.0, 0.0),
                                note,
                                palette.text_base_subdued,
                                None,
                            );
                    }
                    // The name's tooltip, `Name <email>`.
                    let target = Rect::from_min_size(name, vec2(end, ROW_TEXT.line_height));
                    if ui.rect_contains_pointer(target.intersect(clip)) {
                        events.push(RowEvent::Hover(
                            exo_ui::CopyTip {
                                target,
                                shown: guest.tip.clone(),
                                copied: guest.email.clone(),
                            },
                            false,
                        ));
                    }
                }
                events
            }
            Row::Organizer { name, creator } => {
                ROW_TEXT.paint_line(painter, text, name, color, None);
                if let Some(creator) = creator {
                    CREATOR.paint_line(
                        painter,
                        text + vec2(0.0, ROW_TEXT.line_height),
                        &format!("Created by: {creator}"),
                        color,
                        None,
                    );
                }
                Vec::new()
            }
            Row::Private => {
                ROW_TEXT.paint_line(painter, text, "Private", color, None);
                Vec::new()
            }
        }
    }
}

/// `runs` laid out `width` wide, and the link of each section, by section.
fn layout_runs(
    ctx: &egui::Context,
    runs: &[Run],
    width: f32,
    color: Color32,
    link: Color32,
) -> (std::sync::Arc<egui::Galley>, Vec<Option<String>>) {
    let bold = TextStyle::new(Font::new(Family::Sans, 700), 12.0, 16.0);
    let mut job = LayoutJob::default();
    let mut links = Vec::new();
    for run in runs {
        let style = if run.bold { bold } else { ROW_TEXT };
        job.append(
            &run.text,
            0.0,
            style.format(if run.link.is_some() { link } else { color }),
        );
        links.push(run.link.clone());
    }
    job.wrap.max_width = width;
    // `word-break: break-word`: a word wider than the line breaks.
    job.wrap.break_anywhere = false;
    (exo_ui::layout(ctx, job), links)
}

/// Paints `runs` wrapped `width` wide at `at`, links in the link colour and underlined when
/// hovered. Returns a link clicked, if it's a web link.
fn paint_runs(
    ui: &Ui,
    painter: &egui::Painter,
    at: Pos2,
    width: f32,
    runs: &[Run],
    clip: Rect,
    id: Id,
) -> Option<String> {
    let palette = exo_ui::palette(ui.ctx());
    let (galley, links) = layout_runs(
        ui.ctx(),
        runs,
        width,
        palette.ink(0.75, 0.75),
        palette.link_text_primary,
    );
    // Each run's characters, in order, since egui lays out a glyph per character.
    let mut ends = Vec::new();
    let mut count = 0;
    for run in runs {
        count += run.text.chars().count();
        ends.push(count);
    }
    let run_of = |char_index: usize| ends.iter().position(|end| char_index < *end);
    let mut clicked = None;
    for (index, link) in links.iter().enumerate() {
        let Some(link) = link else { continue };
        let mut row_start = 0;
        let mut pieces = Vec::new();
        for placed in &galley.rows {
            let glyphs: Vec<_> = placed
                .glyphs
                .iter()
                .enumerate()
                .filter(|(offset, _)| run_of(row_start + offset) == Some(index))
                .map(|(_, glyph)| glyph)
                .collect();
            row_start += placed.char_count_including_newline().0;
            let (Some(first), Some(last)) = (glyphs.first(), glyphs.last()) else {
                continue;
            };
            let rect = placed.rect().translate(at.to_vec2());
            pieces.push(Rect::from_x_y_ranges(
                at.x + placed.pos.x + first.pos.x
                    ..=at.x + placed.pos.x + last.pos.x + last.advance_width,
                rect.y_range(),
            ));
        }
        let mut hovered = false;
        for (piece, rect) in pieces.iter().enumerate() {
            let response = interact_link(ui, rect.intersect(clip), id.with((index, piece)), link);
            hovered |= response.hovered();
            if response.clicked() {
                clicked = crate::format::urls::web(link);
            }
        }
        if hovered {
            for rect in &pieces {
                painter.hline(
                    rect.x_range(),
                    rect.top() + ROW_TEXT.baseline(ui.ctx()) + 1.5,
                    Stroke::new(1.0, palette.link_text_primary),
                );
            }
        }
    }
    exo_ui::paint_text(painter, at, galley, &ROW_TEXT);
    clicked
}

/// `email`'s 20px avatar, their photo when Google has one, and, for an answer, its 12px badge at
/// (12, 9), as the details' and the form's guests have them.
pub(super) fn paint_avatar(
    painter: &egui::Painter,
    at: Pos2,
    email: &str,
    answer: Option<Rsvp>,
    palette: &exo_ui::Palette,
) {
    crate::people::paint_avatar(
        painter,
        Rect::from_min_size(at, Vec2::splat(AVATAR)),
        email,
        true,
    );
    // The answer's badge: its disc `#6ac074`, `#fa7a7a` or `#ffd84f`, the mark in `#f8f8f8`,
    // Carbon `#383b40`.
    let (icon, disc) = match answer {
        Some(Rsvp::Accepted) => ("legacy/DEPRECATED_rsvpAccepted", (0x6a, 0xc0, 0x74)),
        Some(Rsvp::Declined) => ("legacy/DEPRECATED_rsvpDeclined", (0xfa, 0x7a, 0x7a)),
        Some(Rsvp::Tentative) => ("legacy/DEPRECATED_rsvpTentative", (0xff, 0xd8, 0x4f)),
        Some(Rsvp::NeedsAction) | None => return,
    };
    let mark = match palette.theme {
        exo_ui::Theme::Snow => Color32::from_rgb(0xf8, 0xf8, 0xf8),
        exo_ui::Theme::Carbon => Color32::from_rgb(0x38, 0x3b, 0x40),
    };
    exo_ui::paint_icon_two_tone_on(
        painter,
        icon,
        Rect::from_min_size(at + vec2(12.0, 9.0), Vec2::splat(BADGE)),
        mark,
        Color32::from_rgb(disc.0, disc.1, disc.2),
    );
}

/// Whether `attendee` is me.
fn is_me(attendee: &EventAttendee, me: &str) -> bool {
    attendee.is_self == Some(true)
        || attendee
            .email
            .as_deref()
            .is_some_and(|email| email.eq_ignore_ascii_case(me))
}

/// The body's rows, top to bottom.
fn rows(shown: &Shown<'_>) -> Vec<Row> {
    let source = shown.source;
    // A series with a rule says it under the times, in place of ` (recurring)`.
    let recurring = source.recurring_event_id.is_some();
    let mut rows = vec![Row::When(
        when(shown.event, recurring && shown.rule.is_none(), shown.now),
        shown.rule.clone().filter(|_| recurring),
    )];
    let conference = conference(source, shown.me);
    if let Some(row) = &conference {
        rows.push(Row::Conference {
            label: row.label.clone(),
            uri: row.uri.clone(),
            copy: row.copy.clone(),
            join: row.join,
        });
    } else if source.conference_data.as_ref().is_some_and(|data| {
        data.get("createRequest").is_some() && data.get("entryPoints").is_none()
    }) {
        rows.push(Row::ConferencePending(if shown.online {
            "Link will be added momentarily"
        } else {
            "Link will be added when online"
        }));
    }
    // A place that's only the call's link isn't said twice.
    if let Some(location) = source.location.as_deref().filter(|location| {
        !location.trim().is_empty()
            && conference
                .as_ref()
                .is_none_or(|row| row.uri != *location && row.raw != *location)
    }) {
        rows.push(Row::Where(linkify(location)));
    }
    let guests = guests(source);
    if !guests.is_empty() {
        rows.push(Row::Who(guests));
    }
    let about = super::description::lines(source.description.as_deref().unwrap_or_default());
    if !about.is_empty() {
        rows.push(Row::About(about));
    }
    let files: Vec<(String, String)> = source
        .attachments
        .iter()
        .filter_map(|file| Some((file.title.clone()?, file.file_url.clone()?)))
        .collect();
    if !files.is_empty() {
        rows.push(Row::Files(files));
    }
    if let Some(calendar) = shown.calendar {
        rows.push(Row::Calendar(calendar.to_owned()));
    }
    if let Some(organizer) = &source.organizer
        && organizer.is_self != Some(true)
        && !source
            .attendees
            .iter()
            .any(|attendee| attendee.organizer == Some(true))
        && let Some(name) = organizer
            .display_name
            .clone()
            .or_else(|| organizer.email.clone())
    {
        let creator = source.creator.as_ref().filter(|creator| {
            !creator
                .email
                .as_deref()
                .unwrap_or_default()
                .eq_ignore_ascii_case(organizer.email.as_deref().unwrap_or_default())
        });
        rows.push(Row::Organizer {
            name,
            creator: creator.and_then(|creator| {
                creator
                    .display_name
                    .clone()
                    .or_else(|| creator.email.clone())
            }),
        });
    }
    if matches!(
        source.visibility.as_deref(),
        Some("private" | "confidential")
    ) {
        rows.push(Row::Private);
    }
    rows
}

/// The guests: the organizer, then accepted, declined, tentative and unanswered, each group by
/// name. Rooms aren't guests.
fn guests(source: &ApiEvent) -> Vec<Guest> {
    let mut guests: Vec<(u8, String, Guest)> = source
        .attendees
        .iter()
        .filter(|attendee| attendee.resource != Some(true))
        .filter_map(|attendee| {
            let email = attendee.email.clone()?;
            let name = attendee
                .display_name
                .clone()
                .filter(|name| !name.is_empty())
                .unwrap_or_else(|| email.clone());
            let tip = if name == email {
                email.clone()
            } else {
                format!("{name} <{email}>")
            };
            let answer = Rsvp::parse_opt(attendee.response_status.as_deref())
                .filter(|a| *a != Rsvp::NeedsAction);
            let organizer = attendee.organizer == Some(true);
            let rank = if organizer {
                0
            } else {
                match answer {
                    Some(Rsvp::Accepted) => 1,
                    Some(Rsvp::Declined) => 2,
                    Some(Rsvp::Tentative) => 3,
                    _ => 4,
                }
            };
            let note = if organizer {
                Some("(organizer)")
            } else if attendee.optional == Some(true) {
                Some("(optional)")
            } else {
                None
            };
            Some((
                rank,
                name.to_lowercase(),
                Guest {
                    tip,
                    email,
                    name,
                    note,
                    answer,
                },
            ))
        })
        .collect();
    guests.sort_by(|a, b| (a.0, &a.1).cmp(&(b.0, &b.1)));
    guests.into_iter().map(|(_, _, guest)| guest).collect()
}

/// The When row: `Thu, Nov 19 · 2:00 PM – 3:00 PM`, with the end's date when it's another day,
/// `(all day)` for a single all-day day, and ` (recurring)` for a series.
fn when(event: &Event, recurring: bool, now: &Zoned) -> String {
    crate::format::dates::event_details_range(
        &event.start,
        &event.end,
        event.all_day,
        recurring,
        now,
    )
}

/// The event's call (a video entry point first), if it has `conferenceData` or a call URL in its
/// description. Only links Exo would open count.
fn conference(source: &ApiEvent, me: &str) -> Option<super::event::ResolvedConference> {
    conference_of(source, me)
}

#[cfg(test)]
mod tests {
    use super::*;
    use jiff::tz::TimeZone;

    fn event(start: &str, end: &str, all_day: bool) -> Event {
        let tz = TimeZone::get("America/New_York").expect("zone");
        let at = |text: &str| {
            text.parse::<jiff::civil::DateTime>()
                .expect("time")
                .to_zoned(tz.clone())
                .expect("exists")
        };
        let (start, end) = (at(start), at(end));
        let span_row =
            all_day || end.timestamp().duration_since(start.timestamp()).as_hours() >= 24;
        Event {
            key: String::new(),
            title: String::new(),
            start,
            end,
            all_day,
            span_row,
            color: exo_ui::CalendarColor::BLUE,
            rsvp: None,
            busy: Rsvp::Accepted,
            everyone_else_declined: false,
            ical_uid: None,
            calendar_id: String::new(),
            id: None,
            editable: false,
            time_text: None,
            share: None,
            tooltip: None,
        }
    }

    #[test]
    fn when_reads_its_dates_times_and_series() {
        let now = event("2026-11-17T09:00", "2026-11-17T10:00", false).start;
        assert_eq!(
            when(
                &event("2026-11-19T14:00", "2026-11-19T15:00", false),
                false,
                &now
            ),
            "Thu, Nov 19 \u{2004}·\u{2004}2:00 PM\u{2004}–\u{2004}3:00 PM"
        );
        assert_eq!(
            when(
                &event("2026-11-17T15:00", "2026-11-17T16:00", false),
                true,
                &now
            ),
            "Tue, Nov 17 \u{2004}·\u{2004}3:00 PM\u{2004}–\u{2004}4:00 PM (recurring)"
        );
        assert_eq!(
            when(
                &event("2026-11-26T00:00", "2026-11-27T00:00", true),
                false,
                &now
            ),
            "Thu, Nov 26 (all day)"
        );
        assert_eq!(
            when(
                &event("2026-11-26T00:00", "2026-11-28T00:00", true),
                false,
                &now
            ),
            "Thu, Nov 26 \u{2004}–\u{2004}Fri, Nov 27 "
        );
        let twenty_six_hours = event("2026-12-09T09:00", "2026-12-10T11:00", false);
        assert!(twenty_six_hours.in_all_day_row() && !twenty_six_hours.all_day);
        assert_eq!(
            when(&twenty_six_hours, false, &now),
            "Wed, Dec 9 \u{2004}·\u{2004}9:00 AM\u{2004}–\u{2004}Thu, Dec 10 \u{2004}·\u{2004}11:00 AM"
        );
        assert_eq!(
            when(
                &event("2027-01-04T23:00", "2027-01-05T01:00", false),
                false,
                &now
            ),
            "Mon, Jan 4, 2027 \u{2004}·\u{2004}11:00 PM\u{2004}–\u{2004}Tue, Jan 5, 2027 \u{2004}·\u{2004}1:00 AM"
        );
    }

    #[test]
    fn a_meet_call_joins_as_this_account() {
        let source = ApiEvent {
            conference_data: Some(serde_json::json!({
                "entryPoints": [{ "entryPointType": "phone", "uri": "tel:+1-555-0100" }, { "entryPointType": "video", "uri": "https://meet.google.com/abc-defg-hij", "label": "meet.google.com/abc-defg-hij" }],
                "conferenceSolution": { "key": { "type": "hangoutsMeet" }, "name": "Google Meet" },
            })),
            ..ApiEvent::default()
        };
        let row = conference(&source, "maya@brightmoor.co").expect("a call");
        assert_eq!(
            (row.label.as_str(), row.join),
            ("meet.google.com/abc-defg-hij", "Join Google Meet")
        );
        assert_eq!(
            row.uri,
            "https://meet.google.com/abc-defg-hij?authuser=maya%40brightmoor.co"
        );
        assert!(
            conference(
                &ApiEvent {
                    hangout_link: Some("https://meet.google.com/x".into()),
                    ..ApiEvent::default()
                },
                "me"
            )
            .is_none(),
            "a bare hangoutLink has no row"
        );
    }

    #[test]
    fn locations_link_their_urls() {
        let runs = linkify("Room 4 / https://meet.example.com/abc");
        assert_eq!(runs.len(), 2);
        assert_eq!(
            runs[1].link.as_deref(),
            Some("https://meet.example.com/abc")
        );
    }
}

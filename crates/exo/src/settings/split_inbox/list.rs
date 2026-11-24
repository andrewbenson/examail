//! The Split Inbox Settings overview screen: Active and Inactive tabs, the Hide Empty link,
//! subheader formatting, and the sortable drag-and-drop split list.

use egui::{Color32, Context, Rect, Sense, Ui, pos2, vec2};
use exo_store::{ListId, Settings, SplitEntry, SystemSplit};
use exo_ui::{
    ActionKind, DIALOG_ACTION_HEIGHT, DIALOG_WIDTH, DialogAction, motion, paint_icon, palette,
};

use super::mutations::active_rows;
use super::{BODY, BODY_BOLD, CONTENT_WIDTH, Event, ListTab, NUDGE, SMALL};

/// A row: `padding: 4px 32px 8px` around a title and subheader line.
const ROW_HEIGHT: f32 = 52.0;
/// A row's gear, `padding: 6px` around 14px, 7px past the content (`margin-right: -7px`), and its
/// drag handle, 6px around 12×10.
const GEAR_SIZE: f32 = 26.0;
const HANDLE_SIZE: [f32; 2] = [24.0, 22.0];
/// The tabs: a 4px-down 16px label, its 7px padding and 1px underline (Classic), and the
/// container's `padding-bottom: 3px` and 1px border.
const TABS_HEIGHT: f32 = 28.0;
const INTRO: &str = "Customize your splits, create new ones, or explore the library to find your ideal workflow and focus on what matters most.";
/// `Add Split Inbox`: `width: 116px` inside `padding: 8px 12px 4px`.
const ADD_WIDTH: f32 = 116.0 + 24.0;

/// A row of the list: an entry, or the Inactive tab's `Important • Other` while they're off.
#[derive(Clone, Copy)]
enum Row<'a> {
    Entry(SplitEntry<'a>),
    ImportantOtherOff,
}

impl Row<'_> {
    fn list(&self) -> ListId {
        match self {
            Self::Entry(entry) => entry.list(),
            Self::ImportantOtherOff => ListId::Other,
        }
    }

    fn id(&self) -> String {
        match self {
            Self::Entry(entry) => entry.external_id().to_owned(),
            Self::ImportantOtherOff => "important-other-off".to_owned(),
        }
    }
}

/// The rows of `tab`: Active's entries, or the splits that are off, after a row for Important and
/// Other while they're off.
fn rows(settings: &Settings, tab: ListTab) -> Vec<Row<'_>> {
    match tab {
        ListTab::Active => active_rows(settings).into_iter().map(Row::Entry).collect(),
        ListTab::Inactive => settings
            .unified_inbox
            .then_some(Row::ImportantOtherOff)
            .into_iter()
            .chain(
                settings
                    .splits()
                    .filter(|split| split.is_disabled)
                    .map(|split| Row::Entry(SplitEntry::Split(split))),
            )
            .collect(),
    }
}

/// Where the scrolling part starts, from the body's top. It's under the intro and Add Split Inbox
/// (20px under the header), the tabs and their 20px gap.
fn list_head(ctx: &Context) -> f32 {
    20.0 + intro_height(ctx) + 20.0 + TABS_HEIGHT
}

/// Around the rows: the scrolling part's `padding: 12px 0`, and on the Active tab the sortable
/// list's `padding: 4px 0 8px`.
fn rows_padding(tab: ListTab) -> [f32; 2] {
    match tab {
        ListTab::Active => [16.0, 20.0],
        ListTab::Inactive => [12.0, 12.0],
    }
}

/// The scrolling part's natural height.
fn content_height(settings: &Settings, tab: ListTab) -> f32 {
    let [top, bottom] = rows_padding(tab);
    let rows = rows(settings, tab).len();
    // The empty Inactive tab's notice: `margin: 4px 32px 8px` around a line.
    let rows = if rows == 0 {
        32.0
    } else {
        ROW_HEIGHT * rows as f32
    };
    top + rows + bottom
}

/// The list's body height.
pub(super) fn height(ctx: &Context, settings: &Settings, tab: ListTab) -> f32 {
    list_head(ctx) + content_height(settings, tab)
}

fn intro_lines(ctx: &Context) -> Vec<String> {
    // The header row is a flex row: the text, a 20px gap, the button 13px past the edge.
    BODY.wrap(ctx, INTRO, CONTENT_WIDTH - 20.0 - ADD_WIDTH + 13.0)
}

/// The header row: the intro, or the 32px button when that's taller.
fn intro_height(ctx: &Context) -> f32 {
    (intro_lines(ctx).len() as f32 * 20.0).max(DIALOG_ACTION_HEIGHT)
}

/// Split Inbox Settings: the intro and Add Split Inbox, the Active and Inactive tabs with the Hide
/// Empty link, and the tab's rows.
pub(super) fn show(
    ui: &mut Ui,
    body: Rect,
    settings: &Settings,
    tab: ListTab,
    blurred: &mut bool,
    (scroll, scrollbar): (&mut f32, &mut exo_ui::Scrollbar),
    drag: &mut Option<(usize, f32)>,
) -> Option<Event> {
    let ctx = ui.ctx().clone();
    *blurred |= ui.input(|input| input.pointer.any_pressed());
    let tokens = palette(&ctx);
    let painter = ui.painter().clone();
    let (left, right) = (body.left(), body.right());
    let mut event = None;

    let row_top = body.top() + 20.0;
    for (i, line) in intro_lines(&ctx).iter().enumerate() {
        BODY.paint_line(
            &painter,
            pos2(left, row_top + NUDGE + 20.0 * i as f32),
            line,
            tokens.text_base_default,
            None,
        );
    }
    let add = Rect::from_min_size(
        pos2(right + 13.0 - ADD_WIDTH, row_top + 4.0),
        vec2(ADD_WIDTH, DIALOG_ACTION_HEIGHT),
    );
    // It has the keyboard when the list shows. Enter anywhere runs it.
    if DialogAction::new("Add Split Inbox", ActionKind::Primary)
        .focused(!*blurred)
        .show(ui, exo_ui::id(ui.ctx(), "split-inbox-add"), add)
        .clicked()
    {
        event = Some(Event::AddSplit);
    }

    // The tabs sit 4px down in their row, with labels at `rgba(0, 0, 0, .4)`, or `.8` focused.
    // Only Carbon underlines the focused tab, over the row's border.
    let tabs_top = row_top + intro_height(&ctx) + 20.0;
    divider(
        &painter,
        left,
        right,
        tabs_top + TABS_HEIGHT - 1.0,
        tokens.border_base_default,
    );
    let underline = exo_ui::app_theme(&ctx) == exo_ui::Theme::Carbon;
    let mut x = left;
    for (label, which) in [("Active", ListTab::Active), ("Inactive", ListTab::Inactive)] {
        let width = SMALL.width(&ctx, label);
        let rect = Rect::from_min_size(pos2(x, tabs_top + 4.0), vec2(width, 24.0));
        let focused = which == tab;
        SMALL.paint_line(
            &painter,
            pos2(x, rect.top() + 1.0),
            label,
            if focused {
                tokens.ink(0.8, 0.8)
            } else {
                tokens.ink(0.4, 0.4)
            },
            None,
        );
        if focused && underline {
            painter.rect_filled(
                Rect::from_min_max(pos2(rect.left(), rect.bottom() - 1.0), rect.max),
                0.0,
                tokens.ink(0.8, 0.8),
            );
        }
        if ui
            .interact(
                rect,
                exo_ui::id(ui.ctx(), ("split-inbox-tab", label)),
                Sense::click(),
            )
            .on_hover_cursor(egui::CursorIcon::PointingHand)
            .clicked()
        {
            event = Some(Event::SelectTab(which));
        }
        x = rect.right() + 24.0;
    }
    // `Hide Empty Split Inboxes`, right-aligned and centered on the tabs' line, `top: 1px`.
    let label = "Hide Empty Split Inboxes";
    let width = SMALL.width(&ctx, label);
    let link = Rect::from_min_size(pos2(right - width, tabs_top + 5.0), vec2(width, 16.0));
    SMALL.paint_line(&painter, link.min, label, tokens.link_text_primary, None);
    if ui
        .interact(
            link,
            exo_ui::id(ui.ctx(), "split-inbox-hide-empty"),
            Sense::click(),
        )
        .on_hover_cursor(egui::CursorIcon::PointingHand)
        .clicked()
    {
        event = Some(Event::ShowHideEmpty);
    }

    // The scrolling part, full-bleed, down to the body's bottom.
    let rows = rows(settings, tab);
    let area_top = tabs_top + TABS_HEIGHT;
    let natural = content_height(settings, tab);
    let area = Rect::from_min_max(
        pos2(left - 32.0, area_top),
        pos2(right + 32.0, (area_top + natural).min(body.bottom())),
    );
    // The wheel scrolls the splits, and find at once to a match out of view.
    let find = exo_ui::id(ui.ctx(), "split-inbox-settings-find");
    let mut wheel = exo_ui::find::reveal(ui.ctx(), find).map_or(0.0, |delta| -delta.y);
    if ui.rect_contains_pointer(area) {
        wheel += ui.input(|input| input.smooth_scroll_delta.y);
    }
    if wheel != 0.0 {
        *scroll = (*scroll - wheel).clamp(0.0, (natural - area.height()).max(0.0));
    }
    let _found = exo_ui::find::Scope::begin(ui.ctx(), find, area, *scroll);
    let mut area_ui = ui.new_child(egui::UiBuilder::new().max_rect(area));
    area_ui.set_clip_rect(area.intersect(ui.clip_rect()));
    let area_painter = area_ui.painter().clone();
    let rows_top = area_top + rows_padding(tab)[0] - *scroll;

    if rows.is_empty() {
        let notice = match tab {
            ListTab::Active => "When you add a Split, it will appear here.",
            ListTab::Inactive => "When you turn off a Split, it will appear here.",
        };
        // The notice: `rgba(0, 0, 0, .4)`.
        BODY.paint_line(
            &area_painter,
            pos2(left, rows_top + 4.0 + NUDGE),
            notice,
            tokens.ink(0.4, 0.4),
            None,
        );
    }
    let sortable = tab == ListTab::Active;
    let metrics = exo_ui::SortableRowMetrics {
        first_top: rows_top,
        row_step: ROW_HEIGHT,
        row_height: ROW_HEIGHT,
        card_left: left - 32.0,
        card_width: DIALOG_WIDTH,
    };
    if let Some((from, to)) = exo_ui::SortableList::new(rows.len(), metrics)
        .sortable(sortable)
        .accent(tokens.border_base_active)
        .show(
            &mut area_ui,
            drag,
            |index| exo_ui::id(&ctx, ("split-row-shift", rows[index].id())),
            |area_ui, index, top, is_dragging, drag| {
                if let Some(clicked) = split_row(
                    area_ui,
                    &rows[index],
                    index,
                    top,
                    left,
                    right,
                    sortable,
                    is_dragging,
                    drag,
                ) {
                    event = Some(clicked);
                }
            },
            |area_ui, from, top| {
                paint_row_text(
                    area_ui.painter(),
                    &rows[from],
                    top,
                    left,
                    CONTENT_WIDTH - 49.0,
                    tokens,
                );
            },
        )
    {
        event = Some(Event::Reorder(from, to));
    }
    // A list taller than the dialog scrolls, with a scrollbar.
    scrollbar.show(
        &area_ui,
        exo_ui::id(area_ui.ctx(), "split-inbox-scrollbar"),
        area,
        *scroll,
        natural,
    );
    event
}

/// A 1px rule across the content at `y`.
fn divider(painter: &egui::Painter, left: f32, right: f32, y: f32, color: Color32) {
    painter.rect_filled(
        Rect::from_min_max(pos2(left, y), pos2(right, y + 1.0)),
        0.0,
        color,
    );
}

/// A row's subheader.
fn subheader(row: &Row<'_>) -> String {
    let split = match row {
        Row::ImportantOtherOff | Row::Entry(SplitEntry::System(SystemSplit::Other)) => {
            return "Split conversations from marketing, social networks, and automatic updates"
                .into();
        }
        Row::Entry(SplitEntry::System(SystemSplit::Important)) => {
            return "Direct, personal, and high priority messages".into();
        }
        Row::Entry(SplitEntry::System(SystemSplit::Inbox)) => {
            return "Your latest conversations".into();
        }
        Row::Entry(SplitEntry::Split(split)) => split,
    };
    let query = split.query();
    let items = || {
        query
            .split(" OR ")
            .map(|item| item.trim_start_matches("from:"))
            .collect::<Vec<_>>()
    };
    match split.kind.as_deref() {
        Some("team") => {
            let stripped = query
                .trim_end_matches(" to:me")
                .trim_start_matches('(')
                .trim_end_matches(')');
            let items: Vec<&str> = stripped
                .split(" OR ")
                .map(|item| item.trim_start_matches("from:"))
                .filter(|item| !item.is_empty())
                .collect();
            match items.len() {
                0 => "Be responsive to your colleagues".into(),
                1 => query.to_owned(),
                _ => format!("{} to:me", items.join(", ")),
            }
        }
        Some("vip") if query.is_empty() => "Be responsive to specific senders".into(),
        Some("news") if query.is_empty() => "News, articles, and newsletters".into(),
        Some("vip" | "news") => items().join(", "),
        Some("calendar") => "Never miss an invitation again".into(),
        Some("shared") => "View all shared conversations".into(),
        _ if split.template_id.as_deref() == Some("splitTemplate_reminders") => {
            "Reminders that are ready to follow up".into()
        }
        _ => {
            let words = query
                .split(|c: char| !c.is_ascii_alphanumeric())
                .any(|word| word.eq_ignore_ascii_case("and") || word.eq_ignore_ascii_case("or"));
            if words {
                format!("({query})")
            } else {
                query.to_owned()
            }
        }
    }
}

/// Paints a row's title and subheader, `width` wide.
fn paint_row_text(
    painter: &egui::Painter,
    row: &Row<'_>,
    top: f32,
    left: f32,
    width: f32,
    tokens: &exo_ui::Palette,
) {
    let name = match row {
        Row::Entry(entry) => entry.name(),
        Row::ImportantOtherOff => "Important • Other",
    };
    BODY_BOLD.paint_line(
        painter,
        pos2(left, top + 4.0 + NUDGE),
        name,
        tokens.text_base_default,
        Some(width),
    );
    // An empty subheader is a non-breaking space, so the row keeps its height.
    BODY.paint_line(
        painter,
        pos2(left, top + 24.0 + NUDGE),
        &subheader(row),
        tokens.text_base_subdued,
        Some(width),
    );
}

/// A row with its title and subheader, filled while hovered. While hovered it also shows its drag
/// handle (Active only) and gear. A click anywhere opens its settings.
#[expect(
    clippy::too_many_arguments,
    reason = "one row of the list, drawn from the list's layout"
)]
fn split_row(
    ui: &mut Ui,
    row: &Row<'_>,
    index: usize,
    top: f32,
    left: f32,
    right: f32,
    sortable: bool,
    dragging: bool,
    drag: &mut Option<(usize, f32)>,
) -> Option<Event> {
    let ctx = ui.ctx().clone();
    let tokens = palette(&ctx);
    let painter = ui.painter().clone();
    let id = row.id();
    let rect = Rect::from_min_size(pos2(left - 32.0, top), vec2(DIALOG_WIDTH, ROW_HEIGHT));
    let gear = Rect::from_min_size(
        pos2(right + 7.0 - GEAR_SIZE, top + 13.0),
        vec2(GEAR_SIZE, GEAR_SIZE),
    );
    let handle = Rect::from_min_size(
        pos2(gear.left() - HANDLE_SIZE[0], top + 15.0),
        HANDLE_SIZE.into(),
    );
    let handle_response = sortable.then(|| {
        ui.interact(
            handle,
            exo_ui::id(ui.ctx(), ("split-row-handle", &id)),
            Sense::drag(),
        )
        .on_hover_cursor(egui::CursorIcon::Grab)
    });
    let gear_response = ui
        .interact(
            gear,
            exo_ui::id(ui.ctx(), ("split-row-gear", &id)),
            Sense::click(),
        )
        .on_hover_cursor(egui::CursorIcon::PointingHand);
    let response = ui
        .interact(
            rect,
            exo_ui::id(ui.ctx(), ("split-row", &id)),
            Sense::click(),
        )
        .on_hover_cursor(egui::CursorIcon::PointingHand);
    let hovered = !dragging && ui.rect_contains_pointer(rect);
    if hovered {
        // Hovered: `#f2f4fa`, `#dfe3f1` in Carbon.
        let fill = match exo_ui::app_theme(&ctx) {
            exo_ui::Theme::Snow => tokens.fill_active_default,
            exo_ui::Theme::Carbon => Color32::from_rgb(0xdf, 0xe3, 0xf1),
        };
        painter.rect_filled(rect, 0.0, fill);
    }
    let actions_width = if sortable {
        GEAR_SIZE + HANDLE_SIZE[0] - 7.0
    } else {
        GEAR_SIZE - 7.0
    };
    paint_row_text(
        &painter,
        row,
        top,
        left,
        CONTENT_WIDTH - actions_width,
        tokens,
    );

    // The handle and gear fade in while the row is hovered (`transition: opacity 150ms ease`).
    let shown = motion::transition(
        &ctx,
        exo_ui::id(&ctx, ("split-row-actions", &id)),
        hovered,
        0.15,
        motion::CubicBezier::EASE,
    );
    if shown > 0.0 {
        let actions = handle_response
            .as_ref()
            .map(|response| (handle, "legacy/DEPRECATED_dragHandle", response))
            .into_iter()
            .chain([(gear, "legacy/DEPRECATED_settings", &gear_response)]);
        for (rect, icon, response) in actions {
            let color = if response.hovered() {
                tokens.icon_base_hover
            } else {
                tokens.icon_base_default
            };
            paint_icon(
                ui,
                icon,
                exo_ui::icon_rect(&ctx, icon, rect),
                color.gamma_multiply(shown),
            );
        }
    }
    if let Some(handle) = &handle_response
        && handle.drag_started()
    {
        *drag = Some((index, handle.drag_delta().y));
        return None;
    }
    let on_handle = handle_response.is_some_and(|handle| handle.hovered());
    (gear_response.clicked() || (response.clicked() && !on_handle))
        .then(|| Event::OpenRow(row.list()))
}

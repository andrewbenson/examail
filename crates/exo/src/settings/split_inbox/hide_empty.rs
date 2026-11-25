//! The Hide Empty Split Inboxes screen and its per-split settings helpers.

use egui::{Color32, Rect, Sense, Ui, pos2, vec2};
use exo_store::{ListId, Settings, SplitEntry, SplitInbox, SystemSplit};
use exo_ui::{CHECKBOX_SIZE, DIALOG_WIDTH, paint_checkbox, palette};

use super::mutations::active_rows;
use super::{BODY, Event, NUDGE, SMALL};

/// A Hide Empty Split Inboxes row: `padding: 7px 64px` around a 16px label, with a 27px checkbox.
const HIDE_ROW_HEIGHT: f32 = 40.0;

/// Hide Empty's rows: the enabled splits and Other, but not Important or Inbox.
pub(super) fn rows(settings: &Settings) -> Vec<SplitEntry<'_>> {
    active_rows(settings)
        .into_iter()
        .filter(|entry| {
            !matches!(
                entry,
                SplitEntry::System(SystemSplit::Important | SystemSplit::Inbox)
            )
        })
        .collect()
}

/// Makes the row for `list` hide when empty, or not.
pub(super) fn set_hides_when_empty(settings: &mut Settings, list: &ListId, hide: bool) {
    match list {
        ListId::Split(id) => {
            settings.update_split(id, |split| split.hide_split_when_empty = Some(hide))
        }
        _ => settings.set_hide_other_when_empty(hide),
    }
}

/// Whether the row for `list` hides when empty.
pub(super) fn hides_when_empty(settings: &Settings, list: &ListId) -> bool {
    match list {
        ListId::Split(id) => settings.split(id).is_some_and(SplitInbox::hides_when_empty),
        _ => settings.hides_other_when_empty(),
    }
}

pub(super) fn height(settings: &Settings) -> f32 {
    let rows = rows(settings).len();
    if rows == 0 {
        // The empty notice: `padding: 8px 0` around a 20px line.
        20.0 + 36.0
    } else {
        // The subheader row (20 + 8 below), the rows, the list's -20px bottom margin.
        20.0 + 28.0 + HIDE_ROW_HEIGHT * rows as f32 - 20.0
    }
}

/// Hide Empty Split Inboxes: a checkbox row per enabled split and Other, applied as it's clicked.
pub(super) fn show(ui: &mut Ui, body: Rect, settings: &Settings) -> Option<Event> {
    let ctx = ui.ctx().clone();
    let tokens = palette(&ctx);
    let painter = ui.painter().clone();
    let (left, right) = (body.left(), body.right());
    let top = body.top() + 20.0;
    let rows = rows(settings);
    if rows.is_empty() {
        // The empty notice: `rgba(0, 0, 0, .55)`.
        BODY.paint_line(
            &painter,
            pos2(left, top + 8.0 + NUDGE),
            "No active Split Inboxes to hide when empty.",
            tokens.ink(0.55, 0.55),
            None,
        );
        return None;
    }
    let mut event = None;
    BODY.paint_line(
        &painter,
        pos2(left, top + NUDGE),
        "Choose which Split Inboxes are hidden when they have no messages.",
        tokens.text_base_default,
        None,
    );
    let all = rows
        .iter()
        .all(|entry| hides_when_empty(settings, &entry.list()));
    let select = if all { "Unselect all" } else { "Select all" };
    let width = BODY.width(&ctx, select);
    let select_rect = Rect::from_min_size(pos2(right - width, top + NUDGE), vec2(width, 20.0));
    BODY.paint_line(
        &painter,
        select_rect.min,
        select,
        tokens.link_text_primary,
        None,
    );
    if ui
        .interact(
            select_rect,
            exo_ui::id(ui.ctx(), "hide-empty-select-all"),
            Sense::click(),
        )
        .on_hover_cursor(egui::CursorIcon::PointingHand)
        .clicked()
    {
        event = Some(Event::HideAll);
    }
    for (i, entry) in rows.iter().enumerate() {
        let row_top = top + 28.0 + HIDE_ROW_HEIGHT * i as f32;
        // Full-bleed rows, filled while hovered.
        let row = Rect::from_min_size(
            pos2(left - 32.0, row_top),
            vec2(DIALOG_WIDTH, HIDE_ROW_HEIGHT),
        );
        let response = ui
            .interact(
                row,
                exo_ui::id(ui.ctx(), ("hide-empty-row", entry.external_id())),
                Sense::click(),
            )
            .on_hover_cursor(egui::CursorIcon::PointingHand);
        if response.hovered() {
            // Hovered: `hsl(220, 45%, 97%)`, `#e8e9f0` in Carbon.
            let fill = match exo_ui::app_theme(&ctx) {
                exo_ui::Theme::Snow => tokens.fill_hover_default,
                exo_ui::Theme::Carbon => Color32::from_rgb(0xe8, 0xe9, 0xf0),
            };
            painter.rect_filled(row, 0.0, fill);
        }
        if response.clicked() {
            event = Some(Event::ToggleHideEmpty(entry.list()));
        }
        SMALL.paint_line(
            &painter,
            pos2(left, row_top + 7.0 + 6.0),
            entry.name(),
            tokens.text_base_default,
            None,
        );
        paint_checkbox(
            &painter,
            pos2(right + 7.0 - CHECKBOX_SIZE - 6.0, row_top + 13.0),
            hides_when_empty(settings, &entry.list()),
            tokens,
        );
    }
    event
}

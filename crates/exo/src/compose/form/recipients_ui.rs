//! `To` / `Cc` / `Bcc` chip row layout, chip rendering, selection, in-place editing, drag-drop,
//! and the recipient suggestion popup.

use egui::{Color32, CornerRadius, Id, Pos2, Rect, Sense, Ui, Vec2, pos2, vec2};
use exo_store::Address;

use super::{
    CHIP_EXTRA, CHIP_HEIGHT, CHIP_TEXT_X, CHIP_TEXT_Y, Context, FIELD, LABEL_WIDTH, LINE, Response,
    Row, SUGGESTION, SUGGESTION_CELL_PADDING, SUGGESTION_NAME_MIN, SUGGESTION_PADDING,
    SUGGESTION_ROW, focus_if, header, ink, is_dark,
};
use crate::compose::{
    Action, chips, contacts,
    draft::{Field, Input, List, Open},
    recipients,
};

/// The input after the chips. The autosize field is as wide as its text (in whole px) plus 2px,
/// inside `padding-left: 6px` and a 1px border. The input wraps as a box 26px wider than the
/// field.
const INPUT_BORDER_BOX: f32 = 2.0 + 6.0 + 2.0;
const INPUT_SCROLL: f32 = 2.0 + 26.0;
/// The drop cursor: 2px wide, `1.5em` tall, `#6fbeea`, fading in and out over 150ms.
const DROP_CURSOR: Vec2 = Vec2::new(2.0, 21.0);
const DROP_CURSOR_COLOR: Color32 = Color32::from_rgb(0x6f, 0xbe, 0xea);

/// Lays out one row's chips and input, wrapping like `flex-wrap`.
pub(super) fn lay_out_row(
    ctx: &egui::Context,
    open: &Open,
    list: Option<List>,
    top: f32,
    line_width: f32,
) -> Row {
    let mut chips = Vec::new();
    let mut line = 0;
    let mut x = LABEL_WIDTH;
    let line_top = |line: usize| top + line as f32 * LINE;
    let (addresses, text, editing, selecting): (Vec<Address>, &str, Option<usize>, bool) =
        match list {
            Some(list) => {
                let input = open.input(list);
                (
                    open.list(list).clone(),
                    input.text.as_str(),
                    input.editing,
                    !input.selected.is_empty(),
                )
            }
            None => (Vec::new(), "", None, false),
        };
    let text_width = FIELD.width(ctx, text).round();
    let input_width = text_width + INPUT_SCROLL;
    let input_size = vec2(text_width + INPUT_BORDER_BOX, CHIP_HEIGHT);
    let mut input = None;
    for (index, address) in addresses.iter().enumerate() {
        // The chip being edited is the input, as wide as its text.
        let label = chip_label(address);
        let chip_width = if editing == Some(index) {
            input_width
        } else {
            FIELD.width(ctx, &label) + CHIP_EXTRA
        };
        if x + chip_width > line_width && x > LABEL_WIDTH {
            line += 1;
            x = LABEL_WIDTH;
        }
        if editing == Some(index) {
            input = Some(Rect::from_min_size(pos2(x, line_top(line)), input_size));
            x += chip_width;
            continue;
        }
        chips.push((
            index,
            Rect::from_min_size(pos2(x, line_top(line)), vec2(chip_width, CHIP_HEIGHT)),
            label,
        ));
        x += chip_width + 1.0;
    }
    if list.is_some() && editing.is_none() && !selecting {
        if x + input_width > line_width && x > LABEL_WIDTH {
            line += 1;
            x = LABEL_WIDTH;
        }
        // An empty list's input sits 1px higher (`top: -1px`) while nothing is typed.
        let nudge = if addresses.is_empty() && text.is_empty() {
            -1.0
        } else {
            0.0
        };
        input = Some(Rect::from_min_size(
            pos2(x, line_top(line) + nudge),
            input_size,
        ));
    }
    Row {
        list,
        top,
        lines: line + 1,
        chips,
        input,
    }
}

/// A chip's text: the name, else the lower-cased address, else what was typed.
pub(super) fn chip_label(address: &Address) -> String {
    match &address.name {
        Some(name) if !name.is_empty() => name.clone(),
        _ if address.email.is_empty() => String::new(),
        _ => address.email.to_lowercase(),
    }
}

/// Draws one recipient row (or From) and handles its clicks and typing.
#[expect(
    clippy::too_many_arguments,
    reason = "one row of the form, drawn from its layout"
)]
pub(super) fn show_row(
    ui: &mut Ui,
    open: &mut Open,
    row: &Row,
    origin: Pos2,
    base: Id,
    cx: &Context<'_>,
    response: &mut Response,
    now: f64,
) {
    let ctx = ui.ctx().clone();
    let palette = exo_ui::palette(&ctx);
    let dark = is_dark(&ctx);
    let label = match row.list {
        Some(list) => list.label(),
        None => "From",
    };
    let top = origin.y + row.top;
    let label_hit = Rect::from_min_size(pos2(origin.x, top), vec2(LABEL_WIDTH, CHIP_HEIGHT));
    // The label's transparent 1px border puts its text 1px in.
    FIELD.paint_line(
        ui.painter(),
        pos2(origin.x + 1.0, top + CHIP_TEXT_Y),
        label,
        ink(&ctx, 0.8),
        None,
    );
    let field = row.list.map_or(Field::From, List::field);
    let focused_here = cx.focused && open.state.field == field;
    // From's label focuses From only when From hasn't the keyboard.
    if ui
        .interact(label_hit, base.with(("label", label)), Sense::click())
        .on_hover_cursor(egui::CursorIcon::PointingHand)
        .clicked()
        && !(row.list.is_none() && focused_here)
    {
        response.actions.push(Action::Focus(field));
    }
    let Some(list) = row.list else {
        header::from_row(
            ui,
            open,
            pos2(origin.x, top),
            cx,
            base.with("from"),
            response,
        );
        return;
    };
    let selected = open.input(list).selected.clone();
    let caret_at_end = focused_here && selected.is_empty() && open.input(list).editing.is_none();
    let count = row.chips.len();
    let mut remove = None;
    for (index, rect, text) in &row.chips {
        let chip = rect.translate(origin.to_vec2());
        let address = &open.list(list)[*index];
        let invalid = !recipients::sendable(address);
        let id = base.with(("chip", list as usize, *index));
        let hit = ui
            .interact(chip, id, Sense::click_and_drag())
            .on_hover_cursor(egui::CursorIcon::Move);
        let hovered = hit.hovered();
        let is_selected = focused_here && selected.contains(index);
        // A hovered, selected or invalid chip's colours (literals).
        let (background, border) = if invalid {
            (
                Some(Color32::from_rgb(0xef, 0x8e, 0x8e)),
                Some(if is_selected {
                    Color32::from_rgb(126, 30, 30)
                } else {
                    Color32::from_rgb(0xe2, 0x86, 0x86)
                }),
            )
        } else if hovered || is_selected {
            let (fill, edge) = if dark {
                (
                    Color32::from_rgb(0x55, 0x5b, 0x64),
                    Color32::from_rgb(0x63, 0x69, 0x73),
                )
            } else {
                (
                    Color32::from_rgb(0xfa, 0xfa, 0xfa),
                    Color32::from_rgb(0xf3, 0xf3, 0xf3),
                )
            };
            (
                Some(fill),
                Some(if is_selected {
                    Color32::from_rgb(0xae, 0xb1, 0xdd)
                } else {
                    edge
                }),
            )
        } else {
            (None, None)
        };
        let painter = ui.painter_at(chip);
        let snapped = exo_ui::pixel_snap(chip);
        if let Some(fill) = background {
            painter.rect_filled(snapped, CornerRadius::same(2), fill);
        }
        if let Some(edge) = border {
            painter.rect_stroke(
                snapped.shrink(0.5),
                CornerRadius::same(2),
                egui::Stroke::new(1.0, edge),
                egui::StrokeKind::Middle,
            );
        }
        let color = if invalid {
            Color32::WHITE
        } else {
            ink(&ctx, 0.8)
        };
        FIELD.paint_line(
            &painter,
            pos2(chip.left() + CHIP_TEXT_X, chip.top() + CHIP_TEXT_Y),
            text,
            color,
            Some(chip.width() - CHIP_EXTRA + 1.0),
        );
        // The tail: `right: -14px`, translated -6px (-14px when shown). The ✕ is 9px, and the
        // `•` sits 7px back over it.
        let shown = hovered || is_selected || invalid;
        let tail_left = chip.right() - 1.0 + 14.0 - 30.0 - if shown { 14.0 } else { 6.0 };
        if shown {
            let fill = background.unwrap_or(Color32::TRANSPARENT);
            exo_ui::paint_fade_in(
                &painter,
                Rect::from_min_max(
                    pos2(tail_left, chip.top() + 1.0),
                    pos2(chip.right() - 1.0, chip.bottom() - 1.0),
                ),
                fill,
            );
            let cross = Rect::from_min_size(
                pos2(tail_left + 14.0, chip.top() + CHIP_TEXT_Y + 6.0),
                vec2(9.0, 8.0),
            );
            let cross_color = if invalid {
                Color32::WHITE
            } else {
                Color32::from_rgb(0xb9, 0xb9, 0xb9)
            };
            exo_ui::paint_icon(ui, "legacy/close", cross, cross_color);
            let tail = Rect::from_min_size(pos2(tail_left, chip.top()), vec2(30.0, chip.height()));
            if ui
                .interact(tail.intersect(chip), id.with("remove"), Sense::click())
                .on_hover_cursor(egui::CursorIcon::PointingHand)
                .clicked()
            {
                remove = Some(*index);
            }
        }
        let last = *index + 1 == count;
        let separator_hidden = shown || (last && !caret_at_end);
        if !separator_hidden {
            let separator = if invalid {
                Color32::from_white_alpha(51)
            } else {
                ink(&ctx, 0.2)
            };
            FIELD.paint_line(
                ui.painter(),
                pos2(tail_left + 14.0 + 9.0 - 7.0, chip.top() + CHIP_TEXT_Y),
                "•",
                separator,
                None,
            );
        }
        // Mouse-down selects the chip, and a double click edits it in place.
        if hit.is_pointer_button_down_on()
            && ctx.input(|input| input.pointer.primary_pressed())
            && remove.is_none()
        {
            let modifiers = ctx.input(|input| input.modifiers);
            chips::press(open, list, *index, modifiers, cx.book, now);
            response.actions.push(Action::Focus(field));
        }
        if hit.double_clicked() {
            chips::start_edit(open, list, *index);
            response.actions.push(Action::Focus(field));
            return;
        }
        // Chips can be dragged.
        if hit.drag_started() {
            let grab = ctx
                .input(|input| input.pointer.press_origin())
                .map_or(Vec2::ZERO, |at| at - chip.min);
            chips::start_drag(
                open,
                list,
                *index,
                (text.clone(), chip.size(), grab),
                cx.book,
                now,
            );
        }
    }
    drop_target(ui, open, row, list, origin, base, response);
    if let Some(index) = remove {
        open.list_mut(list).remove(index);
        let input = open.input_mut(list);
        input.selected.clear();
        if input.editing.is_some_and(|editing| editing >= index) {
            input.editing = input.editing.and_then(|editing| editing.checked_sub(1));
        }
        open.touch(now);
    }
    // The input goes after the chips or in place of the one being edited. There's none while
    // chips are selected, when nothing typed goes anywhere but a paste replaces them.
    let Some(input) = row.input.map(|input| input.translate(origin.to_vec2())) else {
        if focused_here {
            chips::paste_over(ui, open, list, cx.book, now);
        }
        return;
    };
    let line = Rect::from_min_size(
        pos2(input.left() + 7.0, input.top() + CHIP_TEXT_Y),
        vec2(input.width() - 8.0, 20.0),
    );
    let id = base.with(("input", list as usize));
    let before = open.input(list).text.clone();
    let mut text = before.clone();
    let field_response = exo_ui::text_field(
        ui,
        id,
        &mut text,
        line,
        &FIELD,
        ink(&ctx, 0.8),
        palette.text_selection_default,
    );
    if field_response.gained_focus() {
        open.header.scroll.reveal = true;
    }
    let range = egui::text_edit::TextEditState::load(ui.ctx(), id)
        .and_then(|state| state.cursor.char_range());
    open.input_mut(list).caret = range
        .filter(|range| range.primary == range.secondary)
        .map(|range| range.primary.index.0);
    if text != before {
        open.input_mut(list).text = text;
        open.header.scroll.reveal = true;
        open.input_mut(list).selected.clear();
        open.input_mut(list).just_excluded = None;
        if before.is_empty() && open.input(list).text.contains([',', ';', '\n']) {
            // A pasted list of addresses becomes chips.
            commit_input(open, now, cx.book);
        }
        open.touch(now);
    }
    // Suggestions follow the text while the field has the keyboard.
    if focused_here {
        let state = open.input_mut(list);
        if state.text != state.query {
            state.query.clone_from(&state.text);
            state.closed = false;
            state.highlight = 0;
            let found = match cx.book {
                Some(book) => contacts::suggest_sourced(
                    book,
                    cx.thread_people.unwrap_or(&contacts::Index::default()),
                    &state.text,
                ),
                None => Vec::new(),
            };
            (state.suggestions, state.from_thread) = found.into_iter().unzip();
        }
        if open.input(list).suggesting() {
            let existing: Vec<String> = open
                .list(list)
                .iter()
                .map(|address| address.email.to_lowercase())
                .collect();
            match suggestions(
                ui,
                input,
                open.input_mut(list),
                &existing,
                base.with(("suggestions", list as usize)),
            ) {
                exo_ui::ContactListResponse {
                    removed: Some(index),
                    ..
                } => response.actions.push(Action::RemoveSuggestion(index)),
                exo_ui::ContactListResponse {
                    pressed: Some(picked),
                    ..
                } => response.actions.push(Action::PickSuggestion(picked)),
                // The pointer moving onto a row highlights it.
                exo_ui::ContactListResponse {
                    hovered: Some(hovered),
                    ..
                } if hovered != open.input(list).highlight => {
                    response.actions.push(Action::HighlightSuggestion(hovered));
                }
                _ => {}
            }
        } else {
            // The list is gone, so the next one is sized afresh.
            open.input_mut(list).list_width = 0.0;
        }
    } else if !open.input(list).suggestions.is_empty() {
        let input = open.input_mut(list);
        input.suggestions.clear();
        input.query.clear();
    }
    // Clicking the row's empty space puts the caret at the end.
    let row_rect = Rect::from_min_size(
        pos2(origin.x + LABEL_WIDTH, top),
        vec2(1e4, row.lines as f32 * LINE),
    )
    .intersect(ui.clip_rect());
    let row_click = ui.interact(row_rect, base.with(("row", list as usize)), Sense::click());
    if field_response.gained_focus() || field_response.clicked() || row_click.clicked() {
        open.input_mut(list).selected.clear();
        response.actions.push(Action::Focus(field));
    }
    focus_if(ui, cx, open.state.field == field, id);
    if focused_here && std::mem::take(&mut open.input_mut(list).select_text) {
        let all = egui::text::CCursorRange::two(
            egui::text::CCursor::new(0),
            egui::text::CCursor::new(open.input(list).text.chars().count()),
        );
        exo_ui::focus_field(ui.ctx(), id, all);
    }
}

/// A recipient list as a drop target while chips are dragged. Over the list, the drop cursor
/// shows where the chips would go, and letting go there drops them.
fn drop_target(
    ui: &Ui,
    open: &Open,
    row: &Row,
    list: List,
    origin: Pos2,
    base: Id,
    response: &mut Response,
) {
    let ctx = ui.ctx();
    let area = Rect::from_min_size(
        pos2(origin.x, origin.y + row.top),
        vec2(1e4, row.lines as f32 * LINE),
    )
    .intersect(ui.clip_rect());
    let id = base.with(("drop-cursor", list as usize));
    let place = open
        .header
        .chip_drag
        .as_ref()
        .and(ctx.input(|input| input.pointer.hover_pos()))
        .filter(|at| area.contains(*at))
        .map(|at| {
            let chips: Vec<Rect> = row
                .chips
                .iter()
                .map(|(_, rect, _)| rect.translate(origin.to_vec2()))
                .collect();
            drop_place(&chips, area, at)
        });
    if let Some((_, at)) = place {
        ctx.data_mut(|data| data.insert_temp(id, at));
    }
    let shown = exo_ui::motion::transition(
        ctx,
        id,
        place.is_some(),
        0.15,
        exo_ui::motion::CubicBezier::EASE_IN,
    );
    if shown > 0.0
        && let Some(at) = ctx.data(|data| data.get_temp::<Pos2>(id))
    {
        ui.painter().rect_filled(
            Rect::from_min_size(at, DROP_CURSOR),
            0.0,
            DROP_CURSOR_COLOR.gamma_multiply(shown),
        );
    }
    if let (Some((at, _)), Some(drag)) = (place, &open.header.chip_drag)
        && ctx.input(|input| input.pointer.primary_released())
    {
        response.actions.push(Action::DropChips {
            from: drag.list,
            chips: drag.chips.clone(),
            into: list,
            at,
        });
    }
}

/// For the pointer at `at` over a list at `list` with chips at `chips` (in order), the place a
/// drop goes and the cursor's top left. The row is the lowest whose top is above the pointer.
/// The chip is the first on that row ending right of the pointer, else the row's last. The
/// cursor goes a pixel before the chip, or after it when the pointer is past the chip's middle.
/// An empty list's cursor is at the label's width.
fn drop_place(chips: &[Rect], list: Rect, at: Pos2) -> (usize, Pos2) {
    let row = chips
        .iter()
        .rev()
        .map(|chip| chip.top())
        .find(|top| at.y > *top);
    let on_row: Vec<(usize, Rect)> = chips
        .iter()
        .copied()
        .enumerate()
        .filter(|(_, chip)| Some(chip.top()) == row)
        .collect();
    let fallback = (
        0,
        Rect::from_min_size(pos2(list.left() + LABEL_WIDTH, list.top()), vec2(2.0, 0.0)),
    );
    let (rank, chip) = on_row
        .iter()
        .find(|(_, chip)| chip.right() > at.x)
        .or(on_row.last())
        .copied()
        .unwrap_or(fallback);
    let (x, rank) = if chips.is_empty() {
        (chip.left() - 1.0, 0)
    } else if at.x < chip.left() + chip.width() / 2.0 {
        (chip.left() - 1.0, rank)
    } else {
        (chip.right() + 1.0, rank + 1)
    };
    (
        rank,
        pos2(
            list.left() + (x - list.left()).floor(),
            list.top() + (chip.top() - list.top() + 2.0).floor(),
        ),
    )
}

/// The image that follows the pointer while chips are dragged, at 75% opacity. One chip shows
/// as the grabbed chip, selected, kept where it was grabbed. Several show as a stack of three
/// chips reading `Multiple recipients`, with its top left at the pointer.
pub(super) fn paint_drag_image(ui: &Ui, drag: &chips::Drag) {
    let ctx = ui.ctx();
    let Some(pointer) = ctx.input(|input| input.pointer.hover_pos()) else {
        return;
    };
    let mut painter = ctx.layer_painter(egui::LayerId::new(
        egui::Order::Tooltip,
        Id::new("compose-chip-drag"),
    ));
    painter.set_opacity(0.75);
    // A selected chip's colours (Classic literals).
    let (fill, edge) = if is_dark(ctx) {
        (
            Color32::from_rgb(0x55, 0x5b, 0x64),
            Color32::from_rgb(0xae, 0xb1, 0xdd),
        )
    } else {
        (
            Color32::from_rgb(0xfa, 0xfa, 0xfa),
            Color32::from_rgb(0xae, 0xb1, 0xdd),
        )
    };
    let chip = |rect: Rect, edge: Color32| {
        let snapped = exo_ui::pixel_snap(rect);
        painter.rect_filled(snapped, CornerRadius::same(2), fill);
        painter.rect_stroke(
            snapped.shrink(0.5),
            CornerRadius::same(2),
            egui::Stroke::new(1.0, edge),
            egui::StrokeKind::Middle,
        );
    };
    let (label, size, grab) = &drag.grabbed;
    if drag.chips.len() > 1 {
        // Three 135×28 chips 3px apart, back to front, edged at half strength.
        let faint = edge.gamma_multiply(0.5);
        for step in [6.0, 3.0, 0.0] {
            chip(
                Rect::from_min_size(pointer + vec2(step, step), vec2(135.0, 28.0)),
                faint,
            );
        }
        FIELD.paint_line(
            &painter,
            pointer + vec2(7.0, CHIP_TEXT_Y),
            "Multiple recipients",
            ink(ctx, 0.7),
            None,
        );
        return;
    }
    let rect = Rect::from_min_size(pointer - *grab, *size);
    chip(rect, edge);
    FIELD.paint_line(
        &painter,
        pos2(rect.left() + CHIP_TEXT_X, rect.top() + CHIP_TEXT_Y),
        label,
        ink(ctx, 0.8),
        Some(rect.width() - CHIP_EXTRA + 1.0),
    );
    // The shown tail and its ✕, as [`show_row`] draws them.
    exo_ui::paint_fade_in(
        &painter,
        Rect::from_min_max(
            pos2(rect.right() - 31.0, rect.top() + 1.0),
            pos2(rect.right() - 1.0, rect.bottom() - 1.0),
        ),
        fill,
    );
    let cross = Rect::from_min_size(
        pos2(rect.right() - 17.0, rect.top() + CHIP_TEXT_Y + 6.0),
        vec2(9.0, 8.0),
    );
    exo_ui::paint_icon_on(
        &painter,
        "legacy/close",
        cross,
        Color32::from_rgb(0xb9, 0xb9, 0xb9),
    );
}

/// The suggestions under the input `input` (on screen), as `ContactSelectorList` draws them.
/// There's a name column and an address column with the typed text dark in each, and the
/// highlighted row is filled, with `Remove Suggestion` on it while hovered unless the row comes
/// from the thread. Says which row the pointer picked (mouse-down picks), removed or moved onto.
fn suggestions(
    ui: &Ui,
    input: Rect,
    state: &mut Input,
    existing: &[String],
    id: Id,
) -> exo_ui::ContactListResponse {
    let ctx = ui.ctx().clone();
    let names: Vec<String> = state
        .suggestions
        .iter()
        .map(|address| {
            address
                .name
                .clone()
                .filter(|name| !name.is_empty())
                .unwrap_or_else(|| address.email.clone())
        })
        .collect();
    let name_text = names
        .iter()
        .map(|name| SUGGESTION.width_unkerned(&ctx, name))
        .fold(0.0, f32::max);
    let email_text = state
        .suggestions
        .iter()
        .map(|address| SUGGESTION.width_unkerned(&ctx, &address.email))
        .fold(0.0, f32::max);
    let (name_width, email_width) =
        suggestion_columns(name_text, email_text, &mut state.list_width);
    let rows: Vec<exo_ui::ContactRow<'_>> = state
        .suggestions
        .iter()
        .zip(&names)
        .map(|(address, name)| exo_ui::ContactRow {
            name,
            email: &address.email,
            disabled: existing.contains(&address.email.to_lowercase()),
            avatar: None,
        })
        .collect();
    let pad = SUGGESTION_CELL_PADDING;
    let style = exo_ui::ContactListStyle {
        text: SUGGESTION,
        row: SUGGESTION_ROW,
        padding: SUGGESTION_PADDING,
        columns: exo_ui::ContactColumns {
            avatar: None,
            name: (pad, name_width - 2.0 * pad),
            email: (name_width + pad, email_width - 2.0 * pad),
        },
        force_snow: false,
    };
    let query = state.text.trim().trim_start_matches("to:").trim();
    let removable: Vec<bool> = state.from_thread.iter().map(|thread| !thread).collect();
    let list = exo_ui::ContactSelectorList::new(&rows, query, Some(state.highlight), style)
        .removable(&removable);
    list.show(
        &ctx,
        id,
        pos2(input.left() - 10.0, input.bottom() + 6.0),
        state.list_width,
        egui::Order::Foreground,
    )
}

/// The suggestion list's columns for its widest name and address (`*_text`), given the width
/// the list has had since it opened (`seen`, which this updates). Each cell is its text padded
/// 16px a side, and the name's `min-width: 165px` counts the padding
/// (`box-sizing: border-box`). The list keeps the widest rows it has shown since it opened, and
/// the extra width is shared between the columns in proportion to their widths.
fn suggestion_columns(name_text: f32, email_text: f32, seen: &mut f32) -> (f32, f32) {
    let name = (name_text + 2.0 * SUGGESTION_CELL_PADDING).max(SUGGESTION_NAME_MIN);
    let email = email_text + 2.0 * SUGGESTION_CELL_PADDING;
    *seen = seen.max(name + email);
    let grow = *seen / (name + email);
    (name * grow, email * grow)
}

/// Commits the focused field's typed text as chips (a pasted list splits into several, with
/// what isn't an address left typed).
pub(in crate::compose) fn commit_input(open: &mut Open, now: f64, book: Option<&contacts::Index>) {
    let Some(list) = open.state.field.list() else {
        return;
    };
    // The input moves on without coming into view, because committing sets its value rather
    // than typing.
    open.header.scroll.reveal = false;
    if chips::finish_in_place(open, list, book, now) {
        return;
    }
    // The highlighted suggestion, when suggestions show.
    if open.input(list).suggesting() {
        let input = open.input_mut(list);
        let picked = input.suggestions[input.highlight.min(input.suggestions.len() - 1)].clone();
        input.text.clear();
        input.suggestions.clear();
        input.query.clear();
        input.selected.clear();
        if !open
            .list(list)
            .iter()
            .any(|address| address.email.eq_ignore_ascii_case(&picked.email))
        {
            open.list_mut(list).push(picked);
        }
        open.touch(now);
        return;
    }
    let text = std::mem::take(&mut open.input_mut(list).text);
    let parts = if text.contains([',', ';', '\n']) {
        recipients::split_pasted(&text)
    } else {
        vec![text.trim().to_owned()]
    };
    let mut leftover = String::new();
    for (index, part) in parts.iter().enumerate() {
        if part.is_empty() {
            continue;
        }
        let mut address = recipients::parse(part);
        // Parts without a name take the address book's.
        if address.name.is_none() && !address.email.is_empty() {
            address.name = book.and_then(|book| book.name_for(&address.email));
        }
        let last = index + 1 == parts.len();
        if last && parts.len() > 1 && !recipients::sendable(&address) {
            leftover = part.clone();
            continue;
        }
        if open.recipients().count() >= recipients::MAX_RECIPIENTS {
            break;
        }
        open.list_mut(list).push(address);
    }
    open.input_mut(list).text = leftover;
    open.input_mut(list).selected.clear();
    open.touch(now);
}

/// Commits every list's typed text (as Send does).
pub(in crate::compose) fn commit_all(open: &mut Open, now: f64, book: Option<&contacts::Index>) {
    let field = open.state.field;
    for list in List::ALL {
        if !open.input(list).text.trim().is_empty() {
            open.state.field = list.field();
            commit_input(open, now, book);
        }
    }
    open.state.field = field;
}

/// Backspace in an empty recipient input. The chip before the caret goes, or the one being
/// edited goes and the chip before it is selected.
pub(in crate::compose) fn backspace(open: &mut Open, now: f64) {
    let Some(list) = open.state.field.list() else {
        return;
    };
    let (index, in_place) = match open.input_mut(list).editing.take() {
        Some(index) => (index, true),
        None => match open.list(list).len().checked_sub(1) {
            Some(last) => (last, false),
            None => return,
        },
    };
    if index < open.list(list).len() {
        open.list_mut(list).remove(index);
    }
    let count = open.list(list).len();
    let input = open.input_mut(list);
    input.text.clear();
    input.selected = if in_place && count > 0 {
        vec![index.saturating_sub(1)]
    } else {
        Vec::new()
    };
    input.anchor = input.selected.first().copied();
    open.touch(now);
}

/// ← / → with nothing typed, or at the edge of what's typed, moves the chip selection. Past the
/// last chip, the caret returns to the input. An edit finishes instead. A chip edited in place
/// stays selected (or the one before it, when the edit emptied it). Text typed at the end
/// becomes a chip, which ← selects and → leaves the caret after.
pub(in crate::compose) fn move_selection(
    open: &mut Open,
    delta: isize,
    book: Option<&contacts::Index>,
    now: f64,
) {
    let Some(list) = open.state.field.list() else {
        return;
    };
    if let Some(index) = open.input(list).editing {
        let emptied = open.input(list).text.trim().is_empty();
        chips::finish_in_place(open, list, book, now);
        if emptied {
            let count = open.list(list).len();
            let input = open.input_mut(list);
            input.selected = if count > 0 {
                vec![index.saturating_sub(1).min(count - 1)]
            } else {
                Vec::new()
            };
            input.anchor = input.selected.first().copied();
        }
        return;
    }
    if !open.input(list).text.trim().is_empty() {
        let before = open.list(list).len();
        commit_input(open, now, book);
        let count = open.list(list).len();
        let input = open.input_mut(list);
        input.selected = if delta < 0 && count > before {
            vec![count - 1]
        } else {
            Vec::new()
        };
        input.anchor = input.selected.first().copied();
        return;
    }
    let count = open.list(list).len();
    let input = open.input_mut(list);
    // From the selection's first chip going left, its last going right.
    let current = if delta < 0 {
        input.selected.first()
    } else {
        input.selected.last()
    }
    .copied()
    .unwrap_or(count);
    let next = current.saturating_add_signed(delta).min(count);
    input.selected = if next == count {
        Vec::new()
    } else {
        vec![next]
    };
    input.anchor = input.selected.first().copied();
}

#[cfg(test)]
mod tests {
    use super::{super::test_support::reply, *};

    #[test]
    fn the_input_takes_the_next_line_unless_its_scroll_box_fits() {
        let ctx = egui::Context::default();
        exo_ui::install(&ctx).expect("the system fonts load");
        let open = reply(&[]);
        let output = ctx.run_ui(egui::RawInput::default(), |ui| {
            // Tom's chip and its margin, then the input's box, which is 26px wider than its 2px
            // field, so 28px with nothing typed.
            let line =
                LABEL_WIDTH + FIELD.width(ui.ctx(), "tom@brightmoor.co") + CHIP_EXTRA + 1.0 + 28.0;
            assert_eq!(
                lay_out_row(ui.ctx(), &open, Some(List::To), 0.0, line).lines,
                1
            );
            let wrapped = lay_out_row(ui.ctx(), &open, Some(List::To), 0.0, line - 1.0);
            assert_eq!(
                wrapped.lines, 2,
                "with less left, the input goes on a line of its own"
            );
            assert_eq!(
                wrapped.input.map(|input| (input.left(), input.width())),
                Some((LABEL_WIDTH, 10.0)),
                "its 2px field padded 6px in a 1px border"
            );
        });
        output.drop_without_applying_deltas();
    }

    #[test]
    fn the_suggestion_list_keeps_the_widest_width_it_has_had_since_it_opened() {
        // Text widths (px): `p` shows Paul Durand (71.9) at pdurand@westbrook-realty.com (188.6).
        // Then `pri` shows Priya Raman (73.5) at priya@brightmoor.co (122.5).
        let mut seen = 0.0;
        let (name, email) = suggestion_columns(71.9, 188.6, &mut seen);
        assert_eq!(
            (name, (email * 10.0).round() / 10.0),
            (165.0, 220.6),
            "the name column's 165px includes its padding"
        );
        let (name, email) = suggestion_columns(73.5, 122.5, &mut seen);
        assert!(
            (name - 199.3).abs() < 0.5 && (email - 186.7).abs() < 0.5,
            "measured 199.33 / 186.67, got {name} / {email}"
        );
    }
}

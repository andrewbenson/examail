//! Action pane (`Send` / `Schedule` / `Done`, `Send later`, `Remind me`, `{ }`, Share
//! Availability, `Attach`, `Discard`), attachment cards strip, file drop-zone overlay, and
//! action-bar helpers shared with the read-only snippet preview (`crates/exo/src/snippets/view.rs`).

use egui::{Color32, CornerRadius, Id, Pos2, Rangef, Rect, Sense, Ui, Vec2, pos2, vec2};
use exo_store::DraftAttachment;
use exo_ui::{ComposeKeysTooltip, Family, Font, IconButton, KeyTooltip, TextStyle};

use super::{
    CONTROL_TOOLTIP_ABOVE, CONTROL_TOOLTIP_GAP, Context, Layout, Place, Response, STRIP_WIDER,
    SUBMIT, Surface, ink, is_dark, paint_rule, tooltip, underline,
};
use crate::{
    compose::{
        Action,
        draft::{Field, Open},
        reminder,
    },
    format::friendly::{self, ComposeFormat},
};

/// Vertical distance from a footer's 1px rule down to its action text line (`margin-top: 20px`).
pub(crate) const ACTION_DOWN: f32 = 20.0;
/// Height of a footer text action's clickable box (`padding: 10px` around a 16px line).
pub(crate) const ACTION_HEIGHT: f32 = 36.0;

/// Send's done mark (Classic) is ` +` at 10px (its inline block drops the leading space), 3px
/// after the label, then `padding-right: 17px` that holds the check, a 14×11 background 1px
/// down at the right. It's `#8892b9` in both themes.
const DONE_PLUS: TextStyle = TextStyle::new(Font::new(Family::Sans, 700), 10.0, 16.0);
const DONE_GAP: f32 = 3.0;
const DONE_PADDING: f32 = 17.0;
const DONE_CHECK: Vec2 = Vec2::new(14.0, 11.0);
const DONE_INK: Color32 = Color32::from_rgb(0x88, 0x92, 0xb9);
/// The footer icons: Classic's `gap: 30px`, centered on a 16px line 2.5px above the pane's line.
const ICON_GAP: f32 = 30.0;
const ICON_LINE_UP: f32 = 2.5;
const DISCARD_ICON: &str = "legacy/DEPRECATED_discard";
const ATTACH_ICON: &str = "legacy/DEPRECATED_paperclip";
/// The `#ce9ece` of `Unscheduled: {name} replied`, Snow and Carbon alike.
const UNSCHEDULED_INK: Color32 = Color32::from_rgb(0xce, 0x9e, 0xce);
/// Below this action pane width the button says only `Remind`.
const REMIND_ME_MIN_WIDTH: f32 = 430.0;
/// Below this window width a single date's tooltip says the reminder's date.
const REMIND_TOOLTIP_COMPACT: f32 = 1150.0;

/// Non-inline files in `attachments` (shared across compose forms, test queries, and snippet
/// previews).
pub(crate) fn listed_attachments(
    attachments: &[DraftAttachment],
) -> impl Iterator<Item = &DraftAttachment> {
    attachments
        .iter()
        .filter(|attachment| attachment.content_id.is_none())
}

/// Converts non-inline [`DraftAttachment`]s into [`exo_store::Attachment`] parts for synthetic
/// message cards (such as the read-only snippet preview).
pub(crate) fn draft_attachments_to_parts(
    message_id: &str,
    attachments: &[DraftAttachment],
) -> Vec<exo_store::Attachment> {
    listed_attachments(attachments)
        .map(|file| exo_store::Attachment {
            message_id: message_id.to_owned(),
            attachment_id: None,
            part_id: file.uuid.clone(),
            filename: file.name.clone(),
            mime_type: file.mime_type.clone(),
            size: u32::try_from(file.size).unwrap_or(u32::MAX),
            content_id: None,
            inline: false,
        })
        .collect()
}

/// A draft's listed attachments: its files, not its inline images.
pub(in crate::compose) fn listed(open: &Open) -> impl Iterator<Item = &DraftAttachment> {
    listed_attachments(&open.record.attachments)
}

/// How far `attachment` has gone up to Gmail (0–100) while the draft's copy there lacks it.
/// A snippet's files are Exo's alone and never go up.
pub(in crate::compose) fn uploading(open: &Open, attachment: &DraftAttachment) -> Option<f32> {
    open.sync
        .upload
        .progress(attachment)
        .filter(|_| open.persists_to_gmail())
}

/// Draws a primary footer action (`Send` / `Schedule` / `Done` / `Edit`) at `at`, with its 10px
/// hit padding and 2px focus underline, returning its interaction response and painted label width.
pub(crate) fn submit_button(
    ui: &Ui,
    id: Id,
    at: Pos2,
    label: &str,
    hit_width: Option<f32>,
    focused: bool,
) -> (egui::Response, f32) {
    let ctx = ui.ctx();
    let palette = exo_ui::palette(ctx);
    let label_width = SUBMIT.width(ctx, label);
    let width = hit_width.unwrap_or(label_width);
    let hit = Rect::from_min_size(
        pos2(at.x - 10.0, at.y - 10.0),
        vec2(width + 20.0, ACTION_HEIGHT),
    );
    let response = ui
        .interact(hit, id, Sense::click())
        .on_hover_cursor(egui::CursorIcon::PointingHand);
    SUBMIT.paint_line(
        ui.painter(),
        pos2(at.x, at.y + 1.0),
        label,
        palette.ink(0.8, 0.8),
        None,
    );
    if focused {
        underline(
            ui,
            at.x,
            at.x + width,
            at.y + 17.0,
            palette.border_base_active,
        );
    }
    (response, label_width)
}

/// Draws the right-aligned footer `Discard` icon button and its `⌘ shift ,` tooltip, shared with
/// the read-only snippet preview.
pub(crate) fn discard_button(
    ui: &Ui,
    id: Id,
    right: f32,
    pane_top: f32,
    column: Rangef,
) -> egui::Response {
    let size = exo_ui::icon_size(ui.ctx(), DISCARD_ICON).unwrap_or(vec2(11.0, 12.0));
    let line = pane_top - ICON_LINE_UP;
    let icon = Rect::from_min_size(pos2(right - size.x, line + (16.0 - size.y) / 2.0), size);
    footer_icon_button(
        ui,
        id,
        DISCARD_ICON,
        icon,
        icon_hit(icon),
        line,
        false,
        Place::Below(column),
        KeyTooltip {
            text: "Discard",
            keys: &["⌘", "shift", ","],
        },
        None,
    )
}

#[expect(
    clippy::too_many_arguments,
    reason = "renders a footer icon button, its focus underline, and its shortcut tooltip"
)]
fn footer_icon_button(
    ui: &Ui,
    id: Id,
    name: &str,
    icon: Rect,
    hit: Rect,
    line: f32,
    focused: bool,
    place: Place,
    row: KeyTooltip<'_>,
    note: Option<&str>,
) -> egui::Response {
    let palette = exo_ui::palette(ui.ctx());
    let rest = if focused {
        palette.icon_base_hover
    } else {
        palette.icon_base_default
    };
    let button = IconButton::new(name)
        .colors(rest, palette.icon_base_hover)
        .show(ui, id, hit, exo_ui::icon_rect(ui.ctx(), name, icon));
    if focused {
        // Focused, it gets `padding-bottom: 2px` and a `box-shadow: 0 2px …` under it.
        underline(
            ui,
            icon.left(),
            icon.right(),
            line + 18.0,
            palette.border_base_active,
        );
    }
    tooltip(
        ui,
        place,
        &button,
        focused,
        std::slice::from_ref(&row),
        note,
        CONTROL_TOOLTIP_GAP,
    );
    button
}

/// Draws the draft's attachment cards strip inside the body scroll container.
#[expect(
    clippy::too_many_arguments,
    reason = "lays out attachment cards in grid coordinates within the scrolled viewport"
)]
pub(super) fn show_attachments(
    ui: &Ui,
    open: &Open,
    origin: Pos2,
    top: f32,
    card: Vec2,
    per_row: usize,
    base: Id,
    response: &mut Response,
) {
    let cards: Vec<(String, String, String, Option<f32>)> = listed(open)
        .map(|attachment| {
            (
                attachment.uuid.clone(),
                attachment.name.clone(),
                attachment.mime_type.clone(),
                uploading(open, attachment),
            )
        })
        .collect();
    for (at, (uuid, name, mime_type, upload)) in cards.into_iter().enumerate() {
        let (row, column) = (at / per_row, at % per_row);
        let min = pos2(
            origin.x - STRIP_WIDER
                + exo_ui::CARD_MARGIN_X
                + column as f32 * (card.x + 2.0 * exo_ui::CARD_MARGIN_X),
            origin.y + top + row as f32 * (card.y + exo_ui::CARD_MARGIN_BOTTOM),
        );
        let upload = upload.map(|progress| Uploading {
            progress,
            online: open.sync.online,
        });
        match attachment_card(
            ui,
            Rect::from_min_size(min, card),
            &name,
            &mime_type,
            upload,
            base.with(("attachment", &uuid)),
        ) {
            Some(CardClick::Remove) => response.actions.push(Action::RemoveAttachment(uuid)),
            Some(CardClick::Open { preview }) => response
                .actions
                .push(Action::OpenAttachment { uuid, preview }),
            None => {}
        }
    }
}

/// Draws the footer: divider rule (pane/popout), `Send` / `Schedule` / `Done`, `Send later`,
/// `Remind me`, collapsed snippet header prompt, right-side icon buttons, and file drop-zone.
#[expect(
    clippy::too_many_arguments,
    reason = "renders the footer controls and drop-zone from the form's layout coordinates"
)]
pub(super) fn show_footer(
    ui: &mut Ui,
    open: &Open,
    form: Rect,
    layout: &Layout,
    origin: Pos2,
    width: f32,
    base: Id,
    cx: &Context<'_>,
    response: &mut Response,
) {
    let ctx = ui.ctx().clone();
    let palette = exo_ui::palette(&ctx);
    let mut pane_top = origin.y + layout.footer_top + ACTION_DOWN;
    if matches!(cx.surface, Surface::Pane | Surface::Popout) {
        paint_rule(ui, origin.x, origin.y + layout.footer_top + 16.0, width);
        pane_top += 16.0;
    }
    let focused_on = |field: Field| cx.focused && open.state.field == field;
    // A snippet's label is `Done`, and a draft with a Send later time (not one a reply called
    // off) says `Schedule`.
    let scheduled = open.scheduled_at();
    let label = if open.is_snippet() {
        "Done"
    } else if scheduled.is_some() {
        "Schedule"
    } else {
        "Send"
    };
    // Outside the new-message pane, Send shows `Send +✓` while a send would mark done, which is
    // with the setting on or while ⌘⇧ are held.
    let held = cx.focused && ctx.input(|input| input.modifiers.mac_cmd && input.modifiers.shift);
    let done_icon = (cx.send_and_done || held) && cx.surface != Surface::Pane;
    let label_width = SUBMIT.width(&ctx, label);
    let send_width = if done_icon {
        label_width + DONE_GAP + DONE_PLUS.width(&ctx, "+") + DONE_PADDING
    } else {
        label_width
    };
    let (send, _) = submit_button(
        ui,
        base.with("send"),
        pos2(origin.x, pane_top),
        label,
        Some(send_width),
        focused_on(Field::Send),
    );
    if done_icon {
        DONE_PLUS.paint_line(
            ui.painter(),
            pos2(origin.x + label_width + DONE_GAP, pane_top + 1.0),
            "+",
            ink(&ctx, 0.8),
            None,
        );
        // The check goes on the device pixel nearest its box, half a pixel lower than its 1px
        // offset says (measured).
        let pixels = ctx.pixels_per_point();
        let left = ((origin.x + send_width - DONE_CHECK.x) * pixels).round() / pixels;
        paint_done_check(ui.painter(), pos2(left, pane_top + 2.0 + 0.5));
    }
    // The tooltip says `Send ⌘ enter`. A reply adds `Send + Mark Done ⌘ shift enter`, or with
    // Send + Mark Done on shows only `Send + Mark Done ⌘ enter`. A snippet's says `Done ⌘ enter`.
    let reply = matches!(
        open.record.action,
        exo_store::DraftAction::Reply | exo_store::DraftAction::ReplyAll
    );
    let send_rows = match (reply, cx.send_and_done) {
        (true, true) => vec![KeyTooltip {
            text: "Send + Mark Done",
            keys: &["⌘", "enter"],
        }],
        (true, false) => vec![
            KeyTooltip {
                text: label,
                keys: &["⌘", "enter"],
            },
            KeyTooltip {
                text: "Send + Mark Done",
                keys: &["⌘", "shift", "enter"],
            },
        ],
        (false, _) => vec![KeyTooltip {
            text: label,
            keys: &["⌘", "enter"],
        }],
    };
    match (scheduled, cx.place()) {
        // `Hit ⌘ enter to Schedule` stays open while the form has the keyboard, its keys
        // breathing from .3 to 1 (`ease-in-out .4s infinite alternate`).
        (Some(_), Place::Below(within)) if cx.focused => {
            let phase = (ctx.input(|input| input.time) / 0.4) % 2.0;
            let t = if phase < 1.0 { phase } else { 2.0 - phase } as f32;
            let breath = 0.3 + 0.7 * exo_ui::motion::CubicBezier::EASE_IN_OUT.at(t);
            ctx.request_repaint();
            ComposeKeysTooltip {
                before: "Hit",
                keys: &["⌘", "enter"],
                after: " to Schedule",
            }
            .show_open(
                ui,
                base.with("schedule-tip"),
                send.rect,
                CONTROL_TOOLTIP_GAP,
                within,
                breath,
            );
        }
        (Some(_), _) => {}
        (None, place) => tooltip(
            ui,
            place,
            &send,
            focused_on(Field::Send),
            &send_rows,
            None,
            CONTROL_TOOLTIP_GAP,
        ),
    }
    if send.clicked() {
        let mark_done = !open.is_snippet()
            && ctx.input(|input| input.modifiers.command && input.modifiers.shift);
        response.actions.push(Action::ClickSend { mark_done });
    }
    if !open.is_snippet() {
        let gap = action_gap(open, width, cx, done_icon);
        let after = send_later(
            ui,
            open,
            response,
            pos2(origin.x + send_width + gap, pane_top),
            width,
            cx,
            base,
        );
        remind_me(
            ui,
            open,
            response,
            pos2(after + gap, pane_top),
            width,
            cx,
            base,
        );
    }
    // A collapsed snippet shows `Add To, Cc, Bcc, Subject…` (`ect` only above 1150px) in Remind
    // me's place. Clicking it shows the recipients and subject.
    if open.is_snippet() && !open.state.header_expanded {
        let text = if ctx.content_rect().width() > 1150.0 {
            "Add To, Cc, Bcc, Subject…"
        } else {
            "Add To, Cc, Bcc, Subj…"
        };
        let left = origin.x + send_width + 18.0;
        let add = ui
            .interact(
                Rect::from_min_size(
                    pos2(left - 10.0, pane_top - 10.0),
                    vec2(SUBMIT.width(&ctx, text) + 20.0, ACTION_HEIGHT),
                ),
                base.with("snippet-header"),
                Sense::click(),
            )
            .on_hover_cursor(egui::CursorIcon::PointingHand);
        SUBMIT.paint_line(
            ui.painter(),
            pos2(left, pane_top + 1.0),
            text,
            ink(&ctx, 0.35),
            None,
        );
        if add.clicked() {
            response.actions.push(Action::Focus(Field::To));
        }
    }
    // The footer icons go at their own sizes from the right, centered on a 16px line 2.5px above
    // the pane's line, with their clickable boxes pulled in around them.
    let line = pane_top - ICON_LINE_UP;
    let mut right = origin.x + width;
    let size_of = |name: &str| exo_ui::icon_size(&ctx, name).unwrap_or(vec2(16.0, 16.0));
    let mut place = |size: Vec2| {
        let rect = Rect::from_min_size(pos2(right - size.x, line + (16.0 - size.y) / 2.0), size);
        right -= size.x + ICON_GAP;
        rect
    };
    let discard = place(size_of(DISCARD_ICON));
    let attach = place(size_of(ATTACH_ICON));
    // Then, except on a snippet, come `{ }` and Share Availability's calendar
    // (`DEPRECATED_roundedCalendar`).
    if !open.is_snippet() {
        let name = "legacy/DEPRECATED_snippet";
        let snippet = place(size_of(name));
        let hit = icon_hit(snippet);
        let button = IconButton::new(name)
            .colors(palette.icon_base_default, palette.icon_base_hover)
            .show(
                ui,
                base.with(name),
                hit,
                exo_ui::icon_rect(ui.ctx(), name, snippet),
            );
        let row = KeyTooltip {
            text: "Use Snippet Inline",
            keys: &[";"],
        };
        tooltip(
            ui,
            cx.place(),
            &button,
            false,
            std::slice::from_ref(&row),
            None,
            CONTROL_TOOLTIP_GAP,
        );
        if button.clicked() {
            response.actions.push(Action::SnippetInline);
        }
        let name = "legacy/DEPRECATED_roundedCalendar";
        // Its 14px square is drawn 12px tall (`height: 12px`).
        let calendar = place(vec2(14.0, 12.0));
        let hit = icon_hit(calendar);
        if open.sync.online {
            // On a reply, `You can now hit ⌘ shift A…` stays up in place of its tooltip while no
            // other icon is hovered, with the icon lit and underlined.
            let other_hovered = [icon_hit(snippet), attach_hit(attach), icon_hit(discard)]
                .into_iter()
                .any(|hit| ui.rect_contains_pointer(hit));
            let persistent = cx.availability_tip && reply && !other_hovered;
            let button = IconButton::new(name)
                .colors(palette.icon_base_default, palette.icon_base_hover)
                .lit(persistent)
                .show(ui, base.with(name), hit, exo_ui::pixel_snap(calendar));
            if persistent {
                // Classic's 2px border under its 5px padding.
                underline(
                    ui,
                    calendar.left(),
                    calendar.right(),
                    calendar.bottom() + 5.0,
                    palette.border_base_active,
                );
                match availability_tip(ui, cx, hit, base.with("availability-tip")) {
                    Some(exo_ui::DismissableClick::Close) => {
                        response.actions.push(Action::DismissAvailabilityTip)
                    }
                    // A click on the tip opens the menu, as one on the icon does.
                    Some(exo_ui::DismissableClick::Body) => {
                        response.actions.push(Action::AvailabilityMenu(true))
                    }
                    None => {}
                }
            } else {
                let row = KeyTooltip {
                    text: "Share Availability",
                    keys: &["⌘", "shift", "A"],
                };
                tooltip(
                    ui,
                    cx.place(),
                    &button,
                    false,
                    std::slice::from_ref(&row),
                    Some("Insert a booking link into your message."),
                    CONTROL_TOOLTIP_GAP,
                );
            }
            if button.clicked() {
                response.actions.push(Action::AvailabilityMenu(true));
            }
        } else {
            // Offline it's `disabled`. Its clickable takes no pointer (`pointer-events: none`), so
            // it neither lights nor clicks, and its tooltip says why.
            let anchor = ui.interact(hit, base.with(name), Sense::hover());
            exo_ui::paint_icon(
                ui,
                name,
                exo_ui::pixel_snap(calendar),
                palette.icon_base_default,
            );
            let row = KeyTooltip {
                text: "Share Availability is not available while offline.",
                keys: &[],
            };
            tooltip(
                ui,
                cx.place(),
                &anchor,
                false,
                std::slice::from_ref(&row),
                None,
                CONTROL_TOOLTIP_GAP,
            );
        }
    }
    let icons = [
        (
            (ATTACH_ICON, attach, attach_hit(attach), Field::Attach),
            KeyTooltip {
                text: "Attach",
                keys: &["⌘", "shift", "U"],
            },
            Some("(or drag 'n' drop files)"),
            Action::Attach,
        ),
        (
            (DISCARD_ICON, discard, icon_hit(discard), Field::Discard),
            KeyTooltip {
                text: "Discard",
                keys: &["⌘", "shift", ","],
            },
            None,
            Action::Discard,
        ),
    ];
    for ((name, icon, hit, field), row, note, action) in icons {
        let button = footer_icon_button(
            ui,
            base.with(name),
            name,
            icon,
            hit,
            line,
            focused_on(field),
            cx.place(),
            row,
            note,
        );
        if button.clicked() {
            response.actions.push(action);
        }
    }
    if ctx.input(|input| !input.raw.hovered_files.is_empty()) {
        drop_zone(ui, cx.drop.unwrap_or(form), is_dark(&ctx));
    }
}

/// A footer icon's clickable, which its tooltip hangs under. It covers the icon's 16px line and
/// 8px around.
fn icon_hit(icon: Rect) -> Rect {
    Rect::from_center_size(icon.center(), vec2(icon.width(), 16.0)).expand(8.0)
}

/// Attach's clickable: Classic's inline `padding: 10px`, 10px either side of the icon and 28px
/// tall on its line (measured), so its tooltip sits 2px higher than its neighbours' tooltips.
fn attach_hit(icon: Rect) -> Rect {
    Rect::from_center_size(icon.center(), vec2(icon.width() + 20.0, 28.0))
}

/// The gap from one action's text to the next (Send, Send later, Remind me).
fn action_gap(open: &Open, width: f32, cx: &Context<'_>, marks_done: bool) -> f32 {
    let reminder = shown_reminder(open, cx).is_some();
    let send_at = open.record.send_later.is_some();
    let margin = if cx.surface == Surface::Popout {
        if reminder && !send_at && marks_done {
            3.0
        } else {
            5.0
        }
    } else if marks_done && (reminder || send_at) {
        5.0
    } else if width <= 480.0 {
        12.0
    } else {
        22.0
    };
    10.0 + margin - 10.0 + 10.0
}

/// The reminder the Remind button shows: the draft's, else its thread's.
fn shown_reminder(open: &Open, cx: &Context<'_>) -> Option<(jiff::Timestamp, bool)> {
    open.record
        .reminder
        .map(|reminder| (reminder.at, reminder.keep_on_reply))
        .or(cx.thread_reminder)
}

/// The format of the action pane's dates, `Send:` and `Remind:` alike, for a pane `width` wide.
/// It's narrower with both a send time and a reminder, or popped out.
fn date_format(open: &Open, width: f32, cx: &Context<'_>) -> ComposeFormat {
    use ComposeFormat::{Compact, ExtraCompact, Friendly, MostCompact};
    let reminder = shown_reminder(open, cx).map(|(at, _)| at);
    let send_at = open.record.send_later.as_ref().map(|later| later.at);
    if cx.surface == Surface::Popout {
        return if send_at.is_none() && reminder.is_some() {
            ExtraCompact
        } else {
            MostCompact
        };
    }
    let both = reminder.is_some() && send_at.is_some();
    let now = cx.now;
    let day = |at: jiff::Timestamp| at.to_zoned(now.time_zone().clone()).date();
    let tomorrow = now.date().tomorrow().unwrap_or(now.date());
    let after_tomorrow = |at: Option<jiff::Timestamp>| at.is_some_and(|at| day(at) > tomorrow);
    // The draft's own reminder 2 to 6 days out, so before midnight on the day of
    // `1 week from now`.
    let week = now
        .date()
        .checked_add(jiff::ToSpan::days(7))
        .unwrap_or(now.date());
    let soon = open
        .record
        .reminder
        .is_some_and(|reminder| day(reminder.at) > tomorrow && day(reminder.at) < week);
    if both && after_tomorrow(reminder) && after_tomorrow(send_at) {
        return if width < 641.0 {
            MostCompact
        } else {
            ExtraCompact
        };
    }
    if width < if both { 641.0 } else { 480.0 } {
        MostCompact
    } else if !both && width < 600.0 {
        ExtraCompact
    } else if both || width < 620.0 || soon {
        Compact
    } else {
        Friendly
    }
}

/// The action pane's Send later at `at`: `Send later` in the lighter ink, the draft's time
/// (`Send: {date}`, in the format the pane's `width` allows), or `Unscheduled: {name} replied`.
/// With no time set, its tooltip says what it does. A click opens the picker. Returns where its
/// text ends.
fn send_later(
    ui: &Ui,
    open: &Open,
    response: &mut Response,
    at: Pos2,
    width: f32,
    cx: &Context<'_>,
    base: Id,
) -> f32 {
    let ctx = ui.ctx();
    let (text, color) = match (&open.delivery.unscheduled_by, &open.record.send_later) {
        (Some(name), _) if name.is_empty() => ("Unscheduled: replied".to_owned(), UNSCHEDULED_INK),
        (Some(name), _) => (format!("Unscheduled: {name} replied"), UNSCHEDULED_INK),
        (None, Some(later)) => (
            format!(
                "Send: {}",
                friendly::compose_form(later.at, cx.now, date_format(open, width, cx))
            ),
            ink(ctx, 0.6),
        ),
        (None, None) => ("Send later".to_owned(), ink(ctx, 0.35)),
    };
    let text_width = SUBMIT.width(ctx, &text);
    let hit = Rect::from_min_size(
        pos2(at.x - 10.0, at.y - 10.0),
        vec2(text_width + 20.0, ACTION_HEIGHT),
    );
    let button = ui
        .interact(hit, base.with("send-later"), Sense::click())
        .on_hover_cursor(egui::CursorIcon::PointingHand);
    SUBMIT.paint_line(ui.painter(), pos2(at.x, at.y + 1.0), &text, color, None);
    let focused = cx.focused && open.state.field == Field::SendLater;
    if focused {
        underline(
            ui,
            at.x,
            at.x + text_width,
            at.y + 17.0,
            exo_ui::palette(ctx).border_base_active,
        );
    }
    if open.record.send_later.is_none() && open.delivery.unscheduled_by.is_none() {
        let row = KeyTooltip {
            text: "Send later",
            keys: &["⌘", "shift", "L"],
        };
        tooltip(
            ui,
            cx.place(),
            &button,
            focused,
            std::slice::from_ref(&row),
            Some("Send at the perfect moment"),
            CONTROL_TOOLTIP_GAP,
        );
    }
    if button.clicked() {
        response.actions.push(Action::SendLater);
    }
    at.x + text_width
}

/// The action pane's Remind button at `at`.
fn remind_me(
    ui: &Ui,
    open: &Open,
    response: &mut Response,
    at: Pos2,
    width: f32,
    cx: &Context<'_>,
    base: Id,
) {
    let ctx = ui.ctx();
    let both = shown_reminder(open, cx).is_some() && open.record.send_later.is_some();
    let text = match shown_reminder(open, cx) {
        None if width <= REMIND_ME_MIN_WIDTH => "Remind".to_owned(),
        None => "Remind me".to_owned(),
        Some((when, keep_on_reply)) => {
            let suffix = if keep_on_reply || width < if both { 641.0 } else { 480.0 } {
                ""
            } else if both || width < 600.0 || cx.surface == Surface::Popout {
                "…"
            } else {
                " if no reply"
            };
            format!(
                "Remind: {}{suffix}",
                friendly::compose_form(when, cx.now, date_format(open, width, cx))
            )
        }
    };
    let color = ink(
        ctx,
        if open.record.reminder.is_some() {
            0.6
        } else {
            0.35
        },
    );
    let text_width = SUBMIT.width(ctx, &text);
    let hit = Rect::from_min_size(
        pos2(at.x - 10.0, at.y - 10.0),
        vec2(text_width + 20.0, ACTION_HEIGHT),
    );
    let button = ui
        .interact(hit, base.with("remind-me"), Sense::click())
        .on_hover_cursor(egui::CursorIcon::PointingHand);
    // An automatic reminder just put on flourishes from the label's ink and keeps its last frame.
    match reminder::flourish_progress(open, ctx.input(|input| input.time)) {
        Some(progress) => {
            reminder::paint_flourish(
                ui.painter(),
                pos2(at.x, at.y + 1.0),
                &text,
                SUBMIT,
                progress,
                ink(ctx, 0.6),
            );
            if progress < 1.0 {
                ctx.request_repaint();
            }
        }
        None => {
            SUBMIT.paint_line(ui.painter(), pos2(at.x, at.y + 1.0), &text, color, None);
        }
    }
    let focused = cx.focused && open.state.field == Field::RemindMe;
    if focused {
        underline(
            ui,
            at.x,
            at.x + text_width,
            at.y + 17.0,
            exo_ui::palette(ctx).border_base_active,
        );
    }
    // The tooltip says `Remind me ⌘ shift H`, then what the button is for, or in a compact
    // layout the draft's reminder.
    let note = match open.record.reminder {
        Some(reminder)
            if both
                || ctx.content_rect().width() < REMIND_TOOLTIP_COMPACT
                || cx.surface == Surface::Popout =>
        {
            format!(
                "{}{}",
                friendly::exact_friendly(reminder.at, cx.now),
                if reminder.keep_on_reply {
                    ""
                } else {
                    " if no reply"
                }
            )
        }
        _ => "Follow up on time, every time".to_owned(),
    };
    let row = KeyTooltip {
        text: "Remind me",
        keys: &["⌘", "shift", "H"],
    };
    tooltip(
        ui,
        cx.place(),
        &button,
        focused,
        std::slice::from_ref(&row),
        Some(note.as_str()),
        CONTROL_TOOLTIP_GAP,
    );
    if button.clicked() {
        response.actions.push(Action::RemindMe);
    }
}

/// A card's file on its way to Gmail: how far it has come, and whether Gmail is in reach.
#[derive(Clone, Copy)]
struct Uploading {
    progress: f32,
    online: bool,
}

/// What a click on a draft's card asks for.
enum CardClick {
    /// The cross, which takes the file off the draft.
    Remove,
    /// The card, which opens the file, previewed unless ⌘ or ⇧ was held.
    Open { preview: bool },
}

/// One of a draft's cards. On hover it shows a 50% black overlay and the remove cross (12px, 9px
/// from the top and 8px from the right). While its file goes up, it shows its progress bar
/// (online) and, hovered, `Processing…` or `Will upload when online` under the card. Returns
/// what a click on the card asks for.
fn attachment_card(
    ui: &Ui,
    card: Rect,
    name: &str,
    mime_type: &str,
    upload: Option<Uploading>,
    id: Id,
) -> Option<CardClick> {
    let response = ui
        .interact(card, id, Sense::click())
        .on_hover_cursor(egui::CursorIcon::PointingHand);
    let ctx = ui.ctx();
    let hover = exo_ui::motion::transition(
        ctx,
        id.with("hover"),
        response.hovered(),
        0.25,
        exo_ui::motion::CubicBezier::EASE,
    );
    exo_ui::paint_attachment_card(ui, card, name, mime_type, hover);
    // The overlay: `opacity: 0` → `.5`, `.125s linear`.
    let overlay = exo_ui::motion::transition(
        ctx,
        id.with("overlay"),
        response.hovered(),
        0.125,
        exo_ui::motion::CubicBezier::LINEAR,
    );
    if overlay > 0.0 {
        ui.painter().rect_filled(
            exo_ui::pixel_snap(card),
            CornerRadius::same(4),
            Color32::from_black_alpha((0.5 * overlay * 255.0).round() as u8),
        );
    }
    if let Some(Uploading { progress, online }) = upload {
        // The progress bar goes over the overlay while online, from 10% in whole percents.
        if online {
            exo_ui::paint_attachment_progress(ui, card, name, progress.floor().max(10.0));
        }
        // Its tooltip, centred under the card.
        let text = if online {
            "Processing…"
        } else {
            "Will upload when online"
        };
        KeyTooltip { text, keys: &[] }.show_below(ui, &response, 8.0);
    }
    // The remove cross: a white 9×8 cross scaled to 12px wide, `left center` in its 12px box.
    let cross = Rect::from_min_size(
        pos2(card.right() - 8.0 - 12.0, card.top() + 9.0),
        vec2(12.0, 12.0),
    );
    let on_cross = response.hover_pos().is_some_and(|at| cross.contains(at));
    // `opacity: 0` → `1` on the card → `.7` on the cross, `150ms ease`.
    let shown = exo_ui::motion::transition(
        ctx,
        id.with("cross"),
        response.hovered(),
        0.15,
        exo_ui::motion::CubicBezier::EASE,
    );
    let dimmed = exo_ui::motion::transition(
        ctx,
        id.with("cross-hover"),
        on_cross,
        0.15,
        exo_ui::motion::CubicBezier::EASE,
    );
    let opacity = shown * (1.0 - 0.3 * dimmed);
    if opacity > 0.0 {
        let glyph = Rect::from_center_size(cross.center(), vec2(12.0, 12.0 * 8.0 / 9.0));
        exo_ui::paint_icon(
            ui,
            "css/isInvalid-cross",
            glyph,
            Color32::WHITE.gamma_multiply(opacity),
        );
    }
    if !response.clicked() {
        return None;
    }
    if response
        .interact_pointer_pos()
        .is_some_and(|at| cross.contains(at))
    {
        return Some(CardClick::Remove);
    }
    let modifiers = ui.input(|input| input.modifiers);
    Some(CardClick::Open {
        preview: !(modifiers.command || modifiers.shift),
    })
}

/// The overlay every form shows over `area` while files are dragged over the window: 80%
/// opaque, a 2px dashed border, the page-with-a-plus icon in the middle and `Drop files here`
/// under it.
fn drop_zone(ui: &Ui, area: Rect, dark: bool) {
    let painter = ui.ctx().layer_painter(egui::LayerId::new(
        egui::Order::Foreground,
        Id::new("compose-drop-zone"),
    ));
    let rect = exo_ui::pixel_snap(area);
    // `hsla(0,0%,100%,.98)` / `#27292d` at `opacity: .8`.
    let fill = if dark {
        Color32::from_rgb(0x27, 0x29, 0x2d)
    } else {
        Color32::from_white_alpha(250)
    };
    painter.rect_filled(rect, CornerRadius::same(2), fill.gamma_multiply(0.8));
    // `2px dashed rgba(0,0,0,.1)`, or `hsla(0,0%,100%,.85)` in dark.
    let stroke = egui::Stroke::new(
        2.0,
        if dark {
            Color32::from_white_alpha(217)
        } else {
            Color32::from_black_alpha(26)
        }
        .gamma_multiply(0.8),
    );
    let inner = rect.shrink(1.0);
    let corners = [
        inner.left_top(),
        inner.right_top(),
        inner.right_bottom(),
        inner.left_bottom(),
        inner.left_top(),
    ];
    painter.extend(egui::Shape::dashed_line(&corners, stroke, 6.0, 6.0));
    // The background's 22×26 icon, `center center no-repeat` (its own `#7F7F7F`).
    let icon = exo_ui::pixel_snap(Rect::from_center_size(rect.center(), vec2(22.0, 26.0)));
    exo_ui::paint_icon_on(
        &painter,
        "css/FileDropZone",
        icon,
        Color32::WHITE.gamma_multiply(0.8),
    );
    // `::after { content: "Drop files here" }`: 12px/12px bold, `top: 50%; margin-top: 1.5rem`.
    let label = TextStyle::new(Font::new(Family::Sans, 700), 12.0, 12.0);
    let text = "Drop files here";
    let width = label.width(ui.ctx(), text);
    let color = Color32::from_black_alpha(204).gamma_multiply(0.8);
    label.paint_line(
        &painter,
        pos2(rect.center().x - width / 2.0, rect.center().y + 24.0),
        text,
        color,
        None,
    );
}

/// The done mark's check (the path `M1 5.142L4.858 9l7.587-7.587`, stroked 2px) with its 14×11
/// box at `top_left`.
fn paint_done_check(painter: &egui::Painter, top_left: Pos2) {
    let points =
        [vec2(1.0, 5.142), vec2(4.858, 9.0), vec2(12.445, 1.413)].map(|point| top_left + point);
    painter.add(egui::Shape::line(
        points.to_vec(),
        egui::Stroke::new(2.0, DONE_INK),
    ));
}

/// Share Availability's standing tip (`You can now hit ⌘ shift A…`), pinned where the tooltip
/// of the calendar icon at `hit` goes. It's 256px wide.
fn availability_tip(
    ui: &Ui,
    cx: &Context<'_>,
    hit: Rect,
    id: Id,
) -> Option<exo_ui::DismissableClick> {
    let content = [
        exo_ui::TooltipPart::Text("You can now hit"),
        exo_ui::TooltipPart::Keys(&["⌘", "shift", "A"]),
        exo_ui::TooltipPart::Text("to insert a booking link into your message."),
    ];
    let tip = exo_ui::DismissableTooltip {
        title: "Share Availability",
        content: &content,
        width: 256.0,
    };
    match cx.place() {
        Place::Below(within) => tip.show(
            ui,
            id,
            hit,
            exo_ui::Placement::Below {
                gap: CONTROL_TOOLTIP_GAP,
                shift: 0.0,
            },
            within,
        ),
        Place::Above => tip.show(
            ui,
            id,
            hit,
            exo_ui::Placement::Above(CONTROL_TOOLTIP_ABOVE),
            Rangef::EVERYTHING,
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::{super::test_support::reply, *};

    #[test]
    fn the_action_panes_dates_narrow_with_a_reminder_and_a_send_time() {
        let now: jiff::Zoned = "2026-11-17T09:00[America/New_York]".parse().expect("now");
        let at = |text: &str| text.parse::<jiff::Zoned>().expect("a time").timestamp();
        let cx = |surface| Context {
            aliases: &[],
            me: "",
            surface,
            focused: false,
            book: None,
            thread_people: None,
            pin: Rangef::EVERYTHING,
            drop: None,
            now: &now,
            thread_reminder: None,
            send_and_done: false,
            max_height: f32::INFINITY,
            availability_tip: false,
        };
        let mut open = reply(&[]);
        open.record.reminder = Some(exo_store::DraftReminder {
            at: at("2026-11-19T08:00[America/New_York]"),
            keep_on_reply: false,
            automatic: false,
        });
        assert_eq!(
            date_format(&open, 640.0, &cx(Surface::Inline)),
            ComposeFormat::Compact,
            "a reminder 2 days out (`Thu Nov 19, 8:00 AM`)"
        );
        open.record.reminder = Some(exo_store::DraftReminder {
            at: at("2026-12-01T08:00[America/New_York]"),
            keep_on_reply: false,
            automatic: false,
        });
        assert_eq!(
            date_format(&open, 640.0, &cx(Surface::Inline)),
            ComposeFormat::Friendly
        );
        assert_eq!(
            date_format(&open, 590.0, &cx(Surface::Inline)),
            ComposeFormat::ExtraCompact
        );
        assert_eq!(
            date_format(&open, 640.0, &cx(Surface::Popout)),
            ComposeFormat::ExtraCompact,
            "popped out, a reminder alone"
        );
        open.record.send_later = Some(exo_store::SendLater {
            at: at("2026-11-18T08:00[America/New_York]"),
            abort_on_reply: false,
            current_message_ids: Vec::new(),
        });
        assert_eq!(
            date_format(&open, 640.0, &cx(Surface::Inline)),
            ComposeFormat::MostCompact,
            "both, under 641px"
        );
        assert_eq!(
            date_format(&open, 700.0, &cx(Surface::Inline)),
            ComposeFormat::Compact
        );
        open.record.send_later = Some(exo_store::SendLater {
            at: at("2026-11-25T08:00[America/New_York]"),
            abort_on_reply: false,
            current_message_ids: Vec::new(),
        });
        assert_eq!(
            date_format(&open, 700.0, &cx(Surface::Inline)),
            ComposeFormat::ExtraCompact,
            "both after tomorrow"
        );
    }

    #[test]
    fn the_actions_close_up_popped_out_and_while_a_send_marks_done_with_a_date() {
        let now: jiff::Zoned = "2026-11-17T09:00[America/New_York]".parse().expect("now");
        let cx = |surface| Context {
            aliases: &[],
            me: "",
            surface,
            focused: false,
            book: None,
            thread_people: None,
            pin: Rangef::EVERYTHING,
            drop: None,
            now: &now,
            thread_reminder: None,
            send_and_done: false,
            max_height: f32::INFINITY,
            availability_tip: false,
        };
        let mut open = reply(&[]);
        assert_eq!(
            action_gap(&open, 640.0, &cx(Surface::Inline), false),
            32.0,
            "`margin-right: 22px` between 10px paddings"
        );
        assert_eq!(
            action_gap(&open, 480.0, &cx(Surface::Inline), false),
            22.0,
            "12px in a pane up to 480px"
        );
        assert_eq!(
            action_gap(&open, 500.0, &cx(Surface::Popout), false),
            15.0,
            "popped out, 5px"
        );
        open.record.reminder = Some(exo_store::DraftReminder {
            at: now.timestamp(),
            keep_on_reply: false,
            automatic: false,
        });
        assert_eq!(
            action_gap(&open, 500.0, &cx(Surface::Popout), true),
            13.0,
            "popped out, 3px with a reminder, no send time, marking done"
        );
        assert_eq!(
            action_gap(&open, 640.0, &cx(Surface::Inline), true),
            15.0,
            "a date set while a send marks done"
        );
        assert_eq!(action_gap(&open, 640.0, &cx(Surface::Inline), false), 32.0);
    }
}

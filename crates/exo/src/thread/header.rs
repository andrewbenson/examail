//! The thread's header: the subject wrapped around the floating actions, the star, the label
//! chips and the status line (Unsubscribe), and the back gutter on the left.

use egui::{Color32, CornerRadius, Mesh, Rect, Sense, Shape, Ui, Vec2, pos2, vec2};
use exo_store::ThreadDetail;
use exo_ui::{
    BoxShadow, Family, Font, IconButton, KeyTooltip, LabelChip, Placement, TextStyle, chip_hover,
    motion,
};

use super::{Action, Layout};
use crate::{
    body::Bodies,
    triage_copy::{TriageButton, TriageRemind},
};

/// The header's padding: `24px 4rem 20px`.
pub(super) const PADDING_TOP: f32 = 24.0;
const PADDING_BOTTOM: f32 = 20.0;
/// The bottom padding once the messages scroll under the header: `padding-bottom: 12px`, plus a
/// 1px bottom border.
const SCROLLED_PADDING_BOTTOM: f32 = 12.0;
/// How far above its unscrolled bottom the scrolled header ends, its border included.
pub(super) const SCROLLED_INSET: f32 = PADDING_BOTTOM - SCROLLED_PADDING_BOTTOM - 1.0;
/// The subject: 21px/32px, `top: 3px; margin-right: 12px`.
pub(super) const SUBJECT_LINE: f32 = 32.0;
const SUBJECT: TextStyle = TextStyle::new(Font::new(Family::Sans, 400), 21.0, SUBJECT_LINE);
const SUBJECT_NUDGE: f32 = 3.0;
const SUBJECT_MARGIN: f32 = 12.0;
/// The actions, floated right: 117px of buttons with `margin: 11px -4px -3px 0` around their
/// 16px height, so a margin box 113px wide and 24px tall. Only the subject's first line starts
/// beside it and stops at it, unless the subject is clamped, whose block keeps beside it all the
/// way down.
const ACTIONS_MARGIN_BOX: f32 = 113.0;
/// Mark Done's share of the actions, which a view where conversations can't be marked done (Spam)
/// leaves out.
const DONE_WIDTH: f32 = 23.0;
/// A subject longer than this many UTF-16 units, or than [`LONG`] in a window up to
/// [`CLAMP_WINDOW`] wide, is a block of at most [`CLAMP_LINES`] lines, the last one cut with an
/// ellipsis (`-webkit-line-clamp: 4`).
const VERY_LONG: usize = 140;
const LONG: usize = 96;
const CLAMP_WINDOW: f32 = 800.0;
const CLAMP_LINES: usize = 4;
const ELLIPSIS: &str = "…";
/// The chips follow a clamped subject on a line of their own. Without chips, that line is the
/// empty line of the label's 12px font, 4px tall (measured).
const CLAMPED_EMPTY_LINE: f32 = 4.0;
/// The status line: 12px/16px bold, `padding: 8px 0 1px; margin-bottom: 3px`.
const STATUS: TextStyle = TextStyle::new(Font::new(Family::Sans, 700), 12.0, 16.0);
const STATUS_HEIGHT: f32 = 8.0 + 16.0 + 1.0 + 3.0;
/// Chips sit on the subject's line box, below the line box's top (measured).
const CHIP_TOP: f32 = 11.0;
/// The space after each label chip.
const CHIP_MARGIN: f32 = 6.0;

/// A pending bulk action's status: `#ef8e8e`, a literal in Snow and Carbon.
const PENDING: Color32 = Color32::from_rgb(0xef, 0x8e, 0x8e);

/// What the header's status line says (no AI summaries).
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) enum Status {
    /// A bulk action the conversation waits on: `Pending: Mark Done`.
    Pending(String),
    /// The conversation's last one-click action: `View order`, `Track Order`.
    Action(exo_mail::Action),
    /// The conversation can be unsubscribed from.
    Unsubscribe,
}

/// What the keyboard can focus in the header, in tab order (`[tabindex]`): the status line when
/// it does something, then Done, Remind Me and ⌘K (the conversation arrows have no tab index).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Control {
    Status,
    Done,
    Remind,
    Command,
}

impl Control {
    /// Where the actions' tab indices start. They come after the status line's 1, if there is one.
    pub(super) fn tab_start(controls: &[Self]) -> u8 {
        if controls.contains(&Self::Status) {
            2
        } else {
            1
        }
    }

    /// The control's tab index. The status line's is 1 and the actions' are `start + 2…4`.
    pub(super) fn tab_index(self, start: u8) -> u8 {
        match self {
            Self::Status => 1,
            Self::Done => start + 2,
            Self::Remind => start + 3,
            Self::Command => start + 4,
        }
    }

    /// What clicking the control does when the status line says `status`.
    pub(super) fn action(self, status: Option<&Status>) -> Action {
        match self {
            Self::Status => match status {
                Some(Status::Action(action)) => Action::OpenUrl(action.url.clone()),
                _ => Action::Unsubscribe,
            },
            Self::Done => Action::ToggleDone,
            Self::Remind => Action::Remind,
            Self::Command => Action::CommandPalette,
        }
    }
}

/// The header's controls for its `status` line, in tab order, Done only where conversations can
/// be marked done (`archivable`).
pub(super) fn controls(status: Option<&Status>, archivable: bool) -> Vec<Control> {
    let status =
        matches!(status, Some(Status::Unsubscribe | Status::Action(_))).then_some(Control::Status);
    status
        .into_iter()
        .chain(archivable.then_some(Control::Done))
        .chain([Control::Remind, Control::Command])
        .collect()
}

/// The status line's underline when focused, in Classic: `box-shadow: 0 2px #aeb1dd`.
const STATUS_FOCUS: Color32 = Color32::from_rgb(0xae, 0xb1, 0xdd);

/// The status line for `detail`: the bulk action it waits on, else its last one-click
/// `action`, else Unsubscribe when there's something to unsubscribe from (a usable
/// `List-Unsubscribe`, not of one of `own_domains`, or a rendered body's unsubscribe link).
pub(super) fn status(
    detail: &ThreadDetail,
    bodies: &Bodies,
    pending: Option<&str>,
    action: Option<exo_mail::Action>,
    own_domains: &[String],
) -> Option<Status> {
    if let Some(pending) = pending {
        return Some(Status::Pending(pending.to_owned()));
    }
    if let Some(action) = action {
        return Some(Status::Action(action));
    }
    crate::unsubscribe::find(detail, own_domains, |id| {
        bodies.facts(id).and_then(|facts| facts.unsubscribe.clone())
    })
    .map(|_| Status::Unsubscribe)
}

/// The subject as the header shows it.
pub(super) fn title(detail: &ThreadDetail) -> &str {
    let subject = detail.subject.trim();
    if subject.is_empty() { "-" } else { subject }
}

/// `text` cut after each place a line may break: a run of spaces, and a hyphen between letters
/// (not one before a digit).
fn break_points(text: &str) -> Vec<&str> {
    let chars: Vec<(usize, char)> = text.char_indices().collect();
    let mut pieces = Vec::new();
    let mut start = 0;
    for (at, &(offset, c)) in chars.iter().enumerate() {
        let before = at.checked_sub(1).map(|before| chars[before].1);
        let after = chars.get(at + 1).map(|&(_, c)| c);
        let breaks = match c {
            ' ' => after != Some(' '),
            '-' => {
                before.is_some_and(|before| before != ' ')
                    && after
                        .is_some_and(|after| !matches!(after, ' ' | '-') && !after.is_ascii_digit())
            }
            _ => false,
        };
        if breaks {
            let end = offset + c.len_utf8();
            pieces.push(&text[start..end]);
            start = end;
        }
    }
    if start < text.len() {
        pieces.push(&text[start..]);
    }
    pieces
}

/// How many bytes of `text` fit in `limit` px: at least its first character, for a word broken
/// anywhere.
fn fitting(text: &str, limit: f32, width: impl Fn(&str) -> f32) -> usize {
    let ends: Vec<usize> = text
        .char_indices()
        .skip(1)
        .map(|(at, _)| at)
        .chain([text.len()])
        .collect();
    let fits = ends.partition_point(|&end| width(&text[..end]) <= limit);
    ends[fits.saturating_sub(1)]
}

/// Breaks `text` into lines the way the subject wraps: where a line may break, the first line
/// stopping at the floating actions, and a word too long for a line of its own broken anywhere
/// (`overflow-wrap: break-word`). The last line leaves room for `margin`, the subject's right
/// margin while it's inline.
fn wrap(ui: &Ui, text: &str, first: f32, full: f32, margin: f32) -> Vec<String> {
    let painter = ui.painter();
    let width = |line: &str| {
        SUBJECT
            .single_line(painter, line.trim_end(), Color32::PLACEHOLDER, None)
            .size()
            .x
    };
    let limit = |line: usize| if line == 0 { first } else { full };
    let mut lines: Vec<String> = vec![String::new()];
    for piece in break_points(text) {
        let index = lines.len() - 1;
        let candidate = format!("{}{piece}", lines[index]);
        if width(&candidate) <= limit(index) {
            lines[index] = candidate;
            continue;
        }
        if !lines[index].is_empty() {
            lines.push(String::new());
        }
        let mut rest = piece;
        loop {
            let index = lines.len() - 1;
            if width(rest) <= limit(index) {
                lines[index] = rest.to_owned();
                break;
            }
            let cut = fitting(rest, limit(index), width);
            lines[index] = rest[..cut].to_owned();
            rest = &rest[cut..];
            lines.push(String::new());
        }
    }
    let last = lines.len() - 1;
    if width(&lines[last]) + margin > limit(last)
        && let Some(cut) = lines[last].trim_end().rfind(' ')
    {
        let moved = lines[last][cut + 1..].to_owned();
        lines[last].truncate(cut + 1);
        lines.push(moved);
    }
    lines
}

/// Cuts `lines`, `limit` px wide, to [`CLAMP_LINES`], making room on the last one for an
/// ellipsis by taking as many characters as it needs. Returns whether it cut anything, so that
/// the ellipsis follows the last line (as a glyph run of its own, unkerned).
fn clamp(ui: &Ui, lines: &mut Vec<String>, limit: f32) -> bool {
    if lines.len() <= CLAMP_LINES {
        return false;
    }
    lines.truncate(CLAMP_LINES);
    let width = |line: &str| {
        SUBJECT
            .single_line(ui.painter(), line, Color32::PLACEHOLDER, None)
            .size()
            .x
    };
    let room = limit - width(ELLIPSIS);
    let last = &mut lines[CLAMP_LINES - 1];
    let mut kept = last.trim_end().to_owned();
    while kept.chars().count() > 1 && width(&kept) > room {
        kept.pop();
    }
    *last = kept;
    true
}

/// Where the subject's lines and the label chips after them go.
struct Arrangement {
    lines: Vec<String>,
    /// The last line was cut, and an ellipsis follows it.
    ellipsis: bool,
    /// Each chip's name, line and left edge.
    chips: Vec<(String, usize, f32)>,
    /// The subject's and chips' height.
    height: f32,
}

/// Where the subject and `chips` go, beside actions that have Mark Done when `archivable`.
fn arrange(
    ui: &Ui,
    layout: &Layout,
    detail: &ThreadDetail,
    chips: &[String],
    archivable: bool,
) -> Arrangement {
    let painter = ui.painter();
    let label = layout.label;
    let title = title(detail);
    let length = crate::format::measure::utf16_len(title);
    let clamped = length > VERY_LONG || (length > LONG && layout.window_width() <= CLAMP_WINDOW);
    let actions = if archivable {
        ACTIONS_MARGIN_BOX
    } else {
        ACTIONS_MARGIN_BOX - DONE_WIDTH
    };
    let float_left = label.width() - actions;
    let (lines, ellipsis) = if clamped {
        let mut lines = wrap(ui, title, float_left, float_left, 0.0);
        let cut = clamp(ui, &mut lines, float_left);
        (lines, cut)
    } else {
        // The margin after the subject has to fit on its last line too.
        let lines = wrap(ui, title, float_left, label.width(), SUBJECT_MARGIN);
        (lines, false)
    };
    let (mut line, mut x) = if clamped {
        (lines.len(), label.left())
    } else {
        let last = lines.len() - 1;
        let end = SUBJECT
            .single_line(painter, lines[last].trim_end(), Color32::PLACEHOLDER, None)
            .size()
            .x;
        (last, label.left() + end + SUBJECT_MARGIN)
    };
    let mut placed = Vec::new();
    for name in chips {
        let width = LabelChip::new(painter, name, false).width();
        let limit = if line == 0 {
            label.left() + float_left
        } else {
            label.right()
        };
        if x + width > limit && x > label.left() {
            line += 1;
            x = label.left();
        }
        placed.push((name.clone(), line, x));
        x += width + CHIP_MARGIN;
    }
    let height = match (clamped, placed.last()) {
        (true, None) => lines.len() as f32 * SUBJECT_LINE + CLAMPED_EMPTY_LINE,
        (_, Some((_, last, _))) => (last + 1) as f32 * SUBJECT_LINE,
        (false, None) => lines.len() as f32 * SUBJECT_LINE,
    };
    Arrangement {
        lines,
        ellipsis,
        chips: placed,
        height,
    }
}

impl Arrangement {
    /// Each line's text and where its ellipsis goes, if it has one.
    fn painted<'a>(
        &'a self,
        painter: &egui::Painter,
    ) -> impl Iterator<Item = (&'a str, Option<f32>)> {
        let last = self.lines.len() - 1;
        self.lines.iter().enumerate().map(move |(at, line)| {
            let text = line.trim_end();
            let ellipsis = (self.ellipsis && at == last).then(|| {
                SUBJECT
                    .single_line(painter, text, Color32::PLACEHOLDER, None)
                    .size()
                    .x
            });
            (text, ellipsis)
        })
    }
}

/// The box around the subject's glyphs (26px on each line), for the subject's copy tooltip. None
/// without a subject.
pub(super) fn subject_rect(
    ui: &Ui,
    layout: &Layout,
    detail: &ThreadDetail,
    chips: &[String],
    archivable: bool,
) -> Option<Rect> {
    if detail.subject.trim().is_empty() {
        return None;
    }
    let arrangement = arrange(ui, layout, detail, chips, archivable);
    let painter = ui.painter();
    let width = |text: &str| {
        SUBJECT
            .single_line(painter, text, Color32::PLACEHOLDER, None)
            .size()
            .x
    };
    let width = arrangement
        .painted(painter)
        .map(|(text, ellipsis)| ellipsis.map_or(width(text), |at| at + width(ELLIPSIS)))
        .fold(0.0, f32::max);
    let label = layout.label;
    let top = label.top() + SUBJECT_NUDGE + 3.0;
    Some(Rect::from_min_size(
        pos2(label.left(), top),
        vec2(
            width,
            (arrangement.lines.len() - 1) as f32 * SUBJECT_LINE + 26.0,
        ),
    ))
}

/// The header's height in layout (unscrolled).
pub(super) fn height(
    ui: &Ui,
    layout: &Layout,
    detail: &ThreadDetail,
    chips: &[String],
    archivable: bool,
    has_status: bool,
) -> f32 {
    PADDING_TOP
        + arrange(ui, layout, detail, chips, archivable).height
        + if has_status { STATUS_HEIGHT } else { 0.0 }
        + PADDING_BOTTOM
}

/// Paints the header over the messages' top and handles its clicks. Once the messages have
/// scrolled under it, it gets a background and a bottom border. Its actions have Mark Done when
/// `archivable`. `typing` says one of the thread's drafts has the keyboard, and `focus` is the
/// control the keyboard is on.
#[expect(
    clippy::too_many_arguments,
    reason = "one header, drawn from the thread view's state; a struct would only rename them"
)]
pub(super) fn show(
    ui: &Ui,
    layout: &Layout,
    detail: &ThreadDetail,
    chips: &[String],
    archivable: bool,
    height: f32,
    status: Option<&Status>,
    scrolled: bool,
    typing: bool,
    focus: Option<Control>,
    selection: &mut exo_ui::TextSelection,
    now: &jiff::Zoned,
) -> Vec<Action> {
    let palette = exo_ui::palette(ui.ctx());
    let painter = ui.painter();
    let mut actions = Vec::new();
    let top = layout.header.top();
    if scrolled {
        let bottom = top + height - SCROLLED_INSET;
        // Over a widened column the background and its border run the column's width too,
        // painted under the header's own.
        let spans = if layout.is_wide() {
            vec![layout.column.x_range(), layout.header.x_range()]
        } else {
            vec![layout.header.x_range()]
        };
        for span in spans {
            let rect = Rect::from_x_y_ranges(span, top..=bottom);
            painter.rect_filled(
                exo_ui::pixel_snap(rect),
                0.0,
                palette.background_base_default,
            );
            // `border-bottom: 1px solid rgba(0,0,0,.085)`, `hsla(0,0%,100%,.085)` in Carbon.
            let line = exo_ui::pixel_snap(Rect::from_min_max(
                pos2(rect.left(), bottom - 1.0),
                rect.right_bottom(),
            ));
            painter.rect_filled(line, 0.0, palette.ink(0.085, 0.085));
        }
    }
    let label = layout.label;

    // The actions float right: Done (unless the view can't mark done), Remind Me, ⌘K, then
    // Previous and Next thread, each in its place either way.
    let id = ui.id().with("thread-header");
    let right = label.right();
    let icon_top = label.top() + 11.0;
    let done = detail.labels.iter().all(|label| label != "INBOX");
    let reminded = matches!(detail.reminder, Some(exo_store::ReminderStatus::Set { .. }));
    let remind_status = detail
        .reminder
        .as_ref()
        .map(|reminder| crate::remind::status(reminder, now));
    // While one of the thread's drafts is being written, Remind Me's key is ⌘⇧H, as in compose.
    let remind_keys: &[&str] = if typing {
        &["⌘", "shift", "H"]
    } else {
        &["H"]
    };
    let triage: [(bool, TriageButton<'_>, f32, Vec2, f32, Action); 3] = [
        (
            archivable,
            TriageButton::Done { done },
            110.0,
            vec2(16.0, 14.0),
            4.0,
            Action::ToggleDone,
        ),
        (
            true,
            TriageButton::Remind(TriageRemind {
                set: reminded,
                status: remind_status.as_deref(),
                keys: remind_keys,
                within: egui::Rangef::EVERYTHING,
            }),
            86.0,
            Vec2::splat(14.0),
            5.0,
            Action::Remind,
        ),
        (
            true,
            TriageButton::Command,
            63.0,
            Vec2::splat(14.0),
            4.0,
            Action::CommandPalette,
        ),
    ];
    for (shown, button, from_right, size, expand, action) in triage {
        if !shown {
            continue;
        }
        let glyph = Rect::from_min_size(pos2(right - from_right, icon_top), size);
        let lit = focus.is_some_and(|focus| focus.action(status) == action);
        if button.show(ui, id, glyph.expand(expand), glyph, lit) {
            actions.push(action);
        }
    }
    paint_nav_arrows(ui, id, label, &mut actions);

    // The subject, its star and its labels.
    let arrangement = arrange(ui, layout, detail, chips, archivable);
    let starred = detail
        .messages
        .iter()
        .any(exo_store::MessageDetail::is_starred);
    let star = exo_ui::animated_star(ui.ctx(), id.with(("status", &detail.id)), starred);
    let status_at = pos2(label.left() - 1.0, label.top() + 9.0);
    if detail
        .messages
        .iter()
        .any(|message| message.failure.is_some())
    {
        exo_ui::paint_failed_status(painter, status_at);
    } else {
        exo_ui::paint_thread_status(painter, status_at, exo_ui::Unread::No, star);
    }
    for (at, (line, ellipsis)) in arrangement.painted(painter).enumerate() {
        let line_top = label.top() + at as f32 * SUBJECT_LINE;
        // The subject is selectable. Its selection fills the line box, which the subject's
        // `top: 3px` doesn't move. A line broken inside a word joins the next without a space.
        let galley = SUBJECT.single_line(painter, line, Color32::PLACEHOLDER, None);
        let broken = at
            .checked_sub(1)
            .is_some_and(|previous| !arrangement.lines[previous].ends_with(' '));
        selection.add(
            painter,
            pos2(label.left(), line_top),
            galley,
            SUBJECT_LINE,
            super::SELECTED_SUBJECT,
            if broken { "" } else { " " },
            palette.text_selection_default,
        );
        SUBJECT.paint_line(
            painter,
            pos2(label.left(), line_top + SUBJECT_NUDGE),
            line,
            palette.text_base_default,
            None,
        );
        if let Some(x) = ellipsis {
            SUBJECT.paint_line(
                painter,
                pos2(label.left() + x, line_top + SUBJECT_NUDGE),
                ELLIPSIS,
                palette.text_base_default,
                None,
            );
        }
    }
    for (name, line, x) in arrangement.chips {
        let chip = LabelChip::new(painter, &name, false);
        let line_top = label.top() + line as f32 * SUBJECT_LINE;
        // A chip's clickable strip: 28px tall.
        let strip = Rect::from_min_size(pos2(x, line_top + 3.0), vec2(chip.width(), 28.0));
        // A pointer cursor over a chip.
        let response = ui
            .interact(strip, id.with(("label", &name)), Sense::click())
            .on_hover_cursor(egui::CursorIcon::PointingHand);
        let hover = chip_hover(
            ui.ctx(),
            response.id,
            response.hovered(),
            response.is_pointer_button_down_on(),
        );
        chip.title(pos2(x, line_top + CHIP_TOP), &response);
        chip.paint(painter, pos2(x, line_top + CHIP_TOP), hover);
        if response.clicked() {
            actions.push(Action::ShowLabel(name.clone()));
        }
    }

    // The status line under the subject: a button with a tooltip when it does something, plain
    // text otherwise.
    let status_top = label.top() + arrangement.height + 1.0;
    match status {
        Some(status @ (Status::Unsubscribe | Status::Action(_))) => {
            let (text, keys) = match status {
                Status::Action(action) => (action.text.as_str(), &["⌘", "O"]),
                _ => ("Unsubscribe", &["⌘", "U"]),
            };
            let width = STATUS
                .single_line(painter, text, Color32::PLACEHOLDER, None)
                .size()
                .x;
            let hit = Rect::from_min_size(pos2(label.left(), status_top), vec2(width, 25.0));
            // A pointer cursor over a status line that does something.
            let response = ui
                .interact(hit, id.with("status"), Sense::click())
                .on_hover_cursor(egui::CursorIcon::PointingHand);
            STATUS.paint_line(
                painter,
                pos2(label.left(), status_top + 8.0),
                text,
                palette.text_base_accent,
                None,
            );
            let focused = focus == Some(Control::Status);
            if focused {
                // `box-shadow: 0 2px #aeb1dd` under the button's box (`padding: 8px 0 1px`).
                let line = Rect::from_min_size(pos2(label.left(), hit.bottom()), vec2(width, 2.0));
                painter.rect_filled(exo_ui::pixel_snap(line), 0.0, STATUS_FOCUS);
            }
            // The tooltip's left middle sits 10px out from the status's right middle, and the
            // tooltip's middle is 15.5px below the status's top.
            let mut anchor = response.clone();
            anchor.rect.max.y = hit.top() + 31.0;
            KeyTooltip { text, keys }.show_at_focused(
                ui,
                &anchor,
                Placement::Right(10.0),
                focused,
                None,
            );
            if response.clicked() {
                actions.push(Control::Status.action(Some(status)));
            }
        }
        Some(Status::Pending(text)) => {
            STATUS.paint_line(
                painter,
                pos2(label.left(), status_top + 8.0),
                text,
                PENDING,
                None,
            );
        }
        None => {}
    }
    actions
}

/// The legacy chevrons for Previous and Next thread.
const CHEVRON_UP: &str = "legacy/DEPRECATED_actionChevronUp";
const CHEVRON_DOWN: &str = "legacy/DEPRECATED_actionChevronDown";

/// A new message's header: `New Message` and the conversation arrows.
pub(super) fn show_title(ui: &Ui, layout: &Layout, title: &str) -> Vec<Action> {
    let palette = exo_ui::palette(ui.ctx());
    let label = layout.label;
    let id = ui.id().with("thread-header");
    let mut actions = Vec::new();
    paint_nav_arrows(ui, id, label, &mut actions);
    SUBJECT.paint_line(
        ui.painter(),
        pos2(label.left(), label.top() + SUBJECT_NUDGE),
        title,
        palette.text_base_default,
        None,
    );
    actions
}

/// Paints the Previous and Next conversation chevrons at the right of `label`, with their
/// tooltips centered 10px below.
fn paint_nav_arrows(ui: &Ui, id: egui::Id, label: Rect, actions: &mut Vec<Action>) {
    for (icon, from_right, text, key, delta) in [
        (CHEVRON_UP, 40.0, "Previous Conversation", "K", -1),
        (CHEVRON_DOWN, 16.0, "Next Conversation", "J", 1),
    ] {
        let icon_rect = Rect::from_min_size(
            pos2(label.right() - from_right, label.top() + 11.0),
            Vec2::splat(16.0),
        );
        let response =
            IconButton::new(icon).show(ui, id.with(icon), icon_rect.expand(4.0), icon_rect);
        KeyTooltip { text, keys: &[key] }.show_at(
            ui,
            &response,
            Placement::Below {
                gap: 10.0,
                shift: 0.0,
            },
        );
        if response.clicked() {
            actions.push(Action::Navigate(delta));
        }
    }
}

/// The height of a new message's header: one line, no status, and a 4px `margin-bottom`.
pub(super) fn title_height() -> f32 {
    PADDING_TOP + SUBJECT_LINE + PADDING_BOTTOM + 4.0
}

/// The back button: `top: 25px; left: 2rem`, a 32px button with a 16px arrow in it.
pub(super) fn back_button_rect(layout: &Layout) -> Rect {
    Rect::from_min_size(
        pos2(
            layout.action_pane.left() + 2.0 * layout.rem,
            layout.action_pane.top() + 25.0,
        ),
        Vec2::splat(32.0),
    )
}

/// The back button in the gutter, and the gutter's soft gradient while the pointer is over the
/// gutter. `focused` is whether the gutter has the keyboard.
pub(super) fn back_button(ui: &Ui, layout: &Layout, hovered: bool, focused: bool) -> bool {
    let palette = exo_ui::palette(ui.ctx());
    let painter = ui.painter();
    let pane = layout.action_pane;
    // The gutter's gradient: the accent at 20% opacity, 10% at 35.1% of the way, then nothing (in
    // Carbon, `rgba(13, 13, 28, …)` at 15% and 10%). It covers 75% of the left pane's width, at
    // most the gutter's clickable width, and fades over 100ms. It's gone below 864px of thread
    // pane or 20px of clickable width.
    let opacity = motion::transition(
        ui.ctx(),
        ui.id().with("back-gutter"),
        hovered,
        0.1,
        motion::CubicBezier::EASE,
    );
    let clickable = layout.gutter_clickable();
    if opacity > 0.0 && pane.width() + layout.header.width() >= 864.0 && clickable > 20.0 {
        let width = (0.75 * layout.left_pane).min(clickable);
        let ([r, g, b], first) = match exo_ui::theme(ui.ctx()) {
            exo_ui::Theme::Snow => ([174, 177, 221], 0.2),
            exo_ui::Theme::Carbon => ([13, 13, 28], 0.15),
        };
        let stop = |alpha: f32| {
            Color32::from_rgba_unmultiplied(r, g, b, (alpha * opacity * 255.0).round() as u8)
        };
        let stops = [
            (0.0, stop(first)),
            (0.351, stop(0.1)),
            (1.0, Color32::TRANSPARENT),
        ];
        let mut mesh = Mesh::default();
        for (i, (at, color)) in stops.into_iter().enumerate() {
            let x = pane.left() + at * width;
            mesh.colored_vertex(pos2(x, pane.top()), color);
            mesh.colored_vertex(pos2(x, pane.bottom()), color);
            if i > 0 {
                let base = 2 * i as u32;
                mesh.add_triangle(base - 2, base, base + 1);
                mesh.add_triangle(base - 2, base + 1, base - 1);
            }
        }
        painter.add(Shape::mesh(mesh));
    }
    if pane.width() < 1.0 {
        // Beside a popped-out draft the gutter has no room, so its 32px button spills out of it,
        // 8px in and 25px down. The button holds a caret and keeps its tooltip and click, but not
        // its raised circle.
        let rect = Rect::from_min_size(
            pos2(pane.left() + 8.0, pane.top() + 25.0),
            Vec2::splat(32.0),
        );
        let icon = rect.shrink(8.0);
        let button = ui
            .interact(rect, ui.id().with("compact-back-button"), Sense::click())
            .on_hover_cursor(egui::CursorIcon::PointingHand);
        KeyTooltip {
            text: "Back",
            keys: &["esc"],
        }
        .show_at(
            ui,
            &button,
            Placement::Below {
                gap: 10.0,
                shift: 0.0,
            },
        );
        let color = if button.hovered() {
            palette.icon_base_hover
        } else {
            palette.icon_base_default
        };
        exo_ui::paint_icon(
            ui,
            "legacy/DEPRECATED_actionBackCompact",
            exo_ui::pixel_snap(icon),
            color,
        );
        return button.clicked();
    }
    let button = back_button_rect(layout);
    let on_button = hovered && ui.rect_contains_pointer(button);
    // Only the button has the tooltip, 10px under it. Hover-only, so the gutter under it still
    // takes the click.
    let tip = ui.interact(button, ui.id().with("back-button-tooltip"), Sense::hover());
    KeyTooltip {
        text: "Back",
        keys: &["esc"],
    }
    .show_at(
        ui,
        &tip,
        Placement::Below {
            gap: 10.0,
            shift: 0.0,
        },
    );
    // Over the gutter the button is a raised circle, fading in over 150ms (`transition:
    // box-shadow 150ms ease, background 150ms ease`).
    let raised = motion::transition(
        ui.ctx(),
        ui.id().with("back-button"),
        hovered,
        0.15,
        motion::CubicBezier::EASE,
    );
    // With the keyboard on the gutter, the button's shadows in Classic are
    // `0 0 6px 0 #aeb1dd, 0 2px 12px 0 rgba(174,177,221,.5)` instead of the hover's. The button
    // has no background (`background: none`), and a shadow doesn't paint under its box, so the
    // pane shows inside.
    let lit = motion::transition(
        ui.ctx(),
        ui.id().with("back-button-focus"),
        focused,
        0.15,
        motion::CubicBezier::EASE,
    );
    if lit > 0.0 {
        let glow = |alpha: f32| {
            Color32::from_rgba_unmultiplied(174, 177, 221, (alpha * lit * 255.0).round() as u8)
        };
        let circle = exo_ui::pixel_snap(button);
        BoxShadow::new([0.0, 0.0], 6.0, 0.0, glow(1.0)).paint(
            painter,
            circle,
            CornerRadius::same(16),
        );
        BoxShadow::new([0.0, 2.0], 12.0, 0.0, glow(0.5)).paint(
            painter,
            circle,
            CornerRadius::same(16),
        );
        painter.rect_filled(
            circle,
            CornerRadius::same(16),
            palette.background_base_default.gamma_multiply(lit),
        );
    }
    if raised > 0.0 {
        let glow = |alpha: f32| {
            Color32::from_rgba_unmultiplied(174, 177, 221, (alpha * 255.0).round() as u8)
        };
        let ink =
            |alpha: f32| Color32::from_rgba_unmultiplied(13, 13, 28, (alpha * 255.0).round() as u8);
        let shadows = match exo_ui::theme(ui.ctx()) {
            exo_ui::Theme::Snow => [
                BoxShadow::new([0.0, 0.0], 6.0, 0.0, glow(0.4)),
                BoxShadow::new([0.0, 0.0], 6.0, 0.0, glow(0.45)),
                BoxShadow::new([0.0, 0.0], 0.5, 0.0, glow(0.4)),
            ],
            // In Carbon, these are the shadows over the button itself. Over the rest of the
            // gutter, the button takes the gutter's own shadows and inset highlight.
            exo_ui::Theme::Carbon if on_button => [
                BoxShadow::new([0.0, 0.0], 6.0, 0.0, ink(0.2)),
                BoxShadow::new([0.0, 0.0], 6.0, 0.0, ink(0.25)),
                BoxShadow::new([0.0, 0.0], 0.5, 0.0, ink(0.2)),
            ],
            exo_ui::Theme::Carbon => [
                BoxShadow::new([0.0, 0.0], 6.0, 0.0, Color32::from_black_alpha(26)),
                BoxShadow::new([0.0, 2.0], 6.0, 0.0, Color32::from_black_alpha(38)),
                BoxShadow::new([0.0, 0.0], 0.5, 0.0, Color32::from_black_alpha(26)),
            ],
        };
        let circle = exo_ui::pixel_snap(button);
        let mut lifted = painter.clone();
        lifted.multiply_opacity(raised);
        if !focused {
            for shadow in shadows {
                shadow.paint(&lifted, circle, CornerRadius::same(16));
            }
        }
        lifted.rect_filled(
            circle,
            CornerRadius::same(16),
            palette.background_elevated_default,
        );
        // The gutter's `0 1px 0 0 hsla(0, 0%, 100%, .1) inset` in Carbon: white along the top,
        // where the button moved 1px down doesn't cover it. Below the middle the moved circle
        // pokes out, so only the top half is painted.
        if !focused && !on_button && exo_ui::theme(ui.ctx()) == exo_ui::Theme::Carbon {
            let top = lifted.with_clip_rect(Rect::from_min_max(
                circle.min,
                pos2(circle.right(), circle.center().y),
            ));
            top.rect_filled(
                circle,
                CornerRadius::same(16),
                Color32::from_white_alpha(26),
            );
            top.rect_filled(
                circle.translate(vec2(0.0, 1.0)),
                CornerRadius::same(16),
                palette.background_elevated_default,
            );
        }
    }
    // Only the button itself lights the arrow.
    let color = if on_button {
        palette.icon_base_hover
    } else {
        palette.icon_base_default
    };
    exo_ui::paint_icon(
        ui,
        "legacy/DEPRECATED_actionBack",
        exo_ui::pixel_snap(button.shrink(8.0)),
        color,
    );
    // The gutter around it takes the click.
    false
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lines_break_after_spaces_and_hyphens_between_letters() {
        assert_eq!(
            break_points("follow-ups  on 2026-11 -x a--b"),
            ["follow-", "ups  ", "on ", "2026-11 ", "-x ", "a--", "b"]
        );
    }
}

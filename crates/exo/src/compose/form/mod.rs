//! The compose form: high-level layout orchestration, [`Context`], [`Response`], scroll areas,
//! and keyboard tab order across header, body, and footer submodules.
//!
//! Heights follow the CSS flow: the header box pulled up 12px and 4px into its neighbours, the
//! scroll area 4px below it, the body wrapper's top margin collapsing into the push-down box,
//! and the form at least 162px (a reply) or 178px (a new message) with its footer at the bottom.

mod body;
pub(crate) mod footer;
pub(crate) mod header;
mod recipients_ui;

use egui::{Color32, Id, Rangef, Rect, Ui, Vec2, pos2, vec2};
use exo_store::Alias;
use exo_ui::{Family, Font, KeyTooltip, ShortcutTooltip, TextStyle, Theme};

pub(super) use body::reveal_quote;
pub(super) use footer::{listed, uploading};
pub(super) use header::{PLACEHOLDER, choose_from, from_menu, placeholder_header};
pub(super) use recipients_ui::{backspace, commit_all, commit_input, move_selection};

use super::{
    Action, contacts,
    draft::{Field, List, Open},
    recipients,
};

/// Where a form is.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Surface {
    /// Under the message it answers.
    Inline,
    /// The new-message pane.
    Pane,
    /// Popped out beside the thread or list.
    Popout,
}

/// What the form needs besides its draft.
pub(super) struct Context<'a> {
    pub aliases: &'a [Alias],
    /// The account's address, for "Me".
    pub me: &'a str,
    pub surface: Surface,
    /// The form has the keyboard.
    pub focused: bool,
    /// The address book, once read.
    pub book: Option<&'a contacts::Index>,
    /// The open thread's participants, for a reply.
    pub thread_people: Option<&'a contacts::Index>,
    /// The horizontal extent of the thread's scroll area, which the tooltips under the form's
    /// controls stay inside.
    pub pin: Rangef,
    /// What the drop zone covers while files are dragged over the window: the message card the
    /// form is in, or the pop-out's column. `None` covers the form itself.
    pub drop: Option<Rect>,
    /// Now, for `Send: {date}`.
    pub now: &'a jiff::Zoned,
    /// The reminder set on the draft's thread, if any, as its time and whether it's `regardless`.
    /// The Remind button shows it when the draft has none of its own.
    pub thread_reminder: Option<(jiff::Timestamp, bool)>,
    /// The account's `Send + Mark Done` (`sendAndDone`).
    pub send_and_done: bool,
    /// The form's `max-height`, past which the body scrolls between the header and the footer.
    /// `f32::INFINITY` for none.
    pub max_height: f32,
    /// The Share Availability tip, `You can now hit ⌘ shift A…`, may show on a reply.
    pub availability_tip: bool,
}

impl Context<'_> {
    /// Where the controls' tooltips go: under them, or above them in the pop-out.
    pub(super) fn place(&self) -> Place {
        if self.surface == Surface::Popout {
            Place::Above
        } else {
            Place::Below(self.pin)
        }
    }
}

/// Where a control's tooltip goes.
#[derive(Clone, Copy)]
pub(super) enum Place {
    /// Under it, kept inside this extent.
    Below(Rangef),
    /// Above it.
    Above,
}

/// What happened in the form this frame.
pub(super) struct Response {
    pub actions: Vec<Action>,
    /// A press landed inside the form.
    pub clicked_inside: bool,
}

/// The rule over a reply's form: a 1px top border and 17px under it.
pub(super) const SEPARATOR: f32 = 18.0;
/// The form's `max-height: calc(100vh - 120px)`, or `100vh - 116px` for a reply, where `100vh`
/// is the page under the tab strip and over a status bar.
pub(super) const MAX_BELOW_PAGE: f32 = 120.0;
pub(super) const REPLY_MAX_BELOW_PAGE: f32 = 116.0;
/// Room reserved above a snippet card for its name, in both the snippet editor and preview.
pub(crate) const SNIPPET_NAME_ROOM: f32 = 36.0;

pub(crate) const FIELD: TextStyle = TextStyle::new(Font::new(Family::Sans, 700), 14.0, 20.0);
/// The action pane's labels: 12px bold, 16px line.
pub(crate) const SUBMIT: TextStyle = TextStyle::new(Font::new(Family::Sans, 700), 12.0, 16.0);

/// The header: `margin: -12px -8px -4px; padding: 4px 8px; min-height: 40px; max-height:
/// 300px; overflow-y: auto` (border-box), so its rows scroll past 300px.
pub(super) const HEADER_PULL_UP: f32 = 12.0;
pub(super) const HEADER_PULL_DOWN: f32 = 4.0;
pub(super) const HEADER_PADDING: f32 = 4.0;
pub(super) const HEADER_SIDE: f32 = 8.0;
pub(super) const HEADER_MIN: f32 = 40.0;
pub(super) const HEADER_MAX: f32 = 300.0;
/// The collapsed header's `margin: 2px 0 8px` around its 24px line.
pub(super) const COLLAPSED_BLOCK: f32 = 2.0 + 24.0 + 8.0;
/// A recipient row: 28px lines (27px chips and their 1px margin), `margin-bottom: 4px`,
/// `top: -1px`, labels in a 50px column.
pub(super) const LINE: f32 = 28.0;
pub(super) const ROW_GAP: f32 = 4.0;
pub(super) const LABEL_WIDTH: f32 = 50.0;
/// A chip's border box (`padding: 4px 16px 1px 6px` and a 1px border) and its
/// `margin: 0 1px 1px 0`.
pub(super) const CHIP_HEIGHT: f32 = 27.0;
pub(super) const CHIP_EXTRA: f32 = 6.0 + 16.0 + 2.0;
pub(super) const CHIP_TEXT_X: f32 = 7.0;
pub(super) const CHIP_TEXT_Y: f32 = 5.0;
/// The subject: `padding-top: 14px`, a 20px line and `top: 2px`, plus `margin-bottom: 8px` in a
/// reply.
pub(super) const SUBJECT_HEIGHT: f32 = 34.0;
pub(super) const SUBJECT_REPLY_MARGIN: f32 = 8.0;
/// The scroll area: `margin-top: 4px; margin-left: -12px`, with its scrollbar's track at
/// `right: -14px`.
pub(super) const SCROLL_TOP: f32 = 4.0;
pub(super) const SCROLL_LEFT: f32 = 12.0;
pub(super) const SCROLLBAR_RIGHT: f32 = 14.0;
/// The `margin-bottom` of the box that pushes the footer down.
pub(super) const PUSH_DOWN_GAP: f32 = 8.0;
/// The `…` toggle's box: 16px tall.
pub(super) const QUOTING_HEIGHT: f32 = 16.0;
/// The attachment strip: `margin: 4px 0 4px 4px` in the scroll area (`margin-left: -12px`),
/// which makes it 8px wider than the form, with its cards starting 4px left of the form. Its top
/// margin collapses into the push-down box's 8px.
pub(super) const STRIP_WIDER: f32 = 8.0;
/// The footer: `<hr>` (new message) with `margin: 16px 0 -1px`, then the action pane's 20px top
/// margin, 16px line and 16px bottom margin.
const FOOTER_REPLY: f32 = 52.0;
const FOOTER_PANE: f32 = 68.0;
/// The header buttons: Classic's `padding: 4px 8px 7px` around the 12px action, 28×23 boxes
/// floated right and moved 8px into the padding (`right: -8px`), `top: 1px`, or `3px` with the
/// header expanded.
pub(super) const BUTTON: f32 = 28.0;
pub(super) const BUTTON_HEIGHT: f32 = 23.0;
/// A suggestion's name and address: 13px in 28px lines, padded `4px 16px 0`; the name column
/// at least 165px, padding included; the list padded 11px, 8px under the input (offset `2 10`,
/// Classic's `margin: 8px 0 0`); `font-kerning: none`.
pub(super) const SUGGESTION: TextStyle = TextStyle::new(Font::new(Family::Sans, 400), 13.0, 28.0);
pub(super) const SUGGESTION_ROW: f32 = 32.0;
pub(super) const SUGGESTION_PADDING: f32 = 11.0;
pub(super) const SUGGESTION_CELL_PADDING: f32 = 16.0;
pub(super) const SUGGESTION_NAME_MIN: f32 = 165.0;

/// A control's tooltip box starts this far under its 32px box (measured: Send, Attach, Discard).
pub(super) const CONTROL_TOOLTIP_GAP: f32 = 9.0;
/// In the pop-out the tooltip goes above the control, its bottom this far below the top of the
/// control's 32px box (measured: Send, `{ }`, Discard).
pub(super) const CONTROL_TOOLTIP_ABOVE: f32 = -7.0;
/// A header button's tooltip box starts this far under the button's box.
pub(super) const HEADER_TOOLTIP_GAP: f32 = 8.0;

/// The form's vertical metrics for one surface.
struct Metrics {
    padding_top: f32,
    min_height: f32,
    wrapper_margin: f32,
    wrapper_min: f32,
    push_min: f32,
    footer: f32,
}

impl Metrics {
    fn of(surface: Surface, open: &Open) -> Self {
        match surface {
            // A snippet's body wrapper: `margin-top: 6px`.
            Surface::Pane if open.is_snippet() => Self {
                padding_top: 2.0,
                min_height: 178.0,
                wrapper_margin: 6.0,
                wrapper_min: 48.0,
                push_min: 196.0,
                footer: FOOTER_PANE,
            },
            Surface::Pane => Self {
                padding_top: 2.0,
                min_height: 178.0,
                wrapper_margin: 20.0,
                wrapper_min: 48.0,
                push_min: 196.0,
                footer: FOOTER_PANE,
            },
            // A popped-out reply isn't the new-message pane, so it keeps a reply's tighter body.
            Surface::Popout if !open.is_new_email() => Self {
                padding_top: 0.0,
                min_height: 162.0,
                wrapper_margin: if open.state.header_expanded && open.state.cc_bcc_shown {
                    12.0
                } else {
                    0.0
                },
                wrapper_min: 32.0,
                push_min: 196.0,
                footer: FOOTER_PANE,
            },
            Surface::Popout => Self {
                padding_top: 0.0,
                min_height: 178.0,
                wrapper_margin: 20.0,
                wrapper_min: 48.0,
                push_min: 196.0,
                footer: FOOTER_PANE,
            },
            // A reply with Cc, Bcc and From showing gets the body wrapper's `margin-top: 12px`.
            Surface::Inline => Self {
                padding_top: 0.0,
                min_height: 162.0,
                wrapper_margin: if open.state.header_expanded && open.state.cc_bcc_shown {
                    12.0
                } else {
                    0.0
                },
                wrapper_min: 32.0,
                push_min: 32.0,
                footer: FOOTER_REPLY,
            },
        }
    }
}

/// One recipient row as laid out: where its chips and input go.
pub(super) struct Row {
    /// `None` for the From row.
    pub list: Option<List>,
    pub top: f32,
    pub lines: usize,
    pub chips: Vec<(usize, Rect, String)>,
    /// Where the input is: after the chips, in place of the chip being edited, or nowhere while
    /// chips are selected.
    pub input: Option<Rect>,
}

/// Where everything goes, relative to the form's top left.
pub(super) struct Layout {
    pub header_top: f32,
    /// The header's box, and how tall its content is, padding included (more when it scrolls).
    pub header_height: f32,
    pub header_content: f32,
    pub rows: Vec<Row>,
    pub buttons: Vec<Button>,
    pub subject_top: Option<f32>,
    pub body_top: f32,
    pub body_height: f32,
    pub quote_top: Option<f32>,
    /// The attachment strip's top, its cards' size and how many cards fit in a row, when there
    /// are cards.
    pub attachments: Option<(f32, Vec2, usize)>,
    pub footer_top: f32,
    pub height: f32,
    /// The scroll area, when the form is at its max height.
    pub scrolled: Option<Scrolled>,
}

/// The scroll area, when it has less room than its content. It holds its top and bottom,
/// relative to the form, and how tall the body, `…` and files in it are. The body, `…` and files
/// are laid out unscrolled.
#[derive(Clone, Copy)]
pub(super) struct Scrolled {
    pub top: f32,
    pub bottom: f32,
    pub content: f32,
}

/// One of the form's scroll areas: the body's while the form is at its max height, or the
/// header's past 300px. It keeps how far it has scrolled, where the body's caret last was, and
/// its scrollbar.
#[derive(Default)]
pub(crate) struct BodyScroll {
    pub(super) offset: f32,
    /// The scroll area on screen as last drawn, its track included, and how far it scrolls.
    pub(super) area: Option<(Rect, f32)>,
    /// Relative to the editor's top left (the body's).
    pub(super) caret: Option<Rect>,
    /// The caret moved or the body changed, so the caret comes into view. For the header, this
    /// means its input took the keyboard or was typed into.
    pub(super) reveal: bool,
    pub(super) bar: exo_ui::Scrollbar,
}

impl BodyScroll {
    /// Whether the wheel at `pointer`, moving the content `delta` down, scrolls this area
    /// rather than what it's in. It does when the pointer is over the area as last drawn and the
    /// area can still scroll that way. This is Chromium's scroll chaining, where at the area's
    /// end the wheel goes on to the thread.
    pub(crate) fn takes_wheel(&self, pointer: egui::Pos2, delta: f32) -> bool {
        self.area.is_some_and(|(area, most)| {
            area.contains(pointer)
                && if delta > 0.0 {
                    self.offset > 0.0
                } else {
                    delta < 0.0 && self.offset < most
                }
        })
    }
}

/// A header button.
#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) enum Button {
    /// The caret that shows Cc, Bcc and From.
    Expand,
    /// Pop out, or (popped out) pop in.
    Pop,
    /// Close the pop-out.
    Close,
}

impl Button {
    /// Its box's width. Close's `padding-left: 7px` makes Close a pixel narrower.
    pub(super) fn width(self) -> f32 {
        if self == Self::Close {
            BUTTON - 1.0
        } else {
            BUTTON
        }
    }

    /// The field it is when it has the keyboard.
    pub(super) fn field(self) -> Field {
        match self {
            Self::Expand => Field::ExpandRecipients,
            Self::Pop => Field::PopAction,
            Self::Close => Field::PopClose,
        }
    }
}

/// The header buttons the form shows, in order. A snippet doesn't pop out.
fn buttons(open: &Open, surface: Surface) -> Vec<Button> {
    let mut buttons = if open.state.header_expanded && !open.state.cc_bcc_shown {
        vec![Button::Expand]
    } else {
        Vec::new()
    };
    if open.is_snippet() {
        return buttons;
    }
    buttons.push(Button::Pop);
    if surface == Surface::Popout {
        buttons.push(Button::Close);
    }
    buttons
}

pub(crate) fn active_aliases(aliases: &[Alias]) -> usize {
    aliases.iter().filter(|alias| alias.active).count()
}

pub(super) fn ink(ctx: &egui::Context, alpha: f32) -> Color32 {
    exo_ui::palette(ctx).ink(alpha, alpha)
}

pub(super) fn is_dark(ctx: &egui::Context) -> bool {
    exo_ui::theme(ctx) == Theme::Carbon
}

fn layout(ui: &Ui, open: &mut Open, width: f32, left: f32, cx: &Context<'_>) -> Layout {
    let metrics = Metrics::of(cx.surface, open);
    let ctx = ui.ctx();
    let header_top = metrics.padding_top - HEADER_PULL_UP;
    let snippet_pad = if open.is_snippet() && open.state.header_expanded {
        4.0
    } else {
        0.0
    };
    let content_top = header_top + HEADER_PADDING + snippet_pad;
    let buttons = buttons(open, cx.surface);
    // The buttons float right. `top: 3px` (expanded) and `right: -8px` are relative, so the
    // buttons leave the float's place where it was. A list is a flex container, so the float
    // narrows the whole list beside it, not just that list's lines beside it.
    let float_bottom = content_top + BUTTON_HEIGHT;
    let float_width: f32 = buttons.iter().map(|button| button.width()).sum();
    let mut rows = Vec::new();
    let content_height = if open.state.header_expanded {
        let mut y = content_top - 1.0;
        let mut lists: Vec<Option<List>> = vec![Some(List::To)];
        if open.state.cc_bcc_shown {
            lists.extend([Some(List::Cc), Some(List::Bcc)]);
            if active_aliases(cx.aliases) > 1 {
                lists.push(None);
            }
        }
        for list in lists {
            let row = recipients_ui::lay_out_row(
                ctx,
                open,
                list,
                y,
                if y < float_bottom {
                    width - float_width
                } else {
                    width
                },
            );
            y += row.lines as f32 * LINE + ROW_GAP;
            rows.push(row);
        }
        y + 1.0 - content_top
    } else if open.is_snippet() {
        // No `Draft to …` for a snippet.
        0.0
    } else {
        COLLAPSED_BLOCK
    };
    // A snippet's header has no `min-height`, and expanded it has `padding-top: 8px`.
    let header_content = if open.is_snippet() {
        content_height + 2.0 * HEADER_PADDING + snippet_pad
    } else {
        (content_height + 2.0 * HEADER_PADDING).max(HEADER_MIN)
    };
    let header_height = header_content.min(HEADER_MAX);
    let mut y = header_top + header_height - HEADER_PULL_DOWN;
    // A snippet's subject shows with its recipients.
    let subject_top = if open.is_snippet() && !open.state.header_expanded {
        None
    } else if open.is_new_email() {
        let top = y;
        y += SUBJECT_HEIGHT;
        Some(top)
    } else if open.state.header_expanded && open.state.cc_bcc_shown {
        let top = y;
        y += SUBJECT_HEIGHT + SUBJECT_REPLY_MARGIN;
        Some(top)
    } else {
        None
    };
    y += SCROLL_TOP;
    let scroll_top = y;
    let body_top = y + metrics.wrapper_margin;
    let editor = open.editor.height(ui, width, left);
    open.body.height = editor;
    let body_height = editor.max(metrics.wrapper_min);
    let quote_top = open
        .has_hidden_quote(cx.aliases)
        .then_some(body_top + body_height);
    let quote_height = if quote_top.is_some() {
        QUOTING_HEIGHT
    } else {
        0.0
    };
    y = body_top + metrics.push_min.max(body_height + quote_height) + PUSH_DOWN_GAP;
    let cards = listed(open).count();
    let attachments = (cards > 0).then(|| {
        let (card, per_row) = exo_ui::attachment_grid(width + STRIP_WIDER);
        let top = y;
        let strip = cards.div_ceil(per_row) as f32 * (card.y + exo_ui::CARD_MARGIN_BOTTOM);
        // The strip's bottom margin snaps it to the 4px grid.
        y = top + strip + 12.0 - strip % 4.0;
        (top, card, per_row)
    });
    let natural = (y + metrics.footer).max(metrics.min_height);
    // When the form is taller than its `max-height` (a `min-height` wins over it), the scroll
    // area takes what's left between the header and the footer.
    let height = natural.min(cx.max_height).max(metrics.min_height);
    let scrolled = (height < natural).then_some(Scrolled {
        top: scroll_top,
        bottom: height - metrics.footer,
        content: y - scroll_top,
    });
    Layout {
        header_top,
        header_height,
        header_content,
        rows,
        buttons,
        subject_top,
        body_top,
        body_height,
        quote_top,
        attachments,
        footer_top: height - metrics.footer,
        height,
        scrolled,
    }
}

/// The form's height at `width`.
pub(super) fn height(ui: &Ui, open: &mut Open, width: f32, cx: &Context<'_>) -> f32 {
    layout(ui, open, width, 0.0, cx).height
}

/// Paints a 1px horizontal divider (`#ededed` in Snow, `#5f6165` in Carbon), shared across inline
/// compose separators, pane/popout footers, and the read-only snippet card footer.
pub(crate) fn paint_rule(ui: &Ui, left: f32, top: f32, width: f32) {
    let color = if is_dark(ui.ctx()) {
        Color32::from_rgb(0x5f, 0x61, 0x65)
    } else {
        Color32::from_rgb(0xed, 0xed, 0xed)
    };
    ui.painter().rect_filled(
        exo_ui::pixel_snap(Rect::from_min_size(pos2(left, top), vec2(width, 1.0))),
        0.0,
        color,
    );
}

/// The inline form's separator: `border-top: 1px solid #ededed` (dark `#5f6165`), literals.
pub(super) fn separator(ui: &Ui, left: f32, top: f32, width: f32) {
    paint_rule(ui, left, top, width);
}

/// A 2px focus line in `color` across `left..right` from `top` (`box-shadow: 0 2px`, or a
/// `border-bottom: 2px`).
pub(crate) fn underline(ui: &Ui, left: f32, right: f32, top: f32, color: Color32) {
    ui.painter().rect_filled(
        exo_ui::pixel_snap(Rect::from_min_max(pos2(left, top), pos2(right, top + 2.0))),
        0.0,
        color,
    );
}

/// The tooltip of a control in the form, while it's hovered or `focused`: under it, or above it
/// in the pop-out.
pub(super) fn tooltip(
    ui: &Ui,
    place: Place,
    anchor: &egui::Response,
    focused: bool,
    rows: &[KeyTooltip<'_>],
    note: Option<&str>,
    gap: f32,
) {
    let tip = ShortcutTooltip { rows, note };
    match place {
        Place::Below(within) => tip.show_below(ui, anchor, gap, focused, within),
        Place::Above => tip.show_above(ui, anchor, CONTROL_TOOLTIP_ABOVE, focused),
    }
}

/// Gives the widget `id` the keyboard when the form has it and `wanted` says it's this one's.
pub(super) fn focus_if(ui: &Ui, cx: &Context<'_>, wanted: bool, id: Id) {
    if cx.focused && wanted && !ui.memory(|memory| memory.has_focus(id)) {
        ui.memory_mut(|memory| memory.request_focus(id));
    }
}

/// Draws the form at `rect`'s top left, `rect.width()` wide, and handles its input.
pub(super) fn show(ui: &mut Ui, open: &mut Open, rect: Rect, cx: &Context<'_>) -> Response {
    let ctx = ui.ctx().clone();
    let now = ctx.input(|input| input.time);
    let width = rect.width();
    let origin = rect.min;
    let layout = layout(ui, open, width, origin.x, cx);
    let form = Rect::from_min_size(origin, vec2(width, layout.height));
    let mut response = Response {
        actions: Vec::new(),
        clicked_inside: false,
    };
    let pressed_at = ctx.input(|input| {
        input
            .pointer
            .any_pressed()
            .then(|| input.pointer.press_origin())
            .flatten()
    });
    response.clicked_inside =
        pressed_at.is_some_and(|at| form.contains(at) && ui.clip_rect().contains(at));
    let base = exo_ui::id(&ctx, ("compose", open.id.clone()));
    // On a field nothing is typed into (a button, From, a reply's subject, the `…`), no text
    // widget keeps the keyboard, so keys go to the field's commands.
    if cx.focused && !open.takes_text() {
        ctx.memory_mut(|memory| {
            if let Some(focused) = memory.focused() {
                memory.surrender_focus(focused);
            }
        });
    }

    header::show_snippet_name(ui, open, origin, width, base, cx, &mut response, now);
    header::show_header(
        ui,
        open,
        &layout,
        origin,
        width,
        base,
        cx,
        &mut response,
        now,
    );
    if let Some(top) = layout.subject_top {
        header::show_subject(ui, open, top, origin, width, base, cx, &mut response, now);
    }
    body::show(
        ui,
        open,
        form,
        &layout,
        origin,
        width,
        base,
        cx,
        &mut response,
        now,
    );
    footer::show_footer(
        ui,
        open,
        form,
        &layout,
        origin,
        width,
        base,
        cx,
        &mut response,
    );
    response
}

/// The field Tab (1) or ⇧Tab (-1) moves to, in the form's tab order, which wraps around: the
/// header's fields, the subject (a reply's too, read-only, when Cc and Bcc show), the body,
/// Send, the icons, then the header buttons, the last first. Send later and Remind me follow
/// Send (but not on a snippet).
pub(super) fn next_field(open: &Open, aliases: &[Alias], surface: Surface, delta: isize) -> Field {
    let mut order = Vec::new();
    // A snippet's name comes first, and a snippet has no collapsed header.
    if open.is_snippet() {
        order.push(Field::SnippetName);
    }
    if open.state.header_expanded {
        order.push(Field::To);
        if open.state.cc_bcc_shown {
            order.extend([Field::Cc, Field::Bcc]);
            if active_aliases(aliases) > 1 {
                order.push(Field::From);
            }
        }
    } else if !open.is_snippet() {
        order.push(Field::CollapsedHeader);
    }
    let subject = if open.is_snippet() {
        open.state.header_expanded
    } else {
        open.is_new_email() || open.state.header_expanded && open.state.cc_bcc_shown
    };
    if subject {
        order.push(Field::Subject);
    }
    order.extend([Field::Body, Field::Send]);
    if !open.is_snippet() {
        order.extend([Field::SendLater, Field::RemindMe]);
    }
    order.extend([Field::Attach, Field::Discard]);
    order.extend(
        buttons(open, surface)
            .iter()
            .rev()
            .map(|button| button.field()),
    );
    // The `…` isn't in the tab order, so from it Tab goes on as from the body.
    let current = if open.state.field == Field::QuotingStart {
        Field::Body
    } else {
        open.state.field
    };
    let at = order
        .iter()
        .position(|field| *field == current)
        .unwrap_or(0);
    order[(at as isize + delta).rem_euclid(order.len() as isize) as usize]
}

/// The form as it prints: `Draft to …`, or the recipient lists the header shows (From left out),
/// then what's written, then the `…` when a quote or signature is behind one.
pub(super) fn printout(open: &Open, aliases: &[Alias], me: &str) -> crate::print::Draft {
    let header = if open.state.header_expanded {
        let mut lists = vec![List::To];
        if open.state.cc_bcc_shown {
            lists.extend([List::Cc, List::Bcc]);
        }
        let chips = |list: List| {
            open.list(list)
                .iter()
                .map(|address| crate::print::Chip {
                    label: recipients_ui::chip_label(address),
                    invalid: !recipients::sendable(address),
                })
                .collect()
        };
        crate::print::DraftHeader::Lists(
            lists
                .into_iter()
                .map(|list| (list.label(), chips(list)))
                .collect(),
        )
    } else {
        crate::print::DraftHeader::Collapsed(header::collapsed_text(open, me))
    };
    crate::print::Draft {
        header,
        body: open.editor.html(),
        quote: open.has_hidden_quote(aliases),
    }
}

#[cfg(test)]
pub(super) mod test_support {
    use exo_store::{Address, Alias, DraftAction, DraftRecord};

    use super::Open;

    pub(super) fn reply(cc: &[&str]) -> Open {
        let address = |email: &&str| Address {
            name: None,
            email: (*email).into(),
        };
        let record = DraftRecord {
            action: DraftAction::ReplyAll,
            from: Some(Address {
                name: None,
                email: "maya@brightmoor.co".into(),
            }),
            to: vec![Address {
                name: None,
                email: "tom@brightmoor.co".into(),
            }],
            cc: cc.iter().map(address).collect(),
            bcc: Vec::new(),
            subject: "Re: Lunch".into(),
            body: String::new(),
            quoted_content: None,
            quoted_inlined: false,
            in_reply_to: None,
            in_reply_to_rfc822: None,
            references: Vec::new(),
            rfc822_id: "x@y".into(),
            fingerprint: String::new(),
            attachments: Vec::new(),
            send_later: None,
            reminder: None,
        };
        Open::new("draft00aaaaaaaaaaaaaa".into(), Some("t".into()), record)
    }

    pub(super) fn aliases(count: usize) -> Vec<Alias> {
        [
            "maya@brightmoor.co",
            "hello@mayaokafor.com",
            "a@brightmoor.co",
        ][..count]
            .iter()
            .map(|email| Alias {
                email: (*email).into(),
                name: None,
                signature: None,
                default: false,
                primary: false,
                active: true,
            })
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::{
        test_support::{aliases, reply},
        *,
    };

    fn walk(
        open: &mut Open,
        aliases: &[Alias],
        surface: Surface,
        delta: isize,
        steps: usize,
    ) -> Vec<Field> {
        (0..steps)
            .map(|_| {
                open.state.field = next_field(open, aliases, surface, delta);
                open.state.field
            })
            .collect()
    }

    #[test]
    fn tab_goes_through_send_and_the_icons_and_wraps() {
        let mut collapsed = reply(&[]);
        assert_eq!(collapsed.state.field, Field::Body);
        let order = walk(&mut collapsed, &aliases(1), Surface::Inline, 1, 8);
        assert_eq!(
            order,
            [
                Field::Send,
                Field::SendLater,
                Field::RemindMe,
                Field::Attach,
                Field::Discard,
                Field::PopAction,
                Field::CollapsedHeader,
                Field::Body
            ]
        );
        assert_eq!(
            walk(&mut collapsed, &aliases(1), Surface::Inline, -1, 2),
            [Field::CollapsedHeader, Field::PopAction]
        );

        let mut expanded = reply(&["priya@brightmoor.co"]);
        expanded.state.expand(Field::Discard);
        let order = walk(&mut expanded, &aliases(2), Surface::Popout, 1, 8);
        assert_eq!(
            order,
            [
                Field::PopClose,
                Field::PopAction,
                Field::To,
                Field::Cc,
                Field::Bcc,
                Field::From,
                Field::Subject,
                Field::Body
            ],
            "a reply's subject is in the order once Cc shows"
        );
    }
}

//! Compose header: snippet name field, folded/expanded header rows, `From` alias dropdown,
//! header buttons & tooltips, editable/read-only subject line, and shared read-only recipient +
//! subject layout for snippet previews.

use egui::{Color32, Id, Pos2, Rect, Sense, Ui, UiBuilder, pos2, vec2};
use exo_store::{Address, Alias, Contact, MessageDetail};
use exo_ui::{Family, Font, IconButton, KeyTooltip, TextStyle};

use super::{
    BUTTON, BUTTON_HEIGHT, BodyScroll, Button, CHIP_TEXT_Y, Context, FIELD, HEADER_PADDING,
    HEADER_SIDE, HEADER_TOOLTIP_GAP, LABEL_WIDTH, Layout, Place, Response, SUBJECT_HEIGHT,
    SUGGESTION_PADDING, Surface, active_aliases, focus_if, ink, recipients_ui, tooltip, underline,
};
use crate::{
    compose::{
        Action,
        draft::{Field, Open, SubjectReplacement},
        recipients, signature, subject,
    },
    format::names::{self, NameFormat, Person},
};

/// The collapsed header: 14px bold in a 24px line.
const COLLAPSED: TextStyle = TextStyle::new(Font::new(Family::Sans, 700), 14.0, 24.0);
/// The From list's addresses: 13px in 28px rows, padded 18px.
const FROM_ITEM: TextStyle = TextStyle::new(Font::new(Family::Sans, 400), 13.0, 28.0);
const FROM_ITEM_PADDING: f32 = 18.0;
/// The address that `"use default"` (the draft's own From) sorts by among the aliases in a
/// snippet's From list. It is never sent or saved, because a snippet saves its default From as
/// `None`.
const USE_DEFAULT: &str = "default.snippet.address@exo.invalid";

/// What stays under the message while its draft is popped out, after the separator: the
/// collapsed `Draft to …` (`margin: -2px 0 12px`) and the pop-in button. The card ends 38px
/// below the separator (measured), its own 4px padding included.
pub(in crate::compose) const PLACEHOLDER: f32 = 34.0;

/// Draws a snippet's name field above the card (`position: absolute; padding-top: 16px;
/// margin-top: -44px`, its line 38px above the form).
#[expect(
    clippy::too_many_arguments,
    reason = "renders the snippet title field from the form's layout coordinates"
)]
pub(super) fn show_snippet_name(
    ui: &mut Ui,
    open: &mut Open,
    origin: Pos2,
    width: f32,
    base: Id,
    cx: &Context<'_>,
    response: &mut Response,
    now: f64,
) {
    let ctx = ui.ctx().clone();
    let palette = exo_ui::palette(&ctx);
    let field_focused = open.state.field == Field::SnippetName;
    let Some(name) = open.snippet_name_mut() else {
        return;
    };
    let id = base.with("snippet-name");
    let line = Rect::from_min_size(pos2(origin.x, origin.y - 38.0), vec2(width + 27.0, 20.0));
    if name.is_empty() {
        FIELD.paint_line(
            ui.painter(),
            line.min,
            "Enter description",
            ink(&ctx, 0.5),
            None,
        );
    }
    let before = name.clone();
    let field = crate::spelling::text_field(
        ui,
        id,
        name,
        line,
        &FIELD,
        ink(&ctx, 0.8),
        palette.text_selection_default,
    );
    let changed = *name != before;
    if field.gained_focus() || field.clicked() {
        response.actions.push(Action::Focus(Field::SnippetName));
    }
    focus_if(ui, cx, field_focused, id);
    if changed {
        open.touch(now);
    }
}

/// Draws the form's header box (expanded rows + `From` + drag ghost, or collapsed `Draft to …`,
/// plus the right-floated header buttons and optional scrollbar).
#[expect(
    clippy::too_many_arguments,
    reason = "orchestrates header rows, buttons, and scrollbar within the form layout"
)]
pub(super) fn show_header(
    ui: &mut Ui,
    open: &mut Open,
    layout: &Layout,
    origin: Pos2,
    width: f32,
    base: Id,
    cx: &Context<'_>,
    response: &mut Response,
    now: f64,
) {
    let ctx = ui.ctx().clone();
    let header_box = Rect::from_min_size(
        pos2(origin.x - HEADER_SIDE, origin.y + layout.header_top),
        vec2(width + 2.0 * HEADER_SIDE, layout.header_height),
    );
    // What comes into view is the input's caret, which spans the line of its text down to the
    // bottom of its content area (measured: 23px into the input).
    let caret_bottom = CHIP_TEXT_Y + FIELD.content_bottom(&ctx);
    let input = layout
        .rows
        .iter()
        .find(|row| {
            row.list
                .is_some_and(|list| cx.focused && open.state.field == list.field())
        })
        .and_then(|row| row.input)
        .map(|input| {
            Rect::from_x_y_ranges(
                input.x_range(),
                input.top() + CHIP_TEXT_Y..=input.top() + caret_bottom,
            )
        });
    let header_scroll = scroll_header(ui, open, layout, header_box, input);
    let mut header_ui = ui.new_child(
        UiBuilder::new()
            .max_rect(header_box)
            .id_salt(("compose-header", &open.id)),
    );
    if header_scroll.is_some() {
        header_ui.set_clip_rect(header_box.intersect(ui.clip_rect()));
    }
    let header_origin = origin - vec2(0.0, header_scroll.unwrap_or(0.0));
    let content_top = header_origin.y + layout.header_top + HEADER_PADDING;
    if open.state.header_expanded {
        for row in &layout.rows {
            recipients_ui::show_row(
                &mut header_ui,
                open,
                row,
                header_origin,
                base,
                cx,
                response,
                now,
            );
        }
        // A drag lasts while the button's down. Let go off a list and nothing moves.
        if ctx.input(|input| input.pointer.primary_down()) {
            if let Some(drag) = &open.header.chip_drag {
                recipients_ui::paint_drag_image(ui, drag);
            }
        } else {
            open.header.chip_drag = None;
        }
    } else if !open.is_snippet() {
        let line_top = content_top + 2.0;
        // Clicking the collapsed header (or Enter on it) expands it with To focused.
        let header = collapsed_header(
            ui,
            open,
            pos2(origin.x, line_top),
            width - BUTTON,
            cx,
            true,
            base.with("collapsed"),
        );
        if header.clicked() {
            response.actions.push(Action::Focus(Field::To));
        }
    }
    let mut right = origin.x + width + 8.0;
    for button in layout.buttons.iter().rev() {
        let button_width = button.width();
        right -= button_width;
        // `top: 3px`, or `1px` on a collapsed header, whose line starts 2px lower.
        let hit = Rect::from_min_size(
            pos2(right, content_top + 3.0),
            vec2(button_width, BUTTON_HEIGHT),
        );
        let focused = cx.focused && open.state.field == button.field();
        if let Some(action) = header_button(
            &header_ui,
            *button,
            hit,
            cx,
            cx.surface == Surface::Popout,
            focused,
            base,
        ) {
            response.actions.push(action);
        }
    }
    if let Some(offset) = header_scroll {
        open.header.scroll.bar.show(
            ui,
            base.with("header-scrollbar"),
            header_box,
            offset,
            layout.header_content,
        );
    }
}

/// Draws the subject line: editable on a new message (with Unicode replacement and `:` / `;`
/// triggers), or read-only and clickable on a reply.
#[expect(
    clippy::too_many_arguments,
    reason = "draws the subject line from the form's layout coordinates and focus context"
)]
pub(super) fn show_subject(
    ui: &mut Ui,
    open: &mut Open,
    top: f32,
    origin: Pos2,
    width: f32,
    base: Id,
    cx: &Context<'_>,
    response: &mut Response,
    now: f64,
) {
    let ctx = ui.ctx().clone();
    let palette = exo_ui::palette(&ctx);
    let line = Rect::from_min_size(
        pos2(origin.x, origin.y + top + 2.0 + 14.0),
        vec2(width - 27.0, 20.0),
    );
    if open.is_new_email() {
        let id = base.with("subject");
        if open.record.subject.is_empty() {
            FIELD.paint_line(ui.painter(), line.min, "Subject", ink(&ctx, 0.5), None);
        }
        let focused_here = cx.focused && open.state.field == Field::Subject;
        if focused_here {
            undo_subject_replacement(ui, open, id);
        }
        let typed = subject::typed_trigger(ui);
        let typed_text = ui.input(|input| {
            input
                .events
                .iter()
                .any(|event| matches!(event, egui::Event::Text(_)))
        });
        let before = open.record.subject.clone();
        let field = crate::spelling::text_field(
            ui,
            id,
            &mut open.record.subject,
            line,
            &FIELD,
            ink(&ctx, 0.8),
            palette.text_selection_default,
        );
        // Text dragged from a body or another app goes in where it's let go, as an `<input>`
        // takes a drop.
        if exo_ui::text_drag::field(
            ui,
            id,
            &mut open.record.subject,
            line,
            &FIELD,
            ink(&ctx, 0.8),
        )
        .is_some()
        {
            response.actions.push(Action::Focus(Field::Subject));
        }
        if open.record.subject != before {
            if typed_text {
                replace_in_subject(ui, open, id, &before);
            }
            open.touch(now);
        }
        if focused_here {
            subject::update(ui, open, id, line, &FIELD, typed);
        } else {
            open.popups.subject.trigger = None;
        }
        if field.gained_focus() || field.clicked() {
            response.actions.push(Action::Focus(Field::Subject));
        }
        focus_if(ui, cx, open.state.field == Field::Subject, id);
        if focused_here && std::mem::take(&mut open.header.select_subject) {
            let all = egui::text::CCursorRange::two(
                egui::text::CCursor::new(0),
                egui::text::CCursor::new(open.record.subject.chars().count()),
            );
            exo_ui::focus_field(ui.ctx(), id, all);
        }
    } else {
        // A reply's subject is read-only, and clicking it, or Enter on it, starts a new
        // thread. Focused, it's underlined (`box-shadow: 0 2px`).
        FIELD.paint_line(
            ui.painter(),
            line.min,
            &open.record.subject,
            ink(&ctx, 0.8),
            Some(line.width()),
        );
        let subject = Rect::from_min_size(
            pos2(origin.x, origin.y + top + 2.0),
            vec2(width, SUBJECT_HEIGHT),
        );
        if cx.focused && open.state.field == Field::Subject {
            underline(
                ui,
                subject.left(),
                subject.right(),
                subject.bottom(),
                palette.border_base_active,
            );
        }
        if ui
            .interact(subject, base.with("subject"), Sense::click())
            .on_hover_cursor(egui::CursorIcon::PointingHand)
            .clicked()
        {
            response.actions.push(Action::EditSubject);
        }
    }
}

/// The header's rows past its max height, with its box `area` on screen. The wheel over the box
/// scrolls the rows. The focused recipient input's caret line (`input`, relative to the form)
/// comes into view at its nearest edge as the input takes the keyboard and as it's typed into,
/// as Chromium reveals a field's caret. A comma or Enter that makes a chip (no text typed) moves
/// the input without bringing it into view. Returns how far the rows have scrolled, when they
/// can.
fn scroll_header(
    ui: &Ui,
    open: &mut Open,
    layout: &Layout,
    area: Rect,
    input: Option<Rect>,
) -> Option<f32> {
    let most = layout.header_content - layout.header_height;
    let scroll = &mut open.header.scroll;
    if most <= 0.0 {
        *scroll = BodyScroll::default();
        return None;
    }
    if ui.rect_contains_pointer(area) {
        scroll.offset -= ui.input(|input| input.smooth_scroll_delta.y);
    }
    if let Some(shown) = input.filter(|_| std::mem::take(&mut scroll.reveal)) {
        let (top, bottom) = (
            layout.header_top + scroll.offset,
            layout.header_top + layout.header_height + scroll.offset,
        );
        if shown.top() < top {
            scroll.offset -= top - shown.top();
        } else if shown.bottom() > bottom {
            scroll.offset += shown.bottom() - bottom;
        }
    }
    let pixels = ui.pixels_per_point();
    scroll.offset = ((scroll.offset * pixels).round() / pixels).clamp(0.0, most);
    scroll.area = Some((area, most));
    Some(scroll.offset)
}

/// One header button in its box, with its tooltip. Returns what a click asks for.
fn header_button(
    ui: &Ui,
    button: Button,
    hit: Rect,
    cx: &Context<'_>,
    popped_out: bool,
    focused: bool,
    base: Id,
) -> Option<Action> {
    let palette = exo_ui::palette(ui.ctx());
    let many_aliases = active_aliases(cx.aliases) > 1;
    let expand_rows = [
        KeyTooltip {
            text: "Cc",
            keys: &["⌘", "shift", "C"],
        },
        KeyTooltip {
            text: "Bcc",
            keys: &["⌘", "shift", "B"],
        },
        KeyTooltip {
            text: "From",
            keys: &["⌘", "shift", "F"],
        },
    ];
    let (icon, rows, action): (&str, &[KeyTooltip<'_>], Action) = match button {
        Button::Expand => (
            "legacy/DEPRECATED_downCaret",
            if many_aliases {
                &expand_rows[..]
            } else {
                &expand_rows[..2]
            },
            Action::Focus(Field::Cc),
        ),
        Button::Pop if popped_out => (
            "legacy/DEPRECATED_popin",
            &[KeyTooltip {
                text: "Pop In",
                keys: &["⌘", "shift", "P"],
            }],
            Action::ClickPop,
        ),
        Button::Pop => (
            "legacy/DEPRECATED_popout",
            &[
                KeyTooltip {
                    text: "Pop Out",
                    keys: &["⌘", "shift", "P"],
                },
                KeyTooltip {
                    text: "Pop Out & Search",
                    keys: &["⌘", "/"],
                },
            ],
            Action::ClickPop,
        ),
        Button::Close => (
            "legacy/DEPRECATED_popoutclose",
            &[KeyTooltip {
                text: "Close",
                keys: &["esc"],
            }],
            Action::ClickClosePopout,
        ),
    };
    // The icon is centered in a 12px box, and its focus box is 12×17.
    // The action sits 8px from the box's right and 4px from its top.
    let action_box = Rect::from_min_size(
        pos2(hit.right() - 8.0 - 12.0, hit.top() + 4.0),
        vec2(12.0, 12.0),
    );
    let icon_rect = exo_ui::icon_rect(ui.ctx(), icon, action_box);
    let rest = if focused {
        palette.icon_base_hover
    } else {
        palette.icon_base_default
    };
    let clicked = IconButton::new(icon)
        .colors(rest, palette.icon_base_hover)
        .show(ui, base.with(icon), hit, icon_rect);
    if focused {
        // Focused, it gets a `box-shadow: 0 2px …` under its 17px box.
        underline(
            ui,
            action_box.left(),
            action_box.right(),
            action_box.top() + 17.0,
            palette.border_base_active,
        );
    }
    // Header buttons' tooltips are pinned to the window, not the scroll container.
    let place = match cx.place() {
        Place::Below(_) => Place::Below(ui.ctx().content_rect().x_range()),
        above => above,
    };
    tooltip(ui, place, &clicked, focused, rows, None, HEADER_TOOLTIP_GAP);
    clicked.clicked().then_some(action)
}

/// `Draft to …` at `at`, at most `max_width` wide, and the tooltip listing the field shortcuts
/// when it's hovered. `subject` false leaves out Subject, on the pop-out's placeholder. Only
/// its text takes the pointer (`pointer-events: none` around it).
fn collapsed_header(
    ui: &Ui,
    open: &Open,
    at: Pos2,
    max_width: f32,
    cx: &Context<'_>,
    subject: bool,
    id: Id,
) -> egui::Response {
    let palette = exo_ui::palette(ui.ctx());
    let text = collapsed_text(open, cx.me);
    let painter = ui.painter();
    let draft_width = COLLAPSED.paint_line(painter, at, "Draft", palette.actions_draft, None);
    let rest_width = COLLAPSED.paint_line(
        painter,
        pos2(at.x + draft_width, at.y),
        &format!(" to {text}"),
        palette.ink(0.8, 0.8),
        Some(max_width - draft_width),
    );
    let hit = Rect::from_min_size(at, vec2(draft_width + rest_width, COLLAPSED.line_height));
    let response = ui
        .interact(hit, id, Sense::click())
        .on_hover_cursor(egui::CursorIcon::PointingHand);
    let rows = [
        KeyTooltip {
            text: "To",
            keys: &["⌘", "shift", "O"],
        },
        KeyTooltip {
            text: "Cc",
            keys: &["⌘", "shift", "C"],
        },
        KeyTooltip {
            text: "Bcc",
            keys: &["⌘", "shift", "B"],
        },
        KeyTooltip {
            text: "From",
            keys: &["⌘", "shift", "F"],
        },
        KeyTooltip {
            text: "Subject",
            keys: &["⌘", "shift", "S"],
        },
    ];
    let rows: Vec<KeyTooltip<'_>> = rows
        .into_iter()
        .filter(|row| {
            (row.text != "From" || active_aliases(cx.aliases) > 1)
                && (row.text != "Subject" || subject)
        })
        .collect();
    // The tooltip box's top is 3px under the line (measured).
    tooltip(ui, cx.place(), &response, false, &rows, None, 3.0);
    response
}

/// Draws the popped-out draft's placeholder from `top` (the separator's bottom) across
/// `left..left + width`. Returns what it asked for.
pub(in crate::compose) fn placeholder_header(
    ui: &Ui,
    open: &Open,
    left: f32,
    top: f32,
    width: f32,
    cx: &Context<'_>,
) -> Vec<Action> {
    let id = exo_ui::id(ui.ctx(), ("compose-placeholder", &open.id));
    let _ = collapsed_header(
        ui,
        open,
        pos2(left, top - 2.0),
        width - BUTTON,
        cx,
        false,
        id.with("header"),
    );
    // Its icon sits 3px below the separator (measured).
    let hit = Rect::from_min_size(
        pos2(left + width + 8.0 - BUTTON, top - 1.0),
        vec2(BUTTON, BUTTON_HEIGHT),
    );
    header_button(ui, Button::Pop, hit, cx, true, false, id)
        .into_iter()
        .collect()
}

/// The `{recipients}` of `Draft to {recipients}`: one person by full name, or several by first
/// name (leaving you out), joined with `, ` and ` & `, then `. Bcc: …`. At most 15 are named
/// before the Bcc.
pub(super) fn collapsed_text(open: &Open, me: &str) -> String {
    let is_me = |address: &Address| recipients::same_address(&address.email, me);
    let person = |address: &Address| {
        Person::new(
            address.name.clone().unwrap_or_default(),
            address.email.clone(),
        )
    };
    let main: Vec<&Address> = open.record.to.iter().chain(&open.record.cc).collect();
    let join = |names: Vec<String>| match names.as_slice() {
        [] => String::new(),
        [only] => only.clone(),
        [rest @ .., last] => format!("{} & {last}", rest.join(", ")),
    };
    let names = |list: &[&Address]| -> Vec<String> {
        if list.len() == 1 {
            return vec![names::display_name(
                &person(list[0]),
                is_me(list[0]),
                NameFormat::Full,
                false,
            )];
        }
        list.iter()
            .filter(|address| !is_me(address))
            .map(|address| names::display_name(&person(address), false, NameFormat::First, false))
            .collect()
    };
    let mut text = join(names(&main).into_iter().take(15).collect());
    let bcc: Vec<&Address> = open.record.bcc.iter().collect();
    if !bcc.is_empty() {
        text.push_str(&format!(". Bcc: {}", join(names(&bcc))));
    }
    text
}

/// The From row's dropdown, from the row's top left `at`. It shows the alias's address, with a
/// caret while hovered or focused, and when focused and open, its list of aliases under it.
/// Hovered, it shows the alias in the contact pane.
pub(super) fn from_row(
    ui: &Ui,
    open: &mut Open,
    at: Pos2,
    cx: &Context<'_>,
    id: Id,
    response: &mut Response,
) {
    let ctx = ui.ctx().clone();
    let palette = exo_ui::palette(&ctx);
    let focused = cx.focused && open.state.field == Field::From;
    if !focused {
        // Blur closes the list.
        open.header.from_menu = None;
    }
    // The button: `rgba(0,0,0,.75)`, white in Carbon.
    let color = ink(&ctx, 0.75);
    // A snippet without its own From shows `use default` on the button.
    let email = match &open.record.from {
        Some(from) => from.email.clone(),
        None if open.is_snippet() => "use default".to_owned(),
        None => String::new(),
    };
    // `padding-left: 6px`, and the button's `top: 3px` and `padding-top: 2px`.
    let text = pos2(at.x + LABEL_WIDTH + 6.0, at.y + CHIP_TEXT_Y);
    let text_width = FIELD.paint_line(ui.painter(), text, &email, color, None);
    // The caret (Snow/Carbon): 10px wide, 10px after the address, centred on its line, at half
    // opacity while hovered or focused (`opacity` over 150ms).
    let caret = Rect::from_center_size(
        pos2(
            text.x + text_width + 10.0 + 5.0,
            text.y + FIELD.line_height / 2.0,
        ),
        vec2(10.0, 10.0 * 6.7 / 12.0),
    );
    let button = ui
        .interact(
            Rect::from_min_max(pos2(text.x, at.y + 3.0), pos2(caret.right(), at.y + 25.0)),
            id,
            Sense::click(),
        )
        .on_hover_cursor(egui::CursorIcon::PointingHand);
    let shown = exo_ui::motion::transition(
        &ctx,
        id.with("caret"),
        button.hovered() || focused,
        0.15,
        exo_ui::motion::CubicBezier::EASE,
    );
    if shown > 0.0 {
        exo_ui::paint_icon(
            ui,
            "legacy/DEPRECATED_downCaret",
            caret,
            ink(&ctx, 1.0).gamma_multiply(0.5 * shown),
        );
    }
    // Actions go to the draft with the keyboard.
    if button.hovered() && cx.focused {
        response.actions.push(Action::HoverFrom);
    }
    if button.clicked() {
        if !focused {
            response.actions.push(Action::Focus(Field::From));
        }
        response.actions.push(Action::FromToggle);
    }
    let Some(highlight) = open.header.from_menu else {
        return;
    };
    // The list, tethered under the button: 18px left of the address, `margin: 7px -1px`.
    let choices: Vec<String> = from_choices(open, cx.aliases)
        .into_iter()
        .map(choice_label)
        .collect();
    let width = choices
        .iter()
        .map(|email| FROM_ITEM.width(&ctx, email))
        .fold(0.0, f32::max)
        + 2.0 * FROM_ITEM_PADDING;
    let list = Rect::from_min_size(
        pos2(text.x - 18.0, at.y + 25.0 + 8.0),
        vec2(
            width,
            choices.len() as f32 * FROM_ITEM.line_height + 2.0 * SUGGESTION_PADDING,
        ),
    );
    let colors = exo_ui::ContactListColors::new(palette, false);
    let picked = exo_ui::AnchoredList::new(
        id.with("list"),
        list,
        exo_ui::AnchoredCard::menu(colors.background, colors.selected),
    )
    .order(egui::Order::Foreground)
    .uniform_padding(SUGGESTION_PADDING)
    .highlighted(Some(highlight))
    .hover_mode(exo_ui::AnchoredHover::Always)
    .show_uniform(
        &ctx,
        choices.len(),
        FROM_ITEM.line_height,
        |painter, row, index| {
            FROM_ITEM.paint_line(
                painter,
                pos2(row.left() + FROM_ITEM_PADDING, row.top()),
                &choices[index],
                palette.ink(0.7, 0.7),
                None,
            );
        },
    );
    if picked.contains_pointer && ctx.input(|input| input.pointer.any_pressed()) {
        response.clicked_inside = true;
    }
    if let Some(index) = picked.hovered.filter(|&index| index != highlight) {
        response
            .actions
            .push(Action::FromMove(index as isize - highlight as isize));
    }
    if let Some(index) = picked.clicked {
        response.actions.push(Action::FromChoose(Some(index)));
    }
}

/// The From list's items: the active aliases, and for a snippet `"use default"` (`None`). The
/// selected item comes first and the rest sort by address.
pub(in crate::compose) fn from_choices<'a>(
    open: &Open,
    aliases: &'a [Alias],
) -> Vec<Option<&'a Alias>> {
    let selected = open
        .record
        .from
        .as_ref()
        .map_or(USE_DEFAULT, |from| from.email.as_str());
    let mut choices: Vec<Option<&Alias>> = aliases
        .iter()
        .filter(|alias| alias.active)
        .map(Some)
        .collect();
    if open.is_snippet() {
        choices.push(None);
    }
    let email = |choice: &Option<&Alias>| {
        choice
            .map_or(USE_DEFAULT, |alias| alias.email.as_str())
            .to_owned()
    };
    choices.sort_by_key(|choice| {
        (
            !recipients::same_address(&email(choice), selected),
            email(choice),
        )
    });
    choices
}

/// A From list item's text: the alias's address, or `"use default"` with its quotes.
fn choice_label(choice: Option<&Alias>) -> String {
    choice.map_or_else(|| "\"use default\"".to_owned(), |alias| alias.email.clone())
}

/// The From list's keys and clicks: `FromToggle` opens or closes it, `FromMove` moves its
/// highlight (opening it if it's closed), `FromChoose` takes the highlighted alias (or the one
/// given) and closes it, `FromClose` closes it.
pub(in crate::compose) fn from_menu(open: &mut Open, aliases: &[Alias], action: &Action, now: f64) {
    let choices = from_choices(open, aliases);
    let count = choices.len() as isize;
    if count == 0 {
        return;
    }
    match (action, open.header.from_menu) {
        (Action::FromToggle, Some(_)) | (Action::FromClose, _) => open.header.from_menu = None,
        (Action::FromToggle | Action::FromMove(_), None) => open.header.from_menu = Some(0),
        (Action::FromMove(delta), Some(at)) => {
            open.header.from_menu = Some((at as isize + delta).rem_euclid(count) as usize)
        }
        (Action::FromChoose(picked), Some(at)) => {
            let choice = choices[picked.unwrap_or(at).min(count as usize - 1)].cloned();
            open.header.from_menu = None;
            match choice {
                Some(alias) => choose_from(open, aliases, &alias, now),
                None if open.record.from.is_some() => {
                    open.record.from = None;
                    open.touch(now);
                }
                None => {}
            }
        }
        _ => {}
    }
}

/// The draft is from `alias` now, and a signature already in the body follows the new alias.
pub(in crate::compose) fn choose_from(open: &mut Open, aliases: &[Alias], alias: &Alias, now: f64) {
    if open
        .record
        .from
        .as_ref()
        .is_some_and(|from| from.email == alias.email)
    {
        return;
    }
    let old = open.record.from.replace(Address {
        name: alias.name.clone(),
        email: alias.email.clone(),
    });
    if open.record.quoted_inlined {
        signature::switch(open, aliases, old.as_ref());
    }
    open.touch(now);
}

/// Backspace right after a Unicode replacement in the subject brings back what was typed; any
/// other key forgets the replacement.
fn undo_subject_replacement(ui: &Ui, open: &mut Open, id: Id) {
    let Some(replacement) = open.header.subject_replacement.clone() else {
        return;
    };
    let backspace = ui.input(|input| input.events.iter().any(|event| matches!(event, egui::Event::Key { key: egui::Key::Backspace, pressed: true, modifiers, .. } if modifiers.is_none())));
    let other_key = ui.input(|input| input.events.iter().any(|event| matches!(event, egui::Event::Key { key, pressed: true, .. } if *key != egui::Key::Backspace)));
    if other_key {
        open.header.subject_replacement = None;
        return;
    }
    if !backspace {
        return;
    }
    let caret = egui::text_edit::TextEditState::load(ui.ctx(), id)
        .and_then(|state| state.cursor.char_range());
    open.header.subject_replacement = None;
    if caret.is_none_or(|range| {
        !range.is_empty() || usize::from(range.primary.index) != replacement.caret
    }) {
        return;
    }
    ui.input_mut(|input| input.consume_key(egui::Modifiers::NONE, egui::Key::Backspace));
    open.record.subject = replacement.typed;
    let at = egui::text::CCursor::new(replacement.typed_caret);
    exo_ui::focus_field(ui.ctx(), id, egui::text::CCursorRange::one(at));
}

/// When the last edit typed one of `>-—=~<.` and the text before the caret ends in a pattern
/// (`->`, `...`), the pattern becomes its character (`→`, `…`), as in the body.
fn replace_in_subject(ui: &Ui, open: &mut Open, id: Id, before: &str) {
    let Some(caret) = egui::text_edit::TextEditState::load(ui.ctx(), id)
        .and_then(|state| state.cursor.char_range())
        .filter(|range| range.is_empty())
        .map(|range| usize::from(range.primary.index))
    else {
        return;
    };
    let chars: Vec<char> = open.record.subject.chars().collect();
    if chars.len() != before.chars().count() + 1 || caret == 0 || caret > chars.len() {
        return;
    }
    let Some((len, replacement)) =
        exo_editor::unicode_replacement(chars[caret - 1], &chars[..caret])
    else {
        return;
    };
    let start = caret - len;
    let replaced: String = chars[..start]
        .iter()
        .copied()
        .chain(replacement.chars())
        .chain(chars[caret..].iter().copied())
        .collect();
    let after = start + replacement.chars().count();
    open.header.subject_replacement = Some(SubjectReplacement {
        caret: after,
        typed: std::mem::replace(&mut open.record.subject, replaced),
        typed_caret: caret,
    });
    exo_ui::focus_field(
        ui.ctx(),
        id,
        egui::text::CCursorRange::one(egui::text::CCursor::new(after)),
    );
    ui.ctx().request_repaint();
}

/// Laid-out read-only `To` / `Cc` / `Bcc` rows and `14px/20px` bold subject line, shared with the
/// read-only snippet preview card (`crates/exo/src/snippets/view.rs`).
pub(crate) struct ReadOnlyHeader {
    rows: Vec<(&'static str, Vec<Contact>, f32)>,
    rows_top: f32,
    subject_top: Option<f32>,
    body_top: f32,
}

impl ReadOnlyHeader {
    /// Measures non-empty `To`, `Cc`, and `Bcc` rows and the optional subject line within `width`.
    pub(crate) fn layout(
        ui: &Ui,
        message: &MessageDetail,
        width: f32,
        rows_top: f32,
        empty_top: f32,
        subject_spacing: f32,
        body_margin_top: f32,
    ) -> Self {
        let painter = ui.painter();
        let ink = Color32::BLACK;
        let rows: Vec<(&'static str, Vec<Contact>, f32)> = [
            ("To", &message.to),
            ("Cc", &message.cc),
            ("Bcc", &message.bcc),
        ]
        .into_iter()
        .filter(|(_, people)| !people.is_empty())
        .map(|(label, people)| {
            let galley = painter.layout_job(crate::thread::header_job(people, width, ink, ink));
            (label, people.clone(), galley.rows.len().max(1) as f32)
        })
        .collect();
        let has_rows = !rows.is_empty();
        let mut top = if has_rows {
            rows_top
                + rows
                    .iter()
                    .map(|(_, _, lines)| lines * crate::thread::HEADER_ROW)
                    .sum::<f32>()
        } else {
            empty_top
        };
        let subject_top = (!message.subject.is_empty()).then(|| {
            let box_top = top + if has_rows { subject_spacing } else { 0.0 };
            top = box_top + FIELD.line_height;
            box_top
        });
        let body_top = if has_rows || subject_top.is_some() {
            top + body_margin_top
        } else {
            top
        };
        Self {
            rows,
            rows_top,
            subject_top,
            body_top,
        }
    }

    /// Vertical offset where the body begins below the header and subject.
    pub(crate) fn body_top(&self) -> f32 {
        self.body_top
    }

    /// Paints the recipient rows (`rgba(0,0,0,.5)` labels, `.8` names) and the bold subject line.
    pub(crate) fn paint(
        &self,
        painter: &egui::Painter,
        palette: &exo_ui::Palette,
        origin: Pos2,
        width: f32,
        subject: &str,
    ) {
        let (label, contact) = (palette.ink(0.5, 0.5), palette.ink(0.8, 0.8));
        let mut row_top = origin.y + self.rows_top;
        for (row, people, lines) in &self.rows {
            crate::thread::HEADER_TEXT.paint_line(
                painter,
                pos2(origin.x, row_top),
                row,
                label,
                None,
            );
            let galley =
                painter.layout_job(crate::thread::header_job(people, width, label, contact));
            exo_ui::paint_text(
                painter,
                pos2(origin.x, row_top),
                galley,
                &crate::thread::HEADER_TEXT,
            );
            row_top += lines * crate::thread::HEADER_ROW;
        }
        if let Some(subject_top) = self.subject_top {
            FIELD.paint_line(
                painter,
                pos2(origin.x, origin.y + subject_top + 2.0),
                subject,
                palette.ink(0.8, 0.8),
                Some(width),
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{
        super::test_support::{aliases, reply},
        *,
    };

    #[test]
    fn the_from_list_puts_the_selected_alias_first() {
        let mut open = reply(&[]);
        open.record.from = Some(Address {
            name: None,
            email: "hello@mayaokafor.com".into(),
        });
        let aliases = aliases(3);
        let choices: Vec<String> = from_choices(&open, &aliases)
            .into_iter()
            .map(choice_label)
            .collect();
        assert_eq!(
            choices,
            [
                "hello@mayaokafor.com",
                "a@brightmoor.co",
                "maya@brightmoor.co"
            ]
        );
        from_menu(&mut open, &aliases, &Action::FromToggle, 0.0);
        from_menu(&mut open, &aliases, &Action::FromMove(-1), 0.0);
        assert_eq!(open.header.from_menu, Some(2), "↑ wraps");
        from_menu(&mut open, &aliases, &Action::FromChoose(None), 0.0);
        assert_eq!(
            (
                open.header.from_menu,
                open.record.from.map(|from| from.email)
            ),
            (None, Some("maya@brightmoor.co".to_owned()))
        );
    }
}

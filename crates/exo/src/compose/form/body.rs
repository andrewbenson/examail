//! Body editor host, scroll area and caret reveal, `…` quote/signature toggle, and inline
//! `:emoji` / `;snippet` / `+mention` / Share Availability trigger popups.

use egui::{Color32, CornerRadius, Id, Pos2, Rect, Sense, Ui, UiBuilder, pos2, vec2};
use exo_editor::EditorStyle;
use exo_store::Alias;
use exo_ui::{Family, Font, TextStyle, Theme};

use super::{
    BodyScroll, Context, Layout, Response, SCROLL_LEFT, SCROLLBAR_RIGHT, Scrolled, Surface,
    focus_if, footer, ink,
};
use crate::compose::{
    Action, assistant, availability,
    draft::{Field, Open},
    recipients, signature,
};

/// The `…` toggle: 14px in a 16px line, in `sans-serif` (`span::before`). That font's taller
/// line box puts the toggle 2px lower than Exo Sans's would.
const QUOTING: TextStyle = TextStyle::new(Font::new(Family::SansSerif, 400), 14.0, 16.0);
/// A `;` picker row's text: 13px in 28px lines.
const SNIPPET_ROW: TextStyle = TextStyle::new(Font::new(Family::Sans, 400), 13.0, 28.0);

/// Draws the scrolled body area (`exo-editor`, inline trigger popups, `…` quote reveal button,
/// attachment cards strip, and body scrollbar).
#[expect(
    clippy::too_many_arguments,
    reason = "hosts the body editor, inline popups, quote toggle, and attachment strip in the shared scroll area"
)]
pub(super) fn show(
    ui: &mut Ui,
    open: &mut Open,
    form: Rect,
    layout: &Layout,
    origin: Pos2,
    width: f32,
    base: Id,
    cx: &Context<'_>,
    response: &mut Response,
    now: f64,
) {
    let ctx = ui.ctx().clone();
    let viewport = match layout.scrolled {
        Some(scrolled) => Some(scroll_body(ui, open, layout, scrolled, origin, width)),
        // With room enough there's nothing to scroll (`scrollTop` goes back to 0).
        None => {
            open.body.scroll = BodyScroll::default();
            None
        }
    };
    let scroll = if viewport.is_some() {
        open.body.scroll.offset
    } else {
        0.0
    };
    let mut scrolled_ui = ui.new_child(
        UiBuilder::new()
            .max_rect(form)
            .id_salt(("compose-scrolled", &open.id)),
    );
    if let Some(viewport) = viewport {
        scrolled_ui.set_clip_rect(viewport.intersect(ui.clip_rect()));
    }
    let body = Rect::from_min_size(
        pos2(origin.x, origin.y + layout.body_top - scroll),
        vec2(width, layout.body_height),
    );
    let placeholder = placeholder(open, cx);
    let mut body_ui = scrolled_ui.new_child(
        UiBuilder::new()
            .max_rect(body)
            .id_salt(("compose-body", &open.id)),
    );
    if cx.focused
        && open.state.field == Field::Body
        && ui.input(|input| {
            input.key_pressed(egui::Key::Backspace)
                || input.modifiers.command && input.key_pressed(egui::Key::Z)
        })
    {
        assistant::edit_mention(open, cx.book);
    }
    let find = viewport
        .map(|viewport| exo_ui::find::Scope::begin(&ctx, find_scope(&ctx, open), viewport, scroll));
    let edited = open.editor.ui(
        &mut body_ui,
        &EditorStyle {
            placeholder,
            min_height: layout.body_height,
        },
    );
    drop(find);
    if edited.changed {
        open.touch(now);
    }
    if edited.response.clicked() || edited.response.gained_focus() || edited.dropped {
        response.actions.push(Action::Focus(Field::Body));
    }
    if edited.escaped {
        response.actions.push(Action::Escape);
    }
    if edited.past_end && layout.quote_top.is_some() {
        response.actions.push(Action::Focus(Field::QuotingStart));
    }
    response.actions.extend(edited.toolbar.map(Action::Toolbar));
    response
        .actions
        .extend(edited.open_link.clone().map(Action::OpenLink));
    // A date the caret comes onto, or whose text changes under the caret, shows its day.
    if edited.date != open.body.date {
        response.actions.extend(
            edited
                .date
                .as_ref()
                .map(|date| Action::DateActive(date.text.clone())),
        );
        open.body.date = edited.date.clone();
    }
    open.caret = edited.caret;
    if viewport.is_some() {
        follow_caret(
            ui,
            open,
            edited.response.has_focus().then_some(edited.caret),
            edited.changed,
            body.min,
        );
    }
    if edited.context_menu.is_some() {
        response.actions.push(Action::Focus(Field::Body));
    }
    crate::spelling::note_editor(
        &ctx,
        crate::spelling::EditorTarget::Compose(open.id.clone()),
        &edited,
    );
    focus_if(ui, cx, open.state.field == Field::Body, edited.response.id);

    show_popups(ui, open, &edited, width, base, cx, response);

    // The `…`, which reveals the quote and signature.
    if let Some(top) = layout.quote_top.map(|top| top - scroll) {
        show_quote_toggle(&scrolled_ui, open, origin, top, base, cx, response);
    }

    if let Some((top, card, per_row)) = layout
        .attachments
        .map(|(top, card, per_row)| (top - scroll, card, per_row))
    {
        footer::show_attachments(
            &scrolled_ui,
            open,
            origin,
            top,
            card,
            per_row,
            base,
            response,
        );
    }

    if let Some((viewport, content)) =
        viewport.zip(layout.scrolled.map(|scrolled| scrolled.content))
    {
        open.body.scroll.bar.show(
            ui,
            base.with("scrollbar"),
            viewport.with_max_x(viewport.right() + SCROLLBAR_RIGHT),
            scroll,
            content,
        );
    }
}

/// Updates and draws inline `:emoji`, `+mention`, `;snippet`, and Share Availability popups.
fn show_popups(
    ui: &Ui,
    open: &mut Open,
    edited: &exo_editor::EditorResponse,
    width: f32,
    base: Id,
    cx: &Context<'_>,
    response: &mut Response,
) {
    let ctx = ui.ctx();
    // The assistant belongs to the subject while the subject has the keyboard, else to the body.
    let in_subject = open.state.field == Field::Subject;
    let assistant_trigger = if in_subject {
        open.popups.subject.trigger.clone()
    } else {
        edited.trigger.clone()
    };
    // The `;` picker (the assistant's snippet mode) under the `;`.
    let trigger = assistant_trigger
        .clone()
        .filter(|trigger| trigger.kind == exo_editor::TriggerKind::Snippet);
    if trigger != open.popups.snippets.trigger {
        // Its rows follow next frame.
        ctx.request_repaint();
    }
    open.popups.snippets.trigger = trigger;
    // The `:` emoji and `+`/`@`/`-` contact dropdowns. A mention's recipients follow its undo.
    assistant::note_trigger(open, assistant_trigger.as_ref());
    assistant::undo_marks(open, &edited.undone, &edited.redone);
    if cx.focused
        && let Some(picked) = assistant::show(ui, open, cx.pin, base.with("assistant"))
    {
        response
            .actions
            .push(Action::Assistant(assistant::Key::Pick(Some(picked))));
    }
    if cx.focused
        && let Some(trigger) = &open.popups.snippets.trigger
        && (!open.popups.snippets.rows.is_empty() || open.popups.snippets.hint)
        && open.body.date.is_none()
        && let Some(picked) = snippet_picker(
            ui,
            open,
            trigger.anchor,
            in_subject,
            width,
            base.with("snippet-picker"),
        )
    {
        response.actions.push(Action::SnippetPick(Some(picked)));
    }
    // Share Availability's menu shows where the caret was when the menu opened, and shows its
    // row while what's typed since still matches that row. A press elsewhere closes the menu.
    if cx.focused
        && let Some(menu) = &mut open.popups.availability
    {
        if !availability::follow(&open.editor, menu, edited.changed) {
            response.actions.push(Action::AvailabilityMenu(false));
        } else if menu.shows_row() {
            let anchor = *menu.anchor.get_or_insert(edited.caret);
            let (picked, card) = availability::show(ui, anchor, base.with("availability-menu"));
            if picked {
                response.actions.push(Action::AvailabilityPick);
            } else if ctx.input(|input| input.pointer.any_pressed())
                && !ctx
                    .pointer_interact_pos()
                    .is_some_and(|at| card.contains(at))
            {
                response.actions.push(Action::AvailabilityDismiss);
            }
        }
    }
}

/// Draws the `…` toggle button below the body that reveals the hidden quote and signature.
fn show_quote_toggle(
    ui: &Ui,
    open: &Open,
    origin: Pos2,
    top: f32,
    base: Id,
    cx: &Context<'_>,
    response: &mut Response,
) {
    let ctx = ui.ctx();
    let id = base.with("quoting");
    let hit = Rect::from_min_size(pos2(origin.x - 4.0, origin.y + top), vec2(24.0, 24.0));
    let hovered_or_focused = cx.focused && open.state.field == Field::QuotingStart;
    let toggle = ui
        .interact(hit, id, Sense::click())
        .on_hover_cursor(egui::CursorIcon::PointingHand);
    let shift = exo_ui::motion::transition(
        ctx,
        id.with("hover"),
        toggle.hovered() || hovered_or_focused,
        0.1,
        exo_ui::motion::CubicBezier::EASE,
    );
    if hovered_or_focused {
        // The focus ring: a 22×12 pill, `0 0 6px 1px #aeb1dd`.
        let ring = Rect::from_min_size(
            pos2(
                origin.x + 4.0 * shift - 4.0 + 4.0,
                origin.y + top + 6.0 - 4.0,
            ),
            vec2(22.0, 12.0),
        );
        exo_ui::BoxShadow::new([0.0, 0.0], 6.0, 1.0, Color32::from_rgb(0xae, 0xb1, 0xdd)).paint(
            ui.painter(),
            ring,
            CornerRadius::same(2),
        );
    }
    let color = ink(ctx, 0.75).gamma_multiply(0.4);
    QUOTING.paint_line(
        ui.painter(),
        pos2(origin.x + 4.0 * shift, origin.y + top),
        "…",
        color,
        None,
    );
    if toggle.is_pointer_button_down_on() && toggle.drag_started() || toggle.clicked() {
        response.actions.push(Action::ClickRevealQuote);
    }
}

/// The scroll area on screen, with the form's top left at `origin`. The wheel over the area (or
/// over its track, right of it) scrolls it, and a caret that moved last frame comes into view now
/// that the layout has what moved it. The scroll stays within its range and snaps to device
/// pixels, as Chromium scrolls.
fn scroll_body(
    ui: &Ui,
    open: &mut Open,
    layout: &Layout,
    scrolled: Scrolled,
    origin: Pos2,
    width: f32,
) -> Rect {
    let viewport = Rect::from_min_max(
        pos2(origin.x - SCROLL_LEFT, origin.y + scrolled.top),
        pos2(origin.x + width, origin.y + scrolled.bottom),
    );
    let editor_bottom = layout.body_top + open.body.height;
    let find = exo_ui::find::reveal(ui.ctx(), find_scope(ui.ctx(), open));
    let scroll = &mut open.body.scroll;
    if ui.rect_contains_pointer(viewport.with_max_x(viewport.right() + SCROLLBAR_RIGHT)) {
        scroll.offset -= ui.input(|input| input.smooth_scroll_delta.y);
    }
    // Find scrolls at once to a match out of view.
    scroll.offset += find.map_or(0.0, |delta| delta.y);
    if let Some(caret) = scroll.caret.filter(|_| std::mem::take(&mut scroll.reveal)) {
        scroll.offset += reveal(
            caret.translate(vec2(0.0, layout.body_top)),
            editor_bottom,
            scrolled,
            scroll.offset,
        );
    }
    let pixels = ui.pixels_per_point();
    let most = (scrolled.content - viewport.height()).max(0.0);
    scroll.offset = ((scroll.offset * pixels).round() / pixels).clamp(0.0, most);
    scroll.area = Some((
        viewport.with_max_x(viewport.right() + SCROLLBAR_RIGHT),
        most,
    ));
    viewport
}

/// What find calls the body's scroll area.
fn find_scope(ctx: &egui::Context, open: &Open) -> Id {
    exo_ui::id(ctx, ("compose-body-find", &open.id))
}

/// How far to scroll to show `caret` (relative to the form, unscrolled) in the scroll area
/// scrolled to `offset`. Chromium's `CenterIfNeeded` (`RevealSelection`) brings a caret partly
/// out of view in at its nearest edge, and one wholly out of view to the middle. On the body's
/// last line, what's after the caret (the `…`, the files) comes into view with it, so typing at
/// the end keeps the area scrolled to its bottom.
fn reveal(caret: Rect, editor_bottom: f32, scrolled: Scrolled, offset: f32) -> f32 {
    let last_line = caret.bottom() + caret.height() > editor_bottom;
    let shown = if last_line {
        caret.with_max_y(scrolled.top + scrolled.content)
    } else {
        caret
    };
    let shown = if shown.height() > scrolled.bottom - scrolled.top {
        caret
    } else {
        shown
    };
    let (top, bottom) = (scrolled.top + offset, scrolled.bottom + offset);
    if shown.top() >= top && shown.bottom() <= bottom {
        0.0
    } else if shown.bottom() <= top || shown.top() >= bottom {
        shown.center().y - (top + bottom) / 2.0
    } else if shown.top() < top {
        shown.top() - top
    } else {
        shown.bottom() - bottom
    }
}

/// Notes where the caret is in the body (`body` is the editor's top left on screen) while the
/// editor has the keyboard. A caret that moved, or an edit, brings the caret into view next
/// frame ([`reveal`]). Scrolling with the wheel leaves the caret out of view.
fn follow_caret(ui: &Ui, open: &mut Open, caret: Option<Rect>, changed: bool, body: Pos2) {
    let caret = caret.map(|caret| caret.translate(-body.to_vec2()));
    let scroll = &mut open.body.scroll;
    if caret.is_some() && (changed || scroll.caret != caret) {
        scroll.reveal = true;
        ui.ctx().request_repaint();
    }
    scroll.caret = caret;
}

/// The body's placeholder: `Enter content` for a snippet, else `Say hello`, or `Write a note`
/// for a note to self. Under a message only a forward has one, and popped out only a new email
/// or a forward.
fn placeholder(open: &Open, cx: &Context<'_>) -> Option<String> {
    if open.is_snippet() {
        return Some("Enter content".into());
    }
    let visible = cx.surface == Surface::Pane
        || (cx.surface == Surface::Popout && open.is_new_email())
        || open.record.action == exo_store::DraftAction::Forward;
    if !visible {
        return None;
    }
    let note = open.record.to.len() == 1
        && open.record.to[0].name.as_deref() == Some("Me")
        && recipients::same_address(&open.record.to[0].email, cx.me)
        && open.record.cc.is_empty()
        && open.record.bcc.is_empty();
    Some(if note { "Write a note" } else { "Say hello" }.into())
}

/// The `;` picker's box: the base background, 4px corners and the low elevation's shadow,
/// `0 1px 4px 0 #1414130f`. Returns it pixel-snapped.
fn paint_list_card(painter: &egui::Painter, list: Rect) -> Rect {
    let palette = exo_ui::palette(painter.ctx());
    let snapped = exo_ui::pixel_snap(list);
    exo_ui::BoxShadow::new(
        [0.0, 1.0],
        4.0,
        0.0,
        Color32::from_rgba_unmultiplied(0x14, 0x14, 0x13, 0x0f),
    )
    .paint(painter, snapped, CornerRadius::same(4));
    painter.rect_filled(
        snapped,
        CornerRadius::same(4),
        palette.background_base_default,
    );
    snapped
}

/// The `;` picker under the `;` at `anchor`. It shows up to five rows of a snippet's name, its
/// text and `Me`, with the highlighted row filled, or, with no snippets at all,
/// `To create a Snippet, hit ⌘ K → "Snippets"`. It's at least 400px wide (`min-width`), 14px
/// left of the `;`, padded 7px, with no border (Classic's `border: 0; margin: 0 0 0 2px`). The
/// subject's picker (`in_subject`) hangs from the line's bottom 2px right of the `;` (Classic's
/// margin wins over its `-13px`), no wider than `min(100vw - 460px, 600px)`. Returns the row
/// pressed (mouse-down applies it).
fn snippet_picker(
    ui: &Ui,
    open: &mut Open,
    anchor: Rect,
    in_subject: bool,
    form_width: f32,
    id: Id,
) -> Option<usize> {
    let ctx = ui.ctx().clone();
    let palette = exo_ui::palette(&ctx);
    const PAD: f32 = 17.0;
    const ROW: f32 = 32.0;
    let rows = open.popups.snippets.rows.clone();
    let me = SNIPPET_ROW.width(&ctx, "Me");
    let widest = rows
        .iter()
        .map(|row| {
            SNIPPET_ROW
                .width(&ctx, &row.name)
                .min(275.0f32.max(form_width / 2.0))
                + 2.0 * PAD
                + SNIPPET_ROW.width(&ctx, &row.preview).min(300.0)
                + me
                + PAD
        })
        .fold(0.0, f32::max);
    let count = if rows.is_empty() { 1 } else { rows.len() };
    let list = if in_subject {
        let most = (ctx.content_rect().width() - 460.0).min(600.0);
        Rect::from_min_size(
            pos2(anchor.left() + 2.0, anchor.bottom()),
            vec2(
                widest.clamp(400.0, most.max(400.0)),
                count as f32 * ROW + 14.0,
            ),
        )
    } else {
        let width = widest.clamp(400.0, form_width.max(400.0));
        Rect::from_min_size(
            pos2(anchor.left() - 14.0, anchor.bottom() - 1.0),
            vec2(width - 2.0, count as f32 * ROW + 14.0),
        )
    };
    // The highlighted row's fill, the name's colour and the muted text's colour are literals.
    let (selected, name_color, muted) = match palette.theme {
        // `hsl(220, 45%, 97%)`.
        Theme::Snow => (
            Color32::from_rgb(0xf4, 0xf6, 0xfb),
            palette.ink(0.85, 0.85),
            palette.ink(0.45, 0.45),
        ),
        Theme::Carbon => (
            Color32::from_rgb(0x46, 0x4b, 0x53),
            Color32::WHITE,
            palette.ink(0.45, 0.45),
        ),
    };
    let pointer = ctx.input(|input| {
        (
            input.pointer.hover_pos(),
            input.pointer.primary_pressed(),
            input.pointer.is_moving(),
        )
    });
    let mut picked = None;
    egui::Area::new(id)
        .order(egui::Order::Foreground)
        .fixed_pos(list.min)
        .show(&ctx, |area| {
            let painter = area.painter().clone();
            let _ = paint_list_card(&painter, list);
            let _ = area.allocate_rect(list, Sense::click());
            let top = list.top() + 7.0;
            if rows.is_empty() {
                // The hint, with its keys in inline caps.
                let line = pos2(list.left() + PAD, top + 4.0);
                let lead =
                    SNIPPET_ROW.paint_line(&painter, line, "To create a Snippet, hit", muted, None);
                let caps = exo_ui::Keycaps::new(
                    &ctx,
                    &["⌘", "K"],
                    exo_ui::KeycapColors::shortcuts_pane(palette),
                );
                caps.paint(&painter, line.x + lead, top + 8.0);
                SNIPPET_ROW.paint_line(
                    &painter,
                    pos2(line.x + lead + caps.width() + 4.0, line.y),
                    "→ \"Snippets\"",
                    muted,
                    None,
                );
                return;
            }
            for (index, row) in rows.iter().enumerate() {
                let rect = Rect::from_min_size(
                    pos2(list.left(), top + index as f32 * ROW),
                    vec2(list.width(), ROW),
                );
                if index == open.popups.snippets.pick {
                    painter.rect_filled(exo_ui::pixel_snap(rect), 0.0, selected);
                }
                let line = rect.top() + 4.0;
                let name_width = SNIPPET_ROW.paint_line(
                    &painter,
                    pos2(rect.left() + PAD, line),
                    &row.name,
                    name_color,
                    Some(275.0f32.max(rect.width() / 2.0)),
                );
                let me_left = rect.right() - PAD - me;
                let preview_left = rect.left() + PAD + name_width + PAD;
                SNIPPET_ROW.paint_line(
                    &painter,
                    pos2(preview_left, line),
                    &row.preview,
                    muted,
                    Some((me_left - PAD - preview_left).clamp(0.0, 300.0)),
                );
                SNIPPET_ROW.paint_line(&painter, pos2(me_left, line), "Me", muted, None);
                match pointer {
                    (Some(at), pressed, moving) if rect.contains(at) => {
                        if moving && index != open.popups.snippets.pick {
                            open.popups.snippets.pick = index;
                        }
                        if pressed {
                            picked = Some(index);
                        }
                    }
                    _ => {}
                }
            }
        });
    picked
}

/// Puts the signature and quote into the body, as the `…` does.
pub(in crate::compose) fn reveal_quote(open: &mut Open, aliases: &[Alias], now: f64) {
    if open.record.quoted_inlined {
        return;
    }
    let quote = open.record.quoted_content.clone();
    // A draft from another app has its own signature, with its quote.
    let signature = (!open.record.quote_signed).then(|| signature::revealed(open, aliases));
    open.editor
        .reveal_quoted(signature.as_deref(), quote.as_deref());
    open.record.quoted_inlined = true;
    open.state.field = Field::Body;
    open.touch(now);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_moved_caret_scrolls_in_as_chromium_reveals_it() {
        let scrolled = Scrolled {
            top: 0.0,
            bottom: 200.0,
            content: 400.0,
        };
        let caret = |top: f32| Rect::from_min_size(pos2(0.0, top), vec2(1.0, 18.0));
        assert_eq!(reveal(caret(40.0), 500.0, scrolled, 0.0), 0.0, "in view");
        assert_eq!(
            reveal(caret(190.0), 500.0, scrolled, 0.0),
            8.0,
            "partly below: to the edge"
        );
        assert_eq!(
            reveal(caret(300.0), 500.0, scrolled, 0.0),
            209.0,
            "out of view: to the middle"
        );
        assert_eq!(
            reveal(caret(280.0), 300.0, scrolled, 150.0),
            50.0,
            "on the last line, with what's after it"
        );
    }
}

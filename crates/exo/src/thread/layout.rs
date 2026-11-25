//! Pane and card geometry for the conversation view, new-message cards, and popped-out drafts.

use egui::{Rect, Sense, Ui, UiBuilder, pos2, vec2};
use exo_store::Attachment;

use super::{
    Action, CARD_MARGIN_TOP, MESSAGES_MARGIN_TOP, MESSAGES_PADDING_TOP, SANDBOX_MAX_WIDTH,
    collapse, footer, header, message, wide,
};
use crate::body;

/// What the new-message pane holds: its title, the form's height, and the room kept above the
/// card (a snippet's name sits there, 36px).
pub(crate) struct PaneCard<'a> {
    pub title: &'a str,
    pub height: f32,
    pub above: f32,
    /// Whether the pop-out hint goes under the card, for a new message started in a conversation
    /// and not touched yet.
    pub pop_out_hint: bool,
    /// Whether the card is a read-only snippet, with no form.
    pub read_only: bool,
}

/// The thread pane for a new message: the back gutter, the title (`New Message`) with the
/// conversation arrows, and one card holding the compose form. `form` draws the form into the
/// card's content column (left, top, width), at the height `pane` gives. `form` is also given the
/// message column's extent, which tooltips stay inside, and the card.
pub(crate) fn new_message_pane(
    ui: &mut Ui,
    rect: Rect,
    rem: f32,
    window_width: f32,
    popped_out: bool,
    pane: &PaneCard<'_>,
    form: impl FnOnce(&mut Ui, Rect, egui::Rangef, Rect) -> Vec<crate::compose::Action>,
) -> Vec<Action> {
    let (mut actions, form) = card_pane(ui, rect, rem, window_width, popped_out, pane, form);
    actions.splice(0..0, form.into_iter().map(Action::Compose));
    actions
}

/// [`new_message_pane`] around any card's content: what `content` returns comes back beside the
/// pane's own actions.
pub(crate) fn card_pane<T>(
    ui: &mut Ui,
    rect: Rect,
    rem: f32,
    window_width: f32,
    popped_out: bool,
    pane: &PaneCard<'_>,
    content: impl FnOnce(&mut Ui, Rect, egui::Rangef, Rect) -> T,
) -> (Vec<Action>, T) {
    let height = pane.height;
    let palette = exo_ui::palette(ui.ctx());
    ui.painter()
        .rect_filled(rect, 0.0, palette.background_base_default);
    let layout = Layout::new(rect, rem, window_width, popped_out);
    let mut actions = Vec::new();
    let gutter = ui
        .interact(
            layout.gutter_target(),
            ui.id().with("thread-back-gutter"),
            Sense::click(),
        )
        .on_hover_cursor(egui::CursorIcon::PointingHand);
    let hovered = gutter.hovered() || crate::shell::back_area_hovered(ui.ctx());
    let compact_clicked = header::back_button(ui, &layout, hovered, false);
    if gutter.clicked() || compact_clicked {
        actions.push(Action::Back);
    }
    let messages_top = layout.header.top() + header::title_height() + MESSAGES_MARGIN_TOP;
    let card = Rect::from_min_size(
        pos2(
            layout.card.left(),
            messages_top + MESSAGES_PADDING_TOP + CARD_MARGIN_TOP + pane.above,
        ),
        vec2(
            layout.card.width(),
            message::PADDING_TOP + height + message::PADDING_BOTTOM,
        ),
    );
    let clip = Rect::from_min_max(
        pos2(layout.column.left(), messages_top),
        pos2(layout.column.right(), rect.bottom()),
    );
    let mut list = ui.new_child(UiBuilder::new().max_rect(clip));
    list.set_clip_rect(clip.intersect(ui.clip_rect()));
    message::card(
        &list,
        card,
        &layout,
        if pane.read_only {
            message::CardKind::Expanded
        } else {
            message::CardKind::Composing
        },
        false,
    );
    if pane.pop_out_hint {
        footer::paint_pop_out(
            &list,
            &layout,
            card.bottom()
                + collapse(
                    message::EXPANDED_MARGIN_BOTTOM,
                    footer::MARGIN_TOP_UNDER_DRAFT,
                ),
        );
    }
    let content_rect = Rect::from_min_size(
        pos2(layout.inner.left(), card.top() + message::PADDING_TOP),
        vec2(layout.inner.width(), height),
    );
    let shown = content(&mut list, content_rect, layout.column.x_range(), card);
    actions.extend(header::show_title(ui, &layout, pane.title));
    (actions, shown)
}

/// The width of a message card's content column, which a new message's form fills.
pub(crate) fn content_width(rect: Rect, rem: f32, window_width: f32, popped_out: bool) -> f32 {
    Layout::new(rect, rem, window_width, popped_out)
        .inner
        .width()
}

/// The left edge of a message card's content column.
pub(crate) fn content_left(rect: Rect, rem: f32, window_width: f32, popped_out: bool) -> f32 {
    Layout::new(rect, rem, window_width, popped_out)
        .inner
        .left()
}

/// Where a popped-out draft goes beside the thread: the left edge and width of its column, in a
/// content pane `rect`.
pub(crate) fn popout_column(rect: Rect, rem: f32, window_width: f32) -> (f32, f32) {
    let popout = Popout::new(
        container(rect, rem, window_width, true),
        rect.left(),
        window_width,
        rem,
    );
    (popout.left, popout.width)
}

/// How far the pop-out slides left, out of the contact pane's way, while the pointer rests over
/// that pane. In a content pane `rect` too narrow for both columns, it slides until the pop-out
/// ends where the thread's does. Zero when they fit.
pub(crate) fn popout_slide(rect: Rect, rem: f32, window_width: f32) -> f32 {
    let container = container(rect, rem, window_width, true);
    let columns = container.width();
    if columns >= 2.0 * (500.0 + 7.0 * rem) - 2.0 * rem {
        return 0.0;
    }
    let popout = Popout::new(container, rect.left(), window_width, rem);
    // `Popout` has added a rem to the width and taken it off the left.
    rect.left() + columns - (popout.left + popout.width)
}

/// A popped-out draft's columns, for the thread's container `container`, with the left
/// rail and pane `rail` wide in a window `window_width` wide.
struct Popout {
    /// Both columns' width.
    width: f32,
    /// How far the thread's column moves right.
    gutter: f32,
    /// The pop-out column's left edge.
    left: f32,
}

impl Popout {
    fn new(container: Rect, rail: f32, window_width: f32, rem: f32) -> Self {
        let columns = container.width();
        let widest = SANDBOX_MAX_WIDTH + 7.0 * rem;
        let narrowest = 500.0 + 7.0 * rem;
        let (mut width, mut left, mut gutter) = (widest, rail + columns / 2.0, 0.0);
        if columns > 2.0 * widest {
            gutter = (columns - 2.0 * widest) / 2.0;
        } else if columns > 2.0 * narrowest {
            width = columns / 2.0;
        } else {
            width = (columns - rem).min(narrowest);
            left = (rail + width).min(window_width - width);
        }
        Self {
            width: width + rem,
            gutter,
            left: left - rem,
        }
    }
}

/// The thread's container in a content pane `rect`.
pub(super) fn container(rect: Rect, rem: f32, window_width: f32, popped_out: bool) -> Rect {
    container_and_gutter(rect, rem, window_width, popped_out).0
}

/// The thread's container and the back gutter beside it in a content pane `rect`, and the list
/// layout's left pane width. The gutter is `flex: 5.25 0 0; max-width: clamp(300px, 416px, …)`
/// and the thread `flex: 1 1 calc(640px + 8rem)` with `max-width: calc(100% - 4rem)`, centred.
/// Spare room grows both, and whatever the thread can't take goes to the gutter. Beside a
/// popped-out draft the thread's basis is `calc(2 * (640px + 7rem))` and it may take the whole
/// pane.
fn container_and_gutter(
    rect: Rect,
    rem: f32,
    window_width: f32,
    popped_out: bool,
) -> (Rect, Rect, f32) {
    let (basis, thread_max) = if popped_out {
        (2.0 * (SANDBOX_MAX_WIDTH + 7.0 * rem), rect.width())
    } else {
        (SANDBOX_MAX_WIDTH + 8.0 * rem, rect.width() - 4.0 * rem)
    };
    // The list layout's left pane: `clamp(300px, 416px, 23.61vw + 33*(1440px - 100vw)/480)`.
    let left_pane =
        (0.2361 * window_width + 33.0 * (1440.0 - window_width) / 480.0).clamp(300.0, 416.0);
    let free = rect.width() - basis;
    let (mut action, mut thread) = if free >= 0.0 {
        (free * 5.25 / 6.25, basis + free / 6.25)
    } else {
        (0.0, basis + free)
    };
    if thread > thread_max {
        action += thread - thread_max;
        thread = thread_max;
    }
    if action > left_pane {
        thread = (thread + action - left_pane).min(thread_max);
        action = left_pane;
    }
    let spare = (rect.width() - action - thread).max(0.0);
    let action_pane = Rect::from_min_size(
        pos2(rect.left() + spare / 2.0, rect.top()),
        vec2(action, rect.height()),
    );
    let container = Rect::from_min_size(
        pos2(action_pane.right(), rect.top()),
        vec2(thread, rect.height()),
    );
    (container, action_pane, left_pane)
}

/// The widest window whose thread keeps 4px more room at the right, and the room.
const NARROW_WINDOW: f32 = 1024.0;
const NARROW_RIGHT: f32 = 4.0;

/// Where everything in the thread view goes for one window size.
#[derive(Clone, Debug)]
pub(crate) struct Layout {
    pub(super) rem: f32,
    pub(super) popped_out: bool,
    /// The window's width, which some rules follow (`@media (max-width: …)`).
    pub(super) window_width: f32,
    /// The list layout's left pane width, which the back gutter's hover gradient spans 75% of.
    pub(super) left_pane: f32,
    /// The left back gutter.
    pub(super) action_pane: Rect,
    /// The header, full height.
    pub(super) header: Rect,
    /// The subject line's box, inside the header's padding.
    pub(super) label: Rect,
    /// The message column.
    pub(super) column: Rect,
    /// A message card's horizontal extent.
    pub(super) card: Rect,
    /// A message's content inside the card's padding (the body's column).
    pub(super) inner: Rect,
    /// The column widened for a wide body, else `None`.
    pub(super) wide: Option<wide::Column>,
}

impl Layout {
    pub(super) fn new(rect: Rect, rem: f32, window_width: f32, popped_out: bool) -> Self {
        let (container, action_pane, left_pane) =
            container_and_gutter(rect, rem, window_width, popped_out);
        let basis = SANDBOX_MAX_WIDTH + 8.0 * rem;
        // The header and the messages have `max-width: calc(640px + 8rem)`, centred. Beside a
        // popped-out draft (`align-items: flex-start`) the header is up to twice as wide and the
        // messages take the pop-out's width, both at the left.
        let (header, column) = if popped_out {
            let header_width = container.width().min(2.0 * (SANDBOX_MAX_WIDTH + 7.0 * rem));
            let popout = Popout::new(container, rect.left(), window_width, rem);
            let header = Rect::from_min_size(container.min, vec2(header_width, rect.height()));
            (
                header,
                Rect::from_min_size(
                    pos2(container.left() + popout.gutter, rect.top()),
                    vec2(popout.width, rect.height()),
                ),
            )
        } else {
            let column_width = container.width().min(basis);
            let column_left = container.left() + (container.width() - column_width) / 2.0;
            let column = Rect::from_min_size(
                pos2(column_left, rect.top()),
                vec2(column_width, rect.height()),
            );
            (column, column)
        };
        // In a window up to 1024px wide, the header's padding and the messages' margin are 4px
        // wider at the right (`calc(4rem + 4px)`, `calc(2rem + 4px)`).
        let narrow = if window_width <= NARROW_WINDOW {
            NARROW_RIGHT
        } else {
            0.0
        };
        let label = Rect::from_min_size(
            pos2(
                header.left() + 4.0 * rem,
                header.top() + header::PADDING_TOP,
            ),
            vec2(header.width() - 8.0 * rem - narrow, header::SUBJECT_LINE),
        );
        let card = Rect::from_min_size(
            pos2(column.left() + 2.0 * rem, rect.top()),
            vec2(column.width() - 4.0 * rem - narrow, 0.0),
        );
        let inner = Rect::from_min_size(
            pos2(card.left() + 2.0 * rem, rect.top()),
            vec2(card.width() - 4.0 * rem, 0.0),
        );
        Self {
            rem,
            popped_out,
            window_width,
            left_pane,
            action_pane,
            header,
            label,
            column,
            card,
            inner,
            wide: None,
        }
    }

    pub(super) fn rem(&self) -> f32 {
        self.rem
    }

    pub(super) fn window_width(&self) -> f32 {
        self.window_width
    }

    /// Whether the pane is beside a popped-out draft.
    pub(super) fn popped_out(&self) -> bool {
        self.popped_out
    }

    /// How a body in this layout is shown.
    pub(super) fn look(
        &self,
        scale: f32,
        theme: exo_ui::Theme,
        rem: f32,
        show_quoted: bool,
        marketing: bool,
        hide_original: bool,
    ) -> body::Look {
        let palette = theme.palette();
        // A marketing card in Carbon is white.
        let card = if marketing && theme == exo_ui::Theme::Carbon {
            egui::Color32::WHITE
        } else {
            palette.background_elevated_default
        };
        body::Look {
            width: self.inner.width(),
            scale,
            theme,
            rem,
            show_quoted,
            hide_original,
            phase: self.inner.left() - self.inner.left().floor(),
            background: card.to_array(),
        }
    }
}

/// What a click on `attachment`, one of `among`, asks for. With `preview`, an image or a message
/// opens Exo's viewer over the images and messages among them. Anything else gets Quick Look or a
/// download.
pub(super) fn open_file<'a>(
    among: impl Iterator<Item = &'a Attachment>,
    attachment: Attachment,
    preview: bool,
) -> Action {
    if !(preview && crate::attachment_files::is_previewable(&attachment)) {
        return Action::Download {
            attachment,
            open: preview,
        };
    }
    let (attachments, start) = crate::attachment_files::preview_selection(among, &attachment);
    Action::Preview { attachments, start }
}

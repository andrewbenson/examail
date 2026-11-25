//! The open conversation: the back gutter, the header with the subject, labels and actions, and
//! the messages, collapsed or expanded, with their bodies from [`crate::body`].
//!
//! Keys aren't bound here. The operations they trigger are methods ([`ThreadView::next_message`],
//! [`ThreadView::toggle_current`], …) for the app's commands to call.

pub(crate) mod attachments;
pub(crate) mod dates;
mod focus;
mod footer;
mod header;
mod highlight;
pub(crate) mod invite;
mod invites;
mod layout;
mod markers;
mod message;
mod navigation;
mod new_messages;
mod overflow;
mod print;
mod scroll;
mod selection;
mod tooltips;
mod wide;

pub(crate) use focus::{GutterKey, HeaderKey};
pub(crate) use layout::{
    Layout, PaneCard, card_pane, content_left, content_width, new_message_pane, popout_column,
    popout_slide,
};
pub(crate) use message::{
    HEADER as HEADER_TEXT, HEADER_ROW, PADDING_BOTTOM as CARD_PADDING_BOTTOM,
    PADDING_TOP as CARD_PADDING_TOP, append_contacts, contact_title, header_job, person_of,
};

use std::{
    collections::{HashMap, HashSet},
    path::PathBuf,
    sync::Arc,
};

use egui::{Rect, Sense, Ui, UiBuilder, pos2, vec2};
use exo_commands::Region;
use exo_store::{Attachment, MessageDetail, ThreadDetail};
use jiff::Zoned;

use self::{
    invites::InviteState,
    layout::{container, open_file},
    navigation::{ActionState, HoverState},
    scroll::ScrollState,
};
use crate::{
    body::{Bodies, Event, Hit},
    format::names::Person,
};

/// `SANDBOX_MAX_WIDTH`: the message column's widest content.
pub(super) const SANDBOX_MAX_WIDTH: f32 = 640.0;
/// The messages' `padding-top: 28px; margin-top: -20px`.
pub(super) const MESSAGES_PADDING_TOP: f32 = 28.0;
pub(super) const MESSAGES_MARGIN_TOP: f32 = -20.0;
/// Each card's `margin-top: -12px`, collapsed or expanded.
pub(super) const CARD_MARGIN_TOP: f32 = -12.0;
/// 44px (11 × 4px): a collapsed message's pitch, and the positioner's step.
pub(super) const STEP: f32 = 44.0;

/// What a selection in the header's text was in: the subject, or a full header's addresses.
pub(super) const SELECTED_SUBJECT: u32 = 1;
pub(super) const SELECTED_CONTACTS: u32 = 2;

/// What the app should do in response to the user.
#[derive(Clone, Debug, PartialEq)]
pub(crate) enum Action {
    /// The back button, or Escape with nothing to collapse.
    Back,
    /// The header's up/down buttons: the previous (-1) or next (1) conversation.
    Navigate(isize),
    /// The header's Done button.
    ToggleDone,
    /// The header's ⌘K button.
    CommandPalette,
    /// The header's Remind Me button.
    Remind,
    /// A label chip, which shows that label's list.
    ShowLabel(String),
    /// Open a URL in the system browser (a link in a body, the Unsubscribe link).
    OpenUrl(String),
    /// Open a web link without bringing the browser to the front (a ⌘-click on a link).
    OpenUrlInBackground(String),
    /// The Show New Messages bar.
    ShowNewMessages,
    /// The third click in a row on headers that expanded (or collapsed) a message. Suggest ⇧O.
    ExpandAllHint { expand: bool },
    /// The pointer settled on a participant or a `mailto:` link. Show them in the right pane.
    ShowContact(Person),
    /// Text was selected in the subject or a full header's addresses. Suggest ⌘K's copy row.
    CopyHint { subject: bool },
    /// A participant's address in their tooltip, which starts a new message to them, popped out
    /// when ⇧ was held.
    ComposeTo(Person, bool),
    /// Something a copy tooltip put on the clipboard, for its `"…" copied to clipboard.`.
    Copied(String),
    /// The header's Unsubscribe.
    Unsubscribe,
    /// An attachment card, to preview in Quick Look, or only to download when ⌘ or Ctrl was held
    /// or the click was on its download icon.
    Download { attachment: Attachment, open: bool },
    /// An image or message card, or an image in a body, to open in Exo's viewer over
    /// `attachments` from `start`.
    Preview {
        attachments: Vec<Attachment>,
        start: usize,
    },
    /// `Download all` under message `message`'s cards.
    DownloadAll { message: String },
    /// A card dragged out of the window, from `at`.
    DragFile {
        attachment: Attachment,
        at: egui::Pos2,
    },
    /// Something for compose: a reply button, or a draft's own action.
    Compose(crate::compose::Action),
    /// A header chevron expanded (or folded) a message's header. Suggest Shift-H.
    HeaderHint { expanded: bool },
    /// A click on a body's `…` showed (or hid) its quoted text. Suggest O.
    QuoteHint { shown: bool },
    /// The draft being typed in scrolled out of view. Suggest popping it out.
    PopOutHint,
    /// An invite card's RSVP: the invite, my answer recorded, and the answer.
    RespondToInvite(invite::Invite, invite::Response),
    /// The pointer settled on an invite's When or a detected date, to show that day in the right
    /// pane.
    ShowDay(jiff::civil::Date),
    /// An invite's Where that's a phone number, to call (`tel:` and digits, which the card builds
    /// itself).
    Dial(String),
    /// A click on a detected date, for a new event with the conversation's people and subject and
    /// the form's day set to that date.
    CreateEvent(jiff::civil::Date),
}

/// One message as the thread shows it.
#[derive(Clone, Debug)]
pub(super) struct Item {
    pub(super) id: String,
    pub(super) expanded: bool,
    pub(super) header_expanded: bool,
    pub(super) show_quoted: bool,
    /// In Trash while the view doesn't show Trash. Collapsed, the message is the
    /// `This message was moved to Trash.` marker, which the cursor skips.
    pub(super) trashed: bool,
}

/// One thing in the messages column.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Entry {
    /// Message `items[at]`, a card or a collapsed row.
    Message(usize),
    /// Message `items[at]`, in Trash and folded into its marker.
    Trashed(usize),
    /// `Reminder returned`.
    Reminder,
}

/// An entry where the last layout put it: its top in the column's content, its height (a
/// message's with its footer hint), and its card's height.
#[derive(Clone, Copy, Debug)]
pub(super) struct Placed {
    pub(super) entry: Entry,
    pub(super) top: f32,
    pub(super) height: f32,
    pub(super) card: f32,
}

/// The gap two adjoining vertical margins leave (CSS margin collapsing): the largest positive
/// one less the most negative one.
pub(super) fn collapse(a: f32, b: f32) -> f32 {
    a.max(b).max(0.0) + a.min(b).min(0.0)
}

/// The open thread's state.
pub struct ThreadView {
    id: String,
    me: String,
    detail: Option<Arc<ThreadDetail>>,
    items: Vec<Item>,
    /// The focused message.
    focus: Option<String>,
    /// What the attachment cards show besides the messages.
    files: attachments::Files,

    /// Grouped scroll, wide-column, viewport, placement, and highlight state.
    scroll_state: ScrollState,
    /// Grouped invite-card fetch, parsed invites, focus, photo, and calendar-account state.
    invite_state: InviteState,
    /// Grouped body pointer/selection, hover-intent, and header participant tooltip state.
    hover: HoverState,
    /// Grouped triage, keyboard focus zone, header, environment, and arrival state.
    action: ActionState,
}

impl ThreadView {
    /// A view of thread `id` in account `me`, dated relative to `now`, with the invite photos in
    /// `images`. It shows the thread once [`Self::set_detail`] has it.
    pub fn new(id: String, now: Zoned, me: String, images: PathBuf) -> Self {
        Self {
            id,
            me,
            detail: None,
            items: Vec::new(),
            focus: None,
            files: attachments::Files::default(),
            scroll_state: ScrollState::default(),
            invite_state: InviteState::new(images),
            hover: HoverState::default(),
            action: ActionState::new(now),
        }
    }

    /// Sets the header's label chips.
    pub fn set_chips(&mut self, chips: Vec<String>) {
        self.action.chips = chips;
    }

    /// The thread's id.
    pub fn id(&self) -> &str {
        &self.id
    }

    /// The focused message's id.
    pub fn focused(&self) -> Option<&str> {
        self.focus.as_deref()
    }

    /// Whether each message is expanded, oldest first.
    pub fn expanded(&self) -> Vec<bool> {
        self.items.iter().map(|item| item.expanded).collect()
    }

    /// Whether the focused message shows its full header.
    pub fn header_expanded(&self) -> bool {
        self.focus_index()
            .is_some_and(|at| self.items[at].header_expanded)
    }

    /// The thread, once loaded.
    pub fn detail(&self) -> Option<&ThreadDetail> {
        self.detail.as_deref()
    }

    /// The thread, once loaded, shared.
    pub fn shared_detail(&self) -> Option<Arc<ThreadDetail>> {
        self.detail.clone()
    }

    /// The attachment cards' state: downloads' progress, thumbnails, zips.
    pub(crate) fn files_mut(&mut self) -> &mut attachments::Files {
        &mut self.files
    }

    /// The image attachments whose card thumbnails to fetch: those of the expanded messages,
    /// whose cards show. Each is asked for once.
    pub(crate) fn thumbnail_requests(&mut self) -> Vec<Attachment> {
        let Some(detail) = self.detail.clone() else {
            return Vec::new();
        };
        let expanded: HashSet<&str> = self
            .items
            .iter()
            .filter(|item| item.expanded)
            .map(|item| item.id.as_str())
            .collect();
        self.files.thumbnail_requests(
            Self::shown(&detail).filter(|message| expanded.contains(message.id.as_str())),
        )
    }

    /// The messages the thread shows, oldest first: everything but drafts being written, which
    /// belong to compose. A failed or scheduled draft is a message.
    pub(super) fn shown(detail: &ThreadDetail) -> impl Iterator<Item = &MessageDetail> {
        detail.messages.iter().filter(|message| {
            !message.is_draft() || message.failure.is_some() || message.scheduled.is_some()
        })
    }

    pub(super) fn message(&self, id: &str) -> Option<&MessageDetail> {
        self.detail
            .as_deref()?
            .messages
            .iter()
            .find(|message| message.id == id)
    }

    pub(super) fn focus_index(&self) -> Option<usize> {
        let focus = self.focus.as_deref()?;
        self.items.iter().position(|item| item.id == focus)
    }

    /// The focused message's sender, for the contact pane. None when it's you.
    pub fn focused_sender(&self) -> Option<Person> {
        let message = self.message(self.focus.as_deref()?)?;
        message
            .from
            .as_ref()
            .filter(|from| !from.is_me)
            .map(person_of)
    }

    /// The messages the thread shows, oldest first, once it's loaded.
    pub fn messages(&self) -> Vec<&MessageDetail> {
        self.detail
            .as_deref()
            .map(|detail| Self::shown(detail).collect())
            .unwrap_or_default()
    }

    /// Paints the thread view in `rect` (the content pane) and handles the mouse. `region` is
    /// where the keyboard is, and the messages' focus bar shows while the keyboard is theirs.
    /// `drafts` are the thread's drafts, drawn under the messages they answer. `typing` says one
    /// of them has the keyboard, so its bottom is kept in view.
    #[expect(
        clippy::too_many_arguments,
        reason = "one screen, drawn from the app's state; a struct would only rename them"
    )]
    pub(crate) fn show(
        &mut self,
        ui: &mut Ui,
        rect: Rect,
        rem: f32,
        window_width: f32,
        bodies: &mut Bodies,
        region: Region,
        events: &[Event],
        mut drafts: Option<&mut crate::compose::Inline<'_>>,
        typing: bool,
    ) -> Vec<Action> {
        let ctx = ui.ctx().clone();
        let now = ctx.input(|input| input.time);
        let palette = exo_ui::palette(&ctx);
        ui.painter()
            .rect_filled(rect, 0.0, palette.background_base_default);
        let mut actions = Vec::new();
        // A press anywhere in the pane gives the messages the keyboard.
        if ui.input(|input| input.pointer.primary_pressed()) && ui.rect_contains_pointer(rect) {
            self.clicked_in_pane();
        }
        self.hover.selection.begin();
        let mut anchored = None;
        for event in events {
            match event {
                Event::Clicked { id, hit } => self.clicked(id, hit, bodies, &mut actions),
                Event::Anchored { id, top } => anchored = Some((id.clone(), *top)),
                _ => {}
            }
        }
        let Some(detail) = self.detail.clone() else {
            return actions;
        };
        let popped_out = drafts.as_ref().is_some_and(|drafts| drafts.popped_out());
        let mut layout = Layout::new(rect, rem, window_width, popped_out);

        // The header's height depends on how the subject wraps, and the messages start under it.
        self.action.status = header::status(
            &detail,
            bodies,
            self.action.pending.as_deref(),
            self.conversation_action(bodies),
            &self.action.own_domains,
        );
        let header_height = header::height(
            ui,
            &layout,
            &detail,
            &self.action.chips,
            !self.action.spam_view,
            self.action.status.is_some(),
        );
        let messages_top = layout.header.top() + header_height + MESSAGES_MARGIN_TOP;

        // Find scrolls at once to a match out of view (Chromium's `ScrollRectToVisible`), down the
        // messages and across a widened column.
        let find = ui.id().with("thread-find");
        let reveal = exo_ui::find::reveal(&ctx, find);

        // The wheel's gesture goes to a body that scrolls sideways or to the thread, which takes
        // only the axis it runs along.
        let wheel = self.scroll_state.gesture.update(
            ui.input(|input| input.time),
            ui.input(|input| input.smooth_scroll_delta),
            self.hover
                .hovered_body
                .as_deref()
                .map(|id| (id, bodies.scroll_x(id), bodies.overflow(id))),
        );

        // A wide body widens the column, which the wheel scrolls sideways as one (a body that
        // still scrolls on its own keeps the gesture). Not beside a popped-out draft, which
        // a widened column would run under.
        self.find_wide(bodies);
        if let Some(widest) = self.widest().filter(|_| !popped_out) {
            let thread_right = container(rect, rem, window_width, popped_out).right();
            if let Some(reveal) = reveal {
                self.scroll_state.scroll_x = (self.scroll_state.scroll_x + reveal.x).round();
            }
            layout.widen(rect, thread_right, widest, self.scroll_state.scroll_x);
            let delta = wheel.x;
            if delta != 0.0
                && ui.rect_contains_pointer(layout.viewport(messages_top, rect.bottom()))
            {
                // Whole CSS px, so the bodies' subpixel phase (and their layout) stays put.
                self.scroll_state.scroll_x = (self.scroll_state.scroll_x - delta).round();
                layout.widen(rect, thread_right, widest, self.scroll_state.scroll_x);
            }
        }
        (self.scroll_state.scroll_x, self.scroll_state.max_scroll_x) = layout.scroll_x();
        let messages = layout.viewport(messages_top, rect.bottom());
        self.scroll_state.viewport = messages.height();

        // The back gutter: all of its clickable goes back, and hovering it lights the back
        // button, with a pointer cursor. The rail's back area lights it too, even while the
        // gutter has the keyboard.
        let gutter_rect = layout.gutter_target();
        let gutter = ui
            .interact(
                gutter_rect,
                ui.id().with("thread-back-gutter"),
                Sense::click(),
            )
            .on_hover_cursor(egui::CursorIcon::PointingHand);
        let rail = crate::shell::back_area_hovered(&ctx);
        let compact_clicked = header::back_button(
            ui,
            &layout,
            gutter.hovered() || rail,
            region == Region::ActionPane && !rail,
        );
        if gutter.clicked() || compact_clicked {
            actions.push(Action::Back);
        }

        let scale = ctx.pixels_per_point();
        let theme = exo_ui::theme(&ctx);
        // A message with a draft under it is expanded.
        let with_drafts = drafts
            .as_ref()
            .map(|drafts| drafts.messages())
            .unwrap_or_default();
        for item in &mut self.items {
            if with_drafts.contains(&item.id) {
                item.expanded = true;
            }
        }
        // The hint goes under the last message, without a draft.
        self.action.hint = self
            .items
            .last()
            .filter(|last| !with_drafts.contains(&last.id))
            .and_then(|_| {
                let messages: Vec<_> = self
                    .items
                    .iter()
                    .filter_map(|item| Some((self.message(&item.id)?, bodies.facts(&item.id))))
                    .collect();
                match messages.as_slice() {
                    [(only, _)] => footer::intro(only).map(footer::Hint::Intro),
                    _ => None,
                }
                .or_else(|| {
                    footer::calendar_event(&messages, &self.action.now)
                        .then_some(footer::Hint::CalendarEvent)
                })
            });
        let order = self.order(&detail);
        let mut draft_heights = HashMap::new();
        let mut entries = Vec::with_capacity(order.len());
        let mut tops = vec![(0.0, 0.0); self.items.len()];
        let mut y = MESSAGES_PADDING_TOP;
        let mut previous_margin = None::<f32>;
        let mut collapsed_before = 0;
        let mut seen_expanded = false;
        for (k, entry) in order.iter().enumerate() {
            let next = order.get(k + 1);
            let after_marker = k
                .checked_sub(1)
                .is_some_and(|previous| !matches!(order[previous], Entry::Message(_)));
            let (height, card, margin_top, margin_bottom) = match *entry {
                Entry::Message(at) => {
                    let item = &self.items[at];
                    let card = if item.expanded {
                        let layout = self.item_layout(&layout, at);
                        let marketing = bodies
                            .facts(&item.id)
                            .is_some_and(|facts| message::marketing(facts, item.show_quoted));
                        let body = bodies.height(
                            &item.id,
                            layout.look(
                                scale,
                                theme,
                                rem,
                                item.show_quoted,
                                marketing,
                                self.hides_body(&item.id, bodies),
                            ),
                        );
                        let invite = self.invite_block(ui, &item.id, layout.inner.width(), bodies);
                        let draft = match drafts.as_mut().filter(|_| with_drafts.contains(&item.id))
                        {
                            Some(drafts) => drafts.height(ui, &item.id, layout.inner.width()),
                            None => 0.0,
                        };
                        draft_heights.insert(item.id.clone(), draft);
                        message::expanded_height(
                            item.header_expanded,
                            self.message(&item.id),
                            invite,
                            body,
                            layout.inner.width(),
                        ) + draft
                    } else {
                        message::COLLAPSED_HEIGHT
                    };
                    let margin = if item.expanded {
                        message::EXPANDED_MARGIN_BOTTOM
                    } else {
                        message::COLLAPSED_MARGIN_BOTTOM
                    };
                    if !item.expanded && !seen_expanded {
                        collapsed_before += 1;
                    }
                    seen_expanded |= item.expanded;
                    if self.action.hint.is_some() && item.expanded && at + 1 == self.items.len() {
                        // The hint's -16px margin under the card's 28px, then the hint.
                        (
                            card + collapse(margin, footer::MARGIN_TOP) + footer::HEIGHT,
                            card,
                            CARD_MARGIN_TOP,
                            0.0,
                        )
                    } else if matches!(next, Some(Entry::Trashed(_))) {
                        (
                            card,
                            card,
                            CARD_MARGIN_TOP,
                            margin.max(markers::MESSAGE_MARGIN_BEFORE),
                        )
                    } else {
                        (card, card, CARD_MARGIN_TOP, margin)
                    }
                }
                Entry::Trashed(_) | Entry::Reminder => {
                    if !seen_expanded {
                        collapsed_before += usize::from(matches!(entry, Entry::Trashed(_)));
                    }
                    let margin_top = if after_marker {
                        markers::MARGIN_TOP_AFTER_MARKER
                    } else {
                        markers::MARGIN_TOP
                    };
                    // The reminder's marker is wrapped in its hover intent's `div`, which no
                    // sibling rule sees through.
                    let before_message = matches!(entry, Entry::Trashed(_))
                        && matches!(next, Some(Entry::Message(_)));
                    let margin_bottom = if before_message {
                        markers::MARGIN_BOTTOM_BEFORE_MESSAGE
                    } else {
                        markers::MARGIN_BOTTOM
                    };
                    (markers::HEIGHT, markers::HEIGHT, margin_top, margin_bottom)
                }
            };
            y += previous_margin.map_or(margin_top, |previous| collapse(previous, margin_top));
            if let Entry::Message(at) | Entry::Trashed(at) = *entry {
                tops[at] = (y, height);
            }
            entries.push(Placed {
                entry: *entry,
                top: y,
                height,
                card,
            });
            y += height;
            previous_margin = Some(margin_bottom);
        }
        self.scroll_state.tops = tops;
        self.scroll_state.entries = entries;
        // Where what's added after the messages goes: past the last entry's margin.
        self.scroll_state.end = y + previous_margin.unwrap_or(0.0);
        // The table grows so the collapsed messages above the first expanded one can scroll
        // away, and the end can scroll a little past.
        let table_top = MESSAGES_PADDING_TOP + CARD_MARGIN_TOP;
        let natural = y - table_top;
        // A thread opens with its first expanded (unread) message just under the subject, after
        // one collapsed row for context.
        let collapsed_scroll = collapsed_before.saturating_sub(1) as f32 * STEP;
        let viewport = self.scroll_state.viewport;
        let bottom_scroll = (natural - viewport + 2.0 * MESSAGES_PADDING_TOP).max(0.0);
        let max_top = collapsed_scroll.max(bottom_scroll);
        self.scroll_state.content = if max_top > 0.0 {
            table_top + natural.max(viewport + max_top - 4.0)
        } else {
            self.scroll_state.end
        };

        self.settle_scroll(
            ui,
            &layout,
            messages,
            collapsed_scroll,
            anchored,
            reveal,
            wheel.y,
            typing,
            bodies,
            drafts.as_deref(),
            &draft_heights,
            &mut actions,
            now,
            scale,
        );

        // The header comes before the messages on the page, though it's painted over them.
        let header_slot = exo_ui::find::slot(&ctx);
        // Everything in the messages is clipped to them, under the header.
        let mut list = ui.new_child(UiBuilder::new().max_rect(messages));
        list.set_clip_rect(messages.intersect(ui.clip_rect()));
        let show_focus = matches!(
            region,
            Region::ThreadMessages | Region::ThreadMessagesNewMessages
        );
        let scope = exo_ui::find::Scope::begin(&ctx, find, messages, self.scroll_state.scroll);
        self.paint_messages(
            &mut list,
            &layout,
            &detail,
            messages,
            rem,
            show_focus,
            bodies,
            &mut actions,
            drafts,
            &draft_heights,
        );
        self.paint_highlight(&list, &layout, messages, region);
        if layout.is_wide() {
            layout.paint_edges(ui, messages.top(), messages.bottom());
            self.scroll_state.scrollbar_x.show_horizontal(
                &list,
                list.id().with("thread-scrollbar-x"),
                messages,
                self.scroll_state.scroll_x,
                messages.width() + self.scroll_state.max_scroll_x,
            );
        }
        if self.action.has_new {
            let target = messages.top() - self.scroll_state.scroll + self.scroll_state.end;
            if new_messages::show(
                &list,
                &layout,
                messages,
                target,
                region == Region::ThreadMessagesNewMessages,
            ) {
                actions.push(Action::ShowNewMessages);
            }
        }
        drop(scope);
        let scrolled = self.scroll_state.scroll > 0.0;
        let focus = if region == Region::ThreadMessagesHeader {
            self.header_focus()
        } else {
            None
        };
        let header = exo_ui::find::in_slot(&ctx, header_slot, || {
            header::show(
                ui,
                &layout,
                &detail,
                &self.action.chips,
                !self.action.spam_view,
                header_height,
                self.action.status.as_ref(),
                scrolled,
                typing,
                focus,
                &mut self.hover.selection,
                &self.action.now,
            )
        });
        actions.extend(header);
        self.select_header_text(ui, bodies, region, &mut actions);
        self.header_tooltips(ui, &layout, &detail, messages, &mut actions);
        if let Some(person) = self.hover.contact.take() {
            actions.push(Action::ShowContact(person));
        }
        actions
    }

    /// Paints the bar that slides to the focused message, over its card.
    fn paint_highlight(&mut self, ui: &Ui, layout: &Layout, messages: Rect, region: Region) {
        let Some(focus) = self.focus.clone() else {
            return;
        };
        let Some((at, placed)) =
            self.scroll_state
                .entries
                .iter()
                .find_map(|placed| match placed.entry {
                    Entry::Message(at) if self.items[at].id == focus => Some((at, *placed)),
                    _ => None,
                })
        else {
            return;
        };
        // Cards start at the column's left edge, as wide as the message's layout makes them.
        let card = Rect::from_min_size(
            pos2(layout.card.left(), placed.top),
            vec2(self.item_layout(layout, at).card.width(), placed.card),
        );
        let origin = messages.top() - self.scroll_state.scroll;
        self.scroll_state
            .highlight
            .paint(ui, &focus, card, origin, region);
    }

    /// What the messages column lists, in order: the messages, a trashed one folded into its
    /// marker, and `Reminder returned` after the last message that came before it.
    pub(super) fn order(&self, detail: &ThreadDetail) -> Vec<Entry> {
        let mut order: Vec<Entry> = self
            .items
            .iter()
            .enumerate()
            .map(|(at, item)| {
                if item.trashed && !item.expanded {
                    Entry::Trashed(at)
                } else {
                    Entry::Message(at)
                }
            })
            .collect();
        if let Some(exo_store::ReminderStatus::Returned { at, .. }) = detail.reminder {
            let date = |entry: &Entry| match *entry {
                Entry::Message(item) | Entry::Trashed(item) => self
                    .message(&self.items[item].id)
                    .map(|message| message.date),
                Entry::Reminder => None,
            };
            let after = order
                .iter()
                .rposition(|entry| date(entry).is_some_and(|date| date <= at))
                .map_or(0, |last| last + 1);
            order.insert(after, Entry::Reminder);
        }
        order
    }

    /// Paints the visible messages and markers into `ui` (clipped to the message column) and
    /// handles their clicks.
    #[expect(
        clippy::too_many_arguments,
        reason = "the frame's layout, handed down from show"
    )]
    fn paint_messages(
        &mut self,
        ui: &mut Ui,
        layout: &Layout,
        detail: &ThreadDetail,
        messages: Rect,
        rem: f32,
        focused: bool,
        bodies: &mut Bodies,
        actions: &mut Vec<Action>,
        mut drafts: Option<&mut crate::compose::Inline<'_>>,
        draft_heights: &HashMap<String, f32>,
    ) {
        let scale = ui.ctx().pixels_per_point();
        let theme = exo_ui::theme(ui.ctx());
        let origin = pos2(
            layout.card.left(),
            messages.top() - self.scroll_state.scroll,
        );
        let pointer = ui.input(|input| input.pointer.hover_pos());
        self.hover.hovered_body = None;
        self.hover.named.clear();
        self.hover.body_origins.clear();
        // Failed drafts' cards, whose open tooltips go over every card.
        let mut failed = Vec::new();
        // The body holding the selection, whose hint goes over every card.
        let mut selected = None;
        // The invite whose When is under the pointer: its message, and when it starts.
        let mut on_when = None;
        // Every message counts, in view or not, because find searches them all.
        let searching = exo_ui::find::searching(ui.ctx());
        for placed in self.scroll_state.entries.clone() {
            let layout = &match placed.entry {
                Entry::Message(at) => self.item_layout(layout, at),
                Entry::Trashed(_) | Entry::Reminder => Self::spanning(layout),
            };
            let card = Rect::from_min_size(
                origin + vec2(0.0, placed.top),
                vec2(layout.card.width(), placed.card),
            );
            if !searching
                && !Rect::from_min_size(card.min, vec2(card.width(), placed.height))
                    .expand(32.0)
                    .intersects(messages)
            {
                continue;
            }
            let at = match placed.entry {
                Entry::Message(at) => at,
                Entry::Trashed(at) => {
                    let id = ui.id().with(("trashed", &self.items[at].id));
                    if markers::trashed(ui, card, layout, id) {
                        // Expands it, then focuses it and scrolls to it.
                        self.items[at].expanded = true;
                        self.count_toggle(true, actions);
                        self.focus_at(at, 0);
                    }
                    continue;
                }
                Entry::Reminder => {
                    if let Some(exo_store::ReminderStatus::Returned { at, .. }) = detail.reminder {
                        markers::reminder(
                            ui,
                            card,
                            layout,
                            at,
                            &self.action.now,
                            ui.id().with("reminder-date"),
                        );
                    }
                    continue;
                }
            };
            let item = self.items[at].clone();
            let Some(message) = detail.messages.iter().find(|message| message.id == item.id) else {
                continue;
            };
            let is_focus = self.focus.as_deref() == Some(&item.id);
            let state = message::State {
                focused: is_focus,
                show_focus: focused,
                header_expanded: item.header_expanded,
                collapsible: self.collapsible(at),
                composing: drafts
                    .as_ref()
                    .is_some_and(|drafts| drafts.has_form(&item.id)),
                offline: !self.action.online,
                full_names: self.action.full_names,
                reply_labels: is_focus && self.new_account(),
            };
            let response = ui.interact(
                card.intersect(messages),
                ui.id().with(("message", &item.id)),
                Sense::click_and_drag(),
            );
            if item.expanded {
                let marketing = bodies
                    .facts(&item.id)
                    .is_some_and(|facts| message::marketing(facts, item.show_quoted));
                let look = layout.look(
                    scale,
                    theme,
                    rem,
                    item.show_quoted,
                    marketing,
                    self.hides_body(&item.id, bodies),
                );
                let invite_block = self.invite_block(ui, &item.id, layout.inner.width(), bodies);
                let content_top = card.top() + message::body_offset(item.header_expanded, message);
                let body = bodies.height(&item.id, look).map(|height| {
                    Rect::from_min_size(
                        pos2(layout.inner.left(), content_top + invite_block),
                        vec2(layout.inner.width(), height),
                    )
                });
                let pointed = body.map(|body| {
                    overflow::scroll(ui, bodies, &self.scroll_state.gesture, &item.id, body)
                });
                let hit = message::expanded(
                    ui,
                    card,
                    layout,
                    message,
                    detail,
                    marketing,
                    &state,
                    look,
                    bodies,
                    &self.files,
                    invite_block,
                    &self.action.now,
                    response.hovered(),
                    &mut self.hover.named,
                    &mut self.hover.selection,
                );
                if message.failure.is_some() && !item.header_expanded {
                    failed.push((card, at));
                }
                if let Some(hovered_when) =
                    self.paint_invite_card(ui, layout, message, content_top, bodies, actions)
                {
                    on_when = Some(hovered_when);
                }
                if let (Some(body), Some(pointed)) = (body, pointed) {
                    self.hover.body_origins.insert(item.id.clone(), body.min);
                    overflow::scrollbar(ui, bodies, &item.id, body);
                    self.body_pointer(ui, &response, &item.id, pointed, pointer, bodies);
                    if bodies
                        .selection_bounds()
                        .is_some_and(|(id, _)| id == item.id)
                    {
                        selected = Some((item.id.clone(), body.min));
                    }
                }
                let draft = draft_heights.get(&item.id).copied().unwrap_or(0.0);
                if draft > 0.0
                    && let Some(drafts) = drafts.as_mut()
                {
                    let top = card.bottom() - message::PADDING_BOTTOM - draft;
                    let form = Rect::from_min_size(
                        pos2(layout.inner.left(), top),
                        vec2(layout.inner.width(), draft),
                    );
                    // A marketing card in Carbon is white, so its draft keeps Snow's colors, as the
                    // header's ink does (and Carbon's dialogs on their light cards).
                    let shown = if marketing && theme == exo_ui::Theme::Carbon {
                        let ctx = ui.ctx().clone();
                        exo_ui::with_theme(&ctx, exo_ui::Theme::Snow, || {
                            drafts.show(ui, &item.id, form, card, layout.column.x_range())
                        })
                    } else {
                        drafts.show(ui, &item.id, form, card, layout.column.x_range())
                    };
                    actions.extend(shown.into_iter().map(Action::Compose));
                }
                if let Some(hint) = self
                    .action
                    .hint
                    .as_ref()
                    .filter(|_| at + 1 == self.items.len())
                {
                    footer::paint_hint(
                        ui,
                        layout,
                        card.bottom()
                            + collapse(message::EXPANDED_MARGIN_BOTTOM, footer::MARGIN_TOP),
                        hint,
                    );
                }
                match hit {
                    Some(message::Hit::Header) if state.collapsible && draft == 0.0 => {
                        self.items[at].expanded = false;
                        self.count_toggle(false, actions);
                    }
                    Some(message::Hit::HeaderToggle { hint }) => {
                        self.items[at].header_expanded = !item.header_expanded;
                        if hint {
                            actions.push(Action::HeaderHint {
                                expanded: !item.header_expanded,
                            });
                        }
                    }
                    Some(message::Hit::Files(attachments::Click::Open {
                        attachment,
                        preview,
                    })) => {
                        actions.push(open_file(attachments::listed(message), attachment, preview))
                    }
                    Some(message::Hit::Files(attachments::Click::DownloadAll { message })) => {
                        actions.push(Action::DownloadAll { message })
                    }
                    Some(message::Hit::Files(attachments::Click::Drag { attachment, at })) => {
                        actions.push(Action::DragFile { attachment, at })
                    }
                    Some(message::Hit::Reply(action)) => {
                        self.focus = Some(item.id.clone());
                        actions.push(Action::Compose(crate::compose::Action::Reply(action)));
                    }
                    Some(message::Hit::Recover) => {
                        self.focus = Some(item.id.clone());
                        actions.push(Action::Compose(crate::compose::Action::Recover(
                            item.id.clone(),
                        )));
                    }
                    Some(message::Hit::Header) | None => {}
                }
                if response.clicked() || response.drag_started() {
                    self.focus = Some(item.id.clone());
                }
            } else {
                message::collapsed(
                    ui,
                    card,
                    layout,
                    message,
                    detail,
                    bodies.facts(&item.id),
                    &state,
                    &self.action.now,
                    response.hovered(),
                    &mut self.hover.named,
                );
                // A pointer cursor over a collapsed row.
                let response = response.on_hover_cursor(egui::CursorIcon::PointingHand);
                if response.clicked() {
                    self.items[at].expanded = true;
                    self.focus = Some(item.id.clone());
                    self.count_toggle(true, actions);
                }
            }
        }
        // The tooltips go in a layer of their own, so neither the column nor its cards cut their
        // shadows. The layer stays under the thread's header.
        let mut tooltips = ui.painter().clone();
        tooltips.set_clip_rect(Rect::from_x_y_ranges(
            ui.ctx().content_rect().x_range(),
            messages.y_range(),
        ));
        for (card, at) in failed {
            let Some(message) = detail
                .messages
                .iter()
                .find(|message| message.id == self.items[at].id)
            else {
                continue;
            };
            if let Some(url) = message::failed_tooltip(ui, &tooltips, layout, card, message) {
                actions.push(Action::OpenUrl(url.to_owned()));
            }
        }
        if let Some((id, body)) = selected {
            self.selection_hint(ui, &Self::spanning(layout), messages, body, bodies, &id);
        }
        if let Some(id) = self.invite_state.when_intent.update(
            ui.ctx(),
            on_when
                .as_ref()
                .map(|(id, _): &(String, jiff::Timestamp)| id.clone()),
        ) && let Some((_, start)) = on_when.as_ref().filter(|(over, _)| over == id)
        {
            actions.push(Action::ShowDay(
                start.to_zoned(self.action.now.time_zone().clone()).date(),
            ));
        }
        self.hover_date(ui, &Self::spanning(layout), messages, bodies, actions);
        // Hovering a `mailto:` link shows its address's contact, with hover intent.
        let mailto = self
            .hover
            .hovered_body
            .as_deref()
            .and_then(|id| match bodies.hovered(id) {
                Some(Hit::Link { href })
                    if href
                        .get(..7)
                        .is_some_and(|scheme| scheme.eq_ignore_ascii_case("mailto:")) =>
                {
                    Some(href.clone())
                }
                _ => None,
            });
        self.hover_mailto(ui.ctx(), mailto);
        self.scroll_state.scrollbar.show(
            ui,
            ui.id().with("thread-scrollbar"),
            messages,
            self.scroll_state.scroll,
            self.scroll_state.content,
        );
    }
}

#[cfg(test)]
mod tests {
    use exo_store::Contact;

    use super::*;
    use crate::ViewContext;

    fn now() -> Zoned {
        "2026-11-17T09:00:00[America/New_York]"
            .parse()
            .expect("valid time")
    }

    fn message(id: &str, labels: &[&str]) -> MessageDetail {
        MessageDetail {
            id: id.to_owned(),
            labels: labels.iter().map(|label| (*label).to_owned()).collect(),
            date: now().timestamp(),
            from: Some(Contact {
                name: Some("Alex".into()),
                email: "alex@brightmoor.co".into(),
                is_me: false,
            }),
            to: Vec::new(),
            cc: Vec::new(),
            bcc: Vec::new(),
            reply_to: Vec::new(),
            subject: String::new(),
            snippet: String::new(),
            rfc822_id: None,
            in_reply_to: None,
            references: Vec::new(),
            list_id: None,
            list_unsubscribe: None,
            list_unsubscribe_post: None,
            headers: Vec::new(),
            body: exo_store::Body::default(),
            attachments: Vec::new(),
            failure: None,
            scheduled: None,
            sending: false,
        }
    }

    #[test]
    fn sync_context_updates_trash_visibility_and_account_environment() {
        let mut view = ThreadView::new(
            "t1".into(),
            now(),
            "maya@brightmoor.co".into(),
            PathBuf::new(),
        );
        let detail = Arc::new(ThreadDetail {
            id: exo_store::ThreadId::new("t1"),
            subject: "Quarterly sync".into(),
            labels: vec!["INBOX".into()],
            muted: false,
            reminder: None,
            messages: vec![message("m1", &["TRASH"]), message("m2", &["INBOX"])],
        });
        view.set_detail(detail);
        assert_eq!(
            view.expanded(),
            vec![false, true],
            "trashed message folds into marker outside Trash"
        );
        assert_eq!(view.focused(), Some("m2"));

        let mut ctx = ViewContext::new(now());
        ctx.trash_shown = true;
        ctx.online = false;
        ctx.full_names = true;
        ctx.spam_view = true;
        ctx.joined = Some(now().timestamp());
        ctx.pending.insert("t1".into(), "Pending: Mark Done".into());
        ctx.own_domains = vec!["brightmoor.co".into()];
        ctx.calendar_accounts = vec!["team@brightmoor.co".into()];
        ctx.signed_out = vec!["team@brightmoor.co".into()];
        view.sync_context(&ctx);

        assert!(!view.items[0].trashed, "Trash view unfolds trashed marker");
        assert!(!view.action.online);
        assert!(view.action.full_names);
        assert!(view.new_account());
        assert_eq!(view.action.pending.as_deref(), Some("Pending: Mark Done"));
        assert!(view.action.spam_view);
        assert_eq!(view.invite_state.calendar_accounts, ["team@brightmoor.co"]);
        assert_eq!(view.invite_state.signed_out, ["team@brightmoor.co"]);
    }

    #[test]
    fn focus_zones_and_new_messages_round_trip() {
        let mut view = ThreadView::new(
            "t1".into(),
            now(),
            "maya@brightmoor.co".into(),
            PathBuf::new(),
        );
        view.set_chips(vec!["Work".into()]);
        assert_eq!(view.action.chips, ["Work"]);
        view.set_detail(Arc::new(ThreadDetail {
            id: exo_store::ThreadId::new("t1"),
            subject: "Quarterly sync".into(),
            labels: vec!["INBOX".into()],
            muted: false,
            reminder: None,
            messages: vec![message("m1", &["INBOX"])],
        }));
        assert_eq!(view.region(), Region::ThreadMessages);

        // ← at left edge enters ActionPane; → returns to Messages.
        view.left(false);
        assert_eq!(view.region(), Region::ActionPane);
        view.gutter_key(GutterKey::Right);
        assert_eq!(view.region(), Region::ThreadMessages);

        // Arriving message is held behind Show New Messages until revealed.
        view.set_detail(Arc::new(ThreadDetail {
            id: exo_store::ThreadId::new("t1"),
            subject: "Quarterly sync".into(),
            labels: vec!["INBOX".into()],
            muted: false,
            reminder: None,
            messages: vec![message("m1", &["INBOX"]), message("m2", &["INBOX"])],
        }));
        assert!(view.has_new_messages());
        assert!(view.arrow_out(1, 0));
        assert!(view.new_messages_focused());
        assert_eq!(view.region(), Region::ThreadMessagesNewMessages);

        assert!(view.show_new_messages());
        assert!(!view.has_new_messages());
        assert_eq!(view.region(), Region::ThreadMessages);
        assert_eq!(view.focused(), Some("m2"));
    }

    #[test]
    fn scroll_state_clamps_wide_horizontal_scroll() {
        let mut view = ThreadView::new(
            "t1".into(),
            now(),
            "maya@brightmoor.co".into(),
            PathBuf::new(),
        );
        assert_eq!(view.scroll(), 0.0);
        assert!(!view.scroll_sideways(1), "unwidened column does not scroll");

        view.scroll_state.max_scroll_x = 200.0;
        assert!(view.scroll_sideways(1));
        assert_eq!(view.scroll_state.scroll_x, 175.0);
        assert!(view.scroll_sideways(1));
        assert_eq!(view.scroll_state.scroll_x, 200.0);
        assert!(!view.scroll_sideways(1), "clamped at max_scroll_x");

        // ← scrolls sideways first; only enters ActionPane once back at left edge.
        view.left(false);
        assert_eq!(view.scroll_state.scroll_x, 25.0);
        assert_eq!(view.region(), Region::ThreadMessages);
        view.left(false);
        assert_eq!(view.scroll_state.scroll_x, 0.0);
        assert_eq!(view.region(), Region::ThreadMessages);
        view.left(false);
        assert_eq!(view.region(), Region::ActionPane);
    }

    #[test]
    fn invite_state_tracks_parsed_invites_and_rsvp_keyboard_focus() {
        let mut view = ThreadView::new(
            "t1".into(),
            now(),
            "maya@brightmoor.co".into(),
            PathBuf::new(),
        );
        view.set_detail(Arc::new(ThreadDetail {
            id: exo_store::ThreadId::new("t1"),
            subject: "Design review".into(),
            labels: vec!["INBOX".into()],
            muted: false,
            reminder: None,
            messages: vec![message("m1", &["INBOX"])],
        }));
        let ics = concat!(
            "BEGIN:VCALENDAR\r\n",
            "VERSION:2.0\r\n",
            "METHOD:REQUEST\r\n",
            "BEGIN:VEVENT\r\n",
            "UID:evt-42@brightmoor.co\r\n",
            "SUMMARY:Design review\r\n",
            "DTSTART:20261118T150000Z\r\n",
            "DTEND:20261118T160000Z\r\n",
            "ORGANIZER;CN=Alex:mailto:alex@brightmoor.co\r\n",
            "ATTENDEE;PARTSTAT=NEEDS-ACTION:mailto:maya@brightmoor.co\r\n",
            "END:VEVENT\r\n",
            "END:VCALENDAR\r\n",
        );
        view.set_invite("m1", ics.as_bytes());
        assert_eq!(view.invite_uids(), ["evt-42@brightmoor.co"]);
        assert_eq!(view.invite_message("evt-42@brightmoor.co"), Some("m1"));
        assert!(view.can_focus_invite());

        assert!(view.focus_invite());
        assert!(view.invite_focused());
        view.move_invite_focus(1);
        let (_, response) = view.submit_invite().expect("submits second RSVP button");
        assert_eq!(response, invite::ANSWERS[1]);
        view.blur_invite();
        assert!(!view.invite_focused());
    }
}

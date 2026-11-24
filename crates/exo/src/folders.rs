//! The folder drawer: the account, the system folders (Snippets among them) and the user's labels
//! in a drawer that slides out over the list, opened with ← or the header's folders button.
//!
//! Items are numbered from 0, the account, then the folders, then the labels; a nested label's
//! parent heading isn't an item.
//!
//! Over Inbox Zero's photo the drawer is see-through: black at .3 over the photo blurred 35px,
//! with white text.

use egui::{
    Color32, CornerRadius, Id, Painter, Pos2, Rect, Sense, Ui, Vec2, pos2, text::LayoutJob, vec2,
};
use exo_store::{Label, ListCount, ListId};
use exo_ui::{
    Behind, BoxShadow, Family, Font, HighlightState, HighlightTarget, KeycapColors, Keycaps,
    ListHighlight, Palette, TextStyle, Theme,
    motion::{self, CubicBezier},
    paint_job,
};

use crate::lists;

/// The drawer's left edge: right of the rail.
const LEFT: f32 = 40.0;
/// `width: 30vw; min-width: 300px; max-width: 416px`.
const MIN_WIDTH: f32 = 300.0;
const MAX_WIDTH: f32 = 416.0;
/// `transition: transform 200ms ease, opacity 200ms ease`.
const SLIDE: f32 = 0.2;
/// The space above the 48px account row.
const ACCOUNT_TOP: f32 = 20.0;
/// Where the scrolling list starts, from the drawer's top.
const SCROLL_TOP: f32 = 76.0;
/// A row's height, folders, labels and headings alike, its text 16px from its top.
const ROW: f32 = 48.0;
const ROW_PADDING_TOP: f32 = 16.0;
/// Nested labels: 12px above and below the group, pulled 4px up, and 4px back after the last.
const NESTED_PADDING: f32 = 12.0;
const NESTED_PULL: f32 = 4.0;
/// The divider: 11px above its 1px line and 12px below.
const DIVIDER: f32 = 11.0 + 1.0 + 12.0;
const DIVIDER_LINE: f32 = 11.0;
/// A row's label: 12px/16px bold, 1px down.
const LABEL: TextStyle = TextStyle::new(Font::new(Family::Sans, 700), 12.0, 16.0);
/// The Inbox row's `Important • Other`: 12px/16px, `lighter` than the bold label, `.017em`.
const IMPORTANT_OTHER: TextStyle =
    TextStyle::new(Font::new(Family::Sans, 400), 12.0, 16.0).letter_spacing(0.017 * 12.0);
/// The account avatar: a 32px circle, `margin-right: 14px`.
const AVATAR: f32 = 32.0;
const AVATAR_GAP: f32 = 14.0;
/// The avatar's initial: 18px, bold like the row it's in, centered with `dy: .35em`.
const INITIAL: TextStyle = TextStyle::new(Font::new(Family::Sans, 700), 18.0, 18.0);
/// The cursor's highlight: 6px in from either side; fade duration and curve match `ListHighlight`.
const HIGHLIGHT_INSET: f32 = 6.0;
const HIGHLIGHT_FADE: f32 = 0.512;
const HIGHLIGHT_CURVE: CubicBezier = CubicBezier::new(0.19, 1.0, 0.22, 1.0);
/// Labels listed before `Load More`, unless there are fewer than ten more.
const LABELS_SHOWN: usize = 100;
/// `INBOX_COUNTS`: counts this large aren't shown.
const MAX_COUNT: usize = 1000;
/// The Inbox count's offset: 75px right while hidden, 8px left on hover, over 100ms.
const COUNT_HIDDEN: f32 = 75.0;
const COUNT_SHOWN: f32 = -8.0;
const HOVER: f32 = 0.1;
/// Over the photo: `background: rgba(0,0,0,.3); backdrop-filter: blur(35px)`.
const SEE_THROUGH: Color32 = Color32::from_rgba_premultiplied(0, 0, 0, 77);
const BLUR: f32 = 35.0;

/// Where a folder of the drawer goes: a list, or the Snippets folder, which isn't one.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Place {
    /// A list of conversations.
    List(ListId),
    /// The Snippets folder (`SH_SNIPPETS`).
    Snippets,
}

/// A line of the drawer below the account.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Line {
    /// A system folder, with its `g` key if it has one.
    Folder {
        place: Place,
        name: &'static str,
        key: Option<&'static str>,
    },
    /// A user label, showing the part of its name after its parent's when nested.
    Label {
        list: ListId,
        name: String,
        nested: bool,
    },
    /// The parent of nested labels that isn't a label itself.
    Header(String),
    /// More than 110 labels: the rest.
    LoadMore,
}

impl Line {
    /// Whether the cursor can land on it.
    fn is_item(&self) -> bool {
        !matches!(self, Self::Header(_))
    }
}

/// The system folders in the drawer's order. Scheduled, which has no `g` key, always shows, and so
/// do Reminders and Snippets. Muted shows once mute has been used.
fn folders(muted: bool) -> Vec<Line> {
    let folder = |list: ListId, key: Option<&'static str>| Line::Folder {
        name: lists::name(&list, &[], &[]),
        place: Place::List(list),
        key,
    };
    let mut folders = vec![
        folder(ListId::Inbox, Some("I")),
        folder(ListId::Starred, Some("S")),
        folder(ListId::Drafts, Some("D")),
        folder(ListId::Sent, Some("T")),
        folder(ListId::Done, Some("E")),
        folder(ListId::Scheduled, None),
        folder(ListId::Reminders, Some("H")),
        Line::Folder {
            place: Place::Snippets,
            name: "Snippets",
            key: Some(";"),
        },
    ];
    if muted {
        folders.push(folder(ListId::Muted, Some("M")));
    }
    folders.extend([
        folder(ListId::Spam, Some("!")),
        folder(ListId::Trash, Some("#")),
    ]);
    folders
}

/// The drawer's lines: the folders, then the user's labels by name (a leading `[` ignored), nested
/// ones under their parent, and `Load More` past 100 of them unless `all`.
pub fn lines(labels: &[Label], muted: bool, all: bool) -> Vec<Line> {
    let mut user: Vec<&Label> = lists::listed_labels(labels).collect();
    user.sort_by(|a, b| {
        crate::mailbox::compare_label_names(
            a.name.trim_start_matches('['),
            b.name.trim_start_matches('['),
        )
    });
    let shown = if all || user.len() < LABELS_SHOWN + 10 {
        user.len()
    } else {
        LABELS_SHOWN
    };
    let mut lines = folders(muted);
    let mut parent = String::new();
    for label in &user[..shown] {
        let list = ListId::Label(label.id.clone());
        match label.name.split_once('/') {
            Some((top, rest)) => {
                if parent != top {
                    parent = top.to_owned();
                    lines.push(Line::Header(parent.clone()));
                }
                lines.push(Line::Label {
                    list,
                    name: rest.to_owned(),
                    nested: true,
                });
            }
            None => {
                parent.clone_from(&label.name);
                lines.push(Line::Label {
                    list,
                    name: label.name.clone(),
                    nested: false,
                });
            }
        }
    }
    if shown < user.len() {
        lines.push(Line::LoadMore);
    }
    lines
}

/// What a key does in the drawer.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Key {
    /// ↑: up, onto the account row too, but not when held.
    Up,
    /// K, P (and ↑ held): up, stopping at the first folder.
    UpWithin,
    /// ↓, J, N.
    Down,
    /// ⌘↑, Home, ⇧K.
    Top,
    /// ⌘↓, End, ⇧J.
    Bottom,
    /// Page Down, Space.
    PageDown,
    /// Page Up, ⇧Space.
    PageUp,
    /// Enter: open the item.
    Enter,
    /// →: close, or switch accounts on the account row.
    Right,
    /// Esc, ⌘→.
    Close,
}

/// What the drawer asks of the app.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Action {
    /// Go there, with the hint for having clicked it (`None` from the keyboard).
    Show(Place, Option<Hint>),
    /// Close the drawer.
    Close,
    /// Open the Accounts palette, from a click, Enter or → on the account row.
    SwitchAccounts,
}

/// The hint a click on a folder shows: its `g` key, or for a label, the palette.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Hint {
    /// `Next time, hit G then {key} to go to {name} instantly!`
    GoTo {
        key: &'static str,
        name: &'static str,
    },
    /// `Next time, hit ⌘ K and enter the label to go there faster!`
    Label,
}

/// What the drawer shows besides its lines.
pub struct View<'a> {
    /// The account's address.
    pub email: &'a str,
    /// The account's color.
    pub color: Color32,
    /// The Inbox's count, shown on its row while hovered. It's Important's count while Important
    /// and Other are on.
    pub inbox: Option<ListCount>,
    /// Whether Important and Other are on, which the Inbox's row says.
    pub important_other: bool,
    /// What the drawer shows through, over Inbox Zero's photo.
    pub behind: Option<&'a Behind>,
}

/// The drawer's state.
pub struct Folders {
    open: bool,
    /// The highlighted item.
    cursor: usize,
    /// Whether the highlight jumps rather than slides to the next position (a hover moved it).
    jump: bool,
    /// Whether the pointer moved over the drawer since the keyboard last moved the cursor, in
    /// which case hovered folders show their keys and count.
    mousing: bool,
    /// Whether every label is listed (`Load More` was chosen).
    all: bool,
    scroll: f32,
    /// The scroll area's visible height, from the last frame.
    viewport: f32,
    /// Whether the cursor moved by key, so it scrolls into view.
    reveal: bool,
    last_pointer: Option<Pos2>,
    /// Whether the pointer was over the drawer last frame.
    inside: bool,
}

impl Default for Folders {
    fn default() -> Self {
        Self {
            open: false,
            cursor: 1,
            jump: false,
            mousing: false,
            all: false,
            scroll: 0.0,
            viewport: 0.0,
            reveal: false,
            last_pointer: None,
            inside: false,
        }
    }
}

impl Folders {
    /// Whether it's open, in which case it has the keyboard and clicks outside it close it.
    pub fn is_open(&self) -> bool {
        self.open
    }

    /// Whether every label is listed.
    pub fn all(&self) -> bool {
        self.all
    }

    /// Opens on the item for `current` (the first folder for the inbox views or a list the drawer
    /// doesn't have).
    pub fn open(&mut self, lines: &[Line], current: &Place) {
        let current = match current {
            Place::List(list) if lists::is_inbox(list) => &Place::List(ListId::Inbox),
            place => place,
        };
        let at = lines
            .iter()
            .filter(|line| line.is_item())
            .position(|line| match line {
                Line::Folder { place, .. } => place == current,
                Line::Label { list, .. } => {
                    matches!(current, Place::List(current) if current == list)
                }
                _ => false,
            });
        *self = Self {
            open: true,
            cursor: at.map_or(1, |at| at + 1),
            jump: true,
            reveal: true,
            // A pointer that was already over where the drawer opens hasn't moved over it.
            last_pointer: self.last_pointer,
            ..Self::default()
        };
    }

    /// Closes it.
    pub fn close(&mut self) {
        self.open = false;
    }

    /// Carries out `key` over `lines`.
    pub fn key(&mut self, key: Key, lines: &[Line]) -> Option<Action> {
        let last = lines.iter().filter(|line| line.is_item()).count();
        let page = (self.viewport / ROW).floor().max(1.0) as usize;
        let to = match key {
            Key::Up => self.cursor.saturating_sub(1),
            Key::UpWithin => self.cursor.saturating_sub(1).max(1).min(self.cursor),
            Key::Down => (self.cursor + 1).min(last),
            Key::Top => 1,
            Key::Bottom => last,
            Key::PageDown => (self.cursor + page).min(last),
            Key::PageUp => self.cursor.saturating_sub(page).max(1),
            Key::Enter => return self.choose(lines, self.cursor, false),
            Key::Right if self.cursor == 0 => return Some(Action::SwitchAccounts),
            Key::Right | Key::Close => return Some(Action::Close),
        };
        self.move_to(to, false);
        None
    }

    fn move_to(&mut self, cursor: usize, by_pointer: bool) {
        if !by_pointer {
            self.mousing = false;
            self.reveal = true;
        }
        self.jump = by_pointer;
        self.cursor = cursor;
    }

    /// Chooses item `index`, which opens its list or lists every label.
    fn choose(&mut self, lines: &[Line], index: usize, clicked: bool) -> Option<Action> {
        let Some(item) = index.checked_sub(1) else {
            return Some(Action::SwitchAccounts);
        };
        let line = lines.iter().filter(|line| line.is_item()).nth(item)?;
        match line {
            // A folder without a key (Scheduled) has no hint.
            Line::Folder { place, name, key } => Some(Action::Show(
                place.clone(),
                key.filter(|_| clicked).map(|key| Hint::GoTo { key, name }),
            )),
            Line::Label { list, .. } => Some(Action::Show(
                Place::List(list.clone()),
                clicked.then_some(Hint::Label),
            )),
            Line::LoadMore => {
                self.all = true;
                None
            }
            Line::Header(_) => None,
        }
    }

    /// Paints the drawer over `page` (the window below the tab bar) and handles the mouse.
    pub fn show(
        &mut self,
        ui: &Ui,
        page: Rect,
        rem: f32,
        lines: &[Line],
        view: &View<'_>,
    ) -> Option<Action> {
        let ctx = ui.ctx();
        let id = exo_ui::id(ctx, "exo::folders");
        let shown = motion::transition(ctx, id.with("slide"), self.open, SLIDE, CubicBezier::EASE);
        if shown <= 0.0 {
            self.last_pointer = ctx.input(|input| input.pointer.hover_pos());
            return None;
        }
        let width = (0.3 * page.width()).clamp(MIN_WIDTH, MAX_WIDTH);
        let drawer = Rect::from_min_size(
            pos2(page.left() + LEFT - width * (1.0 - shown), page.top()),
            vec2(width, page.height()),
        );
        // Under the toasts' stack and the dialogs, whenever it opened.
        egui::Area::new(id)
            .order(egui::Order::Middle)
            .fixed_pos(page.min)
            .show(ctx, |ui| {
                self.draw(ui, page, drawer, rem, shown, lines, view)
            })
            .inner
    }

    #[expect(
        clippy::too_many_arguments,
        reason = "one drawer, drawn from its own state; a struct would only rename them"
    )]
    fn draw(
        &mut self,
        ui: &mut Ui,
        page: Rect,
        drawer: Rect,
        rem: f32,
        shown: f32,
        lines: &[Line],
        view: &View<'_>,
    ) -> Option<Action> {
        let ctx = ui.ctx().clone();
        let id = exo_ui::id(&ctx, "exo::folders");
        let palette = exo_ui::palette(&ctx);
        let inks = Inks::new(palette, view.behind.is_some());
        let mut action = None;
        // A click anywhere else closes the drawer, and goes no further.
        if self.open && ui.allocate_rect(page, Sense::click()).clicked() {
            action = Some(Action::Close);
        }
        ui.multiply_opacity(shown);
        // The rail stays on top. The drawer slides out from under it, and the drawer's shadow
        // doesn't reach over it.
        ui.shrink_clip_rect(Rect::from_min_max(
            pos2(page.left() + LEFT, page.top()),
            page.max,
        ));
        let painter = ui.painter().clone();
        // `clip-path: inset(0 -30px -30px -30px)`, so no shadow shows above the drawer.
        let shadow_painter = painter.with_clip_rect(Rect::from_min_max(
            pos2(page.left(), drawer.top()),
            page.max,
        ));
        // The drawer's shadow: `0 0 30px 0 rgba(0, 0, 0, .1), 0 0 .5px 0 rgba(0, 0, 0, .05)`, or
        // `0 0 30px 0 #000` in Carbon. Under the see-through drawer it's only outside it, as CSS
        // clips a box-shadow.
        let shadows: &[BoxShadow] = match exo_ui::theme(&ctx) {
            Theme::Snow => &[
                BoxShadow::new([0.0, 0.0], 30.0, 0.0, Color32::from_black_alpha(26)),
                BoxShadow::new([0.0, 0.0], 0.5, 0.0, Color32::from_black_alpha(13)),
            ],
            Theme::Carbon => &[BoxShadow::new([0.0, 0.0], 30.0, 0.0, Color32::BLACK)],
        };
        for shadow in shadows {
            if view.behind.is_some() {
                shadow.paint_outside(&shadow_painter, drawer);
            } else {
                shadow.paint(&shadow_painter, drawer, CornerRadius::ZERO);
            }
        }
        if let Some(behind) = view.behind {
            behind.paint(&painter, drawer, BLUR);
            painter.rect_filled(drawer, 0.0, SEE_THROUGH);
        } else {
            painter.rect_filled(drawer, 0.0, palette.background_elevated_subdued);
        }
        // Clicks on the drawer itself don't reach the layer under it.
        let _ = ui.interact(drawer, id.with("panel"), Sense::click());

        // The pointer only moves the cursor when it really moved.
        let pointer = ctx.input(|input| input.pointer.hover_pos());
        let moved = pointer.is_some() && pointer != self.last_pointer;
        self.last_pointer = pointer;
        if moved && ui.rect_contains_pointer(drawer) {
            self.mousing = true;
        }
        // Leaving any way but over the rail closes it.
        let inside = pointer.is_some_and(|at| drawer.contains(at));
        if self.open && self.inside && !inside && pointer.is_some_and(|at| at.x > drawer.left()) {
            action = Some(Action::Close);
        }
        self.inside = inside;

        let left = drawer.left() + 3.5 * rem;
        let right = drawer.right() - 2.0 * rem;
        let list_area =
            Rect::from_min_max(pos2(drawer.left(), drawer.top() + SCROLL_TOP), drawer.max);
        self.viewport = list_area.height();

        // Lay the lines out, finding each one's top from the top of the scroll content.
        let mut layout = Vec::with_capacity(lines.len() + 1);
        let mut top = 0.0;
        let mut previous_nested = false;
        let folders = lines
            .iter()
            .take_while(|line| matches!(line, Line::Folder { .. }))
            .count();
        for (i, line) in lines.iter().enumerate() {
            if i == folders {
                top += DIVIDER;
            }
            let (height, pull) = match line {
                Line::Label {
                    name, nested: true, ..
                } => {
                    let width = right - (drawer.left() + 3.5 * rem * 1.75);
                    (wrapped_height(&ctx, name, width), NESTED_PULL)
                }
                _ if previous_nested => (ROW, -NESTED_PULL),
                _ => (ROW, 0.0),
            };
            top -= pull;
            layout.push((top, height));
            top += height;
            previous_nested = matches!(line, Line::Label { nested: true, .. });
        }
        let content_height = top;

        // Scrolling: the wheel, and the cursor kept in view as it moves.
        if ui.rect_contains_pointer(list_area) {
            self.scroll -= ctx.input(|input| input.smooth_scroll_delta.y);
        }
        let item_rows: Vec<(f32, f32)> = lines
            .iter()
            .zip(&layout)
            .filter(|(line, _)| line.is_item())
            .map(|(_, row)| *row)
            .collect();
        if std::mem::take(&mut self.reveal)
            && let Some(&(row_top, height)) =
                self.cursor.checked_sub(1).and_then(|at| item_rows.get(at))
        {
            if row_top < self.scroll {
                self.scroll = row_top;
            } else if row_top + height > self.scroll + self.viewport {
                self.scroll = row_top + height - self.viewport;
            }
        }
        self.scroll = self
            .scroll
            .clamp(0.0, (content_height - self.viewport).max(0.0));
        let origin = list_area.top() - self.scroll;

        // The list, under the account row.
        let list_painter = painter.with_clip_rect(list_area);
        let highlight = self
            .cursor
            .checked_sub(1)
            .and_then(|at| item_rows.get(at))
            .copied();
        paint_highlight(
            &list_painter,
            id_for(list_painter.ctx(), "list"),
            drawer,
            origin,
            highlight,
            self.jump,
            view.behind.is_some(),
        );
        let mut index = 0;
        for (i, (line, &(row_top, height))) in lines.iter().zip(&layout).enumerate() {
            if i == folders {
                let y = origin + row_top - DIVIDER + DIVIDER_LINE;
                list_painter.rect_filled(
                    exo_ui::pixel_snap(Rect::from_min_max(pos2(left, y), pos2(right, y + 1.0))),
                    0.0,
                    inks.divider,
                );
            }
            let row = Rect::from_min_size(
                pos2(drawer.left(), origin + row_top),
                vec2(drawer.width(), height),
            );
            if line.is_item() {
                index += 1;
            }
            if !row.intersects(list_area) {
                continue;
            }
            let item = line.is_item().then_some(index);
            let hovered = item.is_some()
                && self.open
                && pointer.is_some_and(|at| row.contains(at) && list_area.contains(at));
            if hovered
                && moved
                && let Some(item) = item
            {
                self.move_to(item, true);
            }
            let clicked = item.is_some()
                && ui
                    .interact(
                        row.intersect(list_area),
                        id.with(("folder", i)),
                        Sense::click(),
                    )
                    .clicked();
            if clicked && let Some(item) = item {
                action = self.choose(lines, item, true).or(action);
            }
            let text_top = row.top()
                + if matches!(line, Line::Label { nested: true, .. }) {
                    NESTED_PADDING
                } else {
                    ROW_PADDING_TOP
                }
                + 1.0;
            match line {
                Line::Folder { place, name, key } => {
                    let inbox = *place == Place::List(ListId::Inbox);
                    let name_width = LABEL.paint_line(
                        &list_painter,
                        pos2(left, text_top),
                        name,
                        inks.label,
                        Some(right - left),
                    );
                    if inbox && view.important_other {
                        paint_important_other(
                            &list_painter,
                            pos2(left + name_width, text_top),
                            &inks,
                        );
                    }
                    let hover = hovered && self.mousing;
                    let shortcut = motion::transition_in(
                        &ctx,
                        id.with(("shortcut", i)),
                        hover,
                        HOVER,
                        CubicBezier::EASE,
                    );
                    let tokens = key.map(|key| ["G", "then", key]);
                    let caps = Keycaps::new(
                        &ctx,
                        tokens.as_ref().map_or(&[][..], |tokens| &tokens[..]),
                        keycap_colors(&inks, shortcut),
                    );
                    // `float: right`, there even while transparent.
                    let shortcut_left = right - caps.width();
                    if shortcut > 0.0 && key.is_some() {
                        caps.paint(
                            &list_painter,
                            shortcut_left,
                            row.top() + ROW_PADDING_TOP - 1.0,
                        );
                    }
                    if inbox {
                        let slide = motion::transition(
                            &ctx,
                            id.with(("count", i)),
                            hover,
                            HOVER,
                            CubicBezier::EASE,
                        );
                        paint_count(
                            &list_painter,
                            inks.count,
                            row,
                            shortcut_left + COUNT_HIDDEN + (COUNT_SHOWN - COUNT_HIDDEN) * slide,
                            view.inbox,
                        );
                    }
                }
                Line::Label {
                    name, nested: true, ..
                } => {
                    let nested_left = drawer.left() + 3.5 * rem * 1.75;
                    let mut job =
                        LayoutJob::single_section(name.clone(), LABEL.format(inks.nested));
                    job.wrap.max_width = right - nested_left;
                    paint_job(&list_painter, pos2(nested_left, text_top), job, &LABEL);
                }
                Line::Label { name, .. } | Line::Header(name) => {
                    LABEL.paint_line(
                        &list_painter,
                        pos2(left, text_top),
                        name,
                        inks.label,
                        Some(right - left),
                    );
                }
                Line::LoadMore => {
                    LABEL.paint_line(
                        &list_painter,
                        pos2(left, text_top),
                        "Load More",
                        palette.vbar_anchor_default,
                        None,
                    );
                }
            }
        }

        // The account, pinned above the list, and the shadow under it once the list scrolls.
        let account = Rect::from_min_size(
            pos2(drawer.left(), drawer.top() + ACCOUNT_TOP),
            vec2(drawer.width(), ROW),
        );
        paint_highlight(
            &painter,
            id_for(painter.ctx(), "account"),
            drawer,
            0.0,
            (self.cursor == 0).then_some((account.top(), ROW)),
            self.jump,
            view.behind.is_some(),
        );
        if self.open && moved && pointer.is_some_and(|at| account.contains(at)) {
            self.move_to(0, true);
        }
        if ui
            .interact(account, id.with("account"), Sense::click())
            .on_hover_cursor(egui::CursorIcon::PointingHand)
            .clicked()
        {
            action = Some(Action::SwitchAccounts);
        }
        paint_account(ui, account, left, right, view, &inks);
        exo_ui::scroll_shadow(
            &painter,
            id.with("shadow"),
            pos2(drawer.left(), list_area.top()),
            drawer.width(),
            self.scroll > 0.0,
        );

        if shown < 1.0 {
            ctx.request_repaint();
        }
        action
    }
}

fn id_for(ctx: &egui::Context, highlight: &str) -> Id {
    exo_ui::id(ctx, ("exo::folders::highlight", highlight))
}

/// A nested label's height: its wrapped lines with 12px above and below.
fn wrapped_height(ctx: &egui::Context, name: &str, width: f32) -> f32 {
    let mut job = LayoutJob::single_section(name.to_owned(), LABEL.format(Color32::PLACEHOLDER));
    job.wrap.max_width = width;
    let rows = ctx
        .fonts_mut(|fonts| fonts.layout_job(job))
        .rows
        .len()
        .max(1);
    rows as f32 * LABEL.line_height + 2.0 * NESTED_PADDING
}

/// The cursor's highlight on the row at `target` (its top and height in content that starts at
/// `origin` on screen, so scrolling doesn't slide it), sliding via [`ListHighlight`] and fading
/// out over [`HIGHLIGHT_FADE`] when the cursor leaves this section.
fn paint_highlight(
    painter: &Painter,
    id: Id,
    drawer: Rect,
    origin: f32,
    target: Option<(f32, f32)>,
    jump: bool,
    photo: bool,
) {
    let ctx = painter.ctx();
    let opacity = motion::transition(
        ctx,
        id.with("shown"),
        target.is_some(),
        if jump { 0.0 } else { HIGHLIGHT_FADE },
        HIGHLIGHT_CURVE,
    );
    let last_id = id.with("last");
    let shown_target = match target {
        Some((top, height)) => {
            let target = HighlightTarget { top, height };
            ctx.data_mut(|data| data.insert_temp(last_id, target));
            Some(target)
        }
        None if opacity > 0.0 => ctx.data(|data| data.get_temp::<HighlightTarget>(last_id)),
        None => {
            ctx.data_mut(|data| data.remove::<HighlightTarget>(last_id));
            None
        }
    };
    let origin = pos2(drawer.left() + HIGHLIGHT_INSET, origin);
    let width = drawer.width() - 2.0 * HIGHLIGHT_INSET;
    if opacity <= 0.0 || shown_target.is_none() {
        let _ =
            ListHighlight::new(id).paint(painter, origin, width, None, HighlightState::default());
        return;
    }
    let mut bar_painter = painter.clone();
    bar_painter.multiply_opacity(opacity);
    let mut bg_painter = bar_painter.clone();
    let paint = |bg: &Painter| {
        ListHighlight::new(id).paint(
            bg,
            origin,
            width,
            shown_target,
            HighlightState {
                focused: true,
                jump,
                ..HighlightState::default()
            },
        )
    };
    // Over Inbox Zero's photo the drawer is translucent dark with white text, so keep the
    // highlight dark and subtle rather than painting Snow's light tint behind white text.
    let bar = if photo {
        bg_painter.multiply_opacity(0.25);
        exo_ui::with_theme(ctx, Theme::Carbon, || paint(&bg_painter))
    } else {
        paint(&bg_painter)
    };
    bar.paint(&bar_painter);
}

/// The Inbox row's `Important • Other` after its name: 12px after it, its dot with 1px either
/// side.
fn paint_important_other(painter: &Painter, at: Pos2, inks: &Inks) {
    let (color, dot) = (
        IMPORTANT_OTHER.format(inks.important),
        IMPORTANT_OTHER.format(inks.dot),
    );
    // The dot's leading space collapses into the one before it, and its trailing one swallows
    // `Other`'s. Its margins go either side.
    let mut job = LayoutJob::default();
    job.append("Important ", 0.0, color.clone());
    job.append("• ", 1.0, dot);
    job.append("Other", 1.0, color);
    paint_job(painter, pos2(at.x + 12.0, at.y), job, &IMPORTANT_OTHER);
}

/// A folder's `G then {key}` caps, `opacity` of the way faded in.
fn keycap_colors(inks: &Inks, opacity: f32) -> KeycapColors {
    KeycapColors {
        background: inks.keycaps.background.gamma_multiply(opacity),
        text: inks.keycaps.text.gamma_multiply(opacity),
        divider: inks.keycaps.divider.gamma_multiply(opacity),
    }
}

/// The Inbox's count, its right edge at `right`. That's 75px past the keys' left edge, or clear
/// of them by 8px while hovered. The row clips it.
fn paint_count(painter: &Painter, color: Color32, row: Rect, right: f32, count: Option<ListCount>) {
    let Some(count) = count.filter(|count| (1..MAX_COUNT).contains(&count.threads)) else {
        return;
    };
    let text = if count.exact {
        count.threads.to_string()
    } else {
        format!("{}+", count.threads)
    };
    let width = LABEL.single_line(painter, &text, color, None).size().x;
    let clip = painter.with_clip_rect(painter.clip_rect().intersect(row));
    LABEL.paint_line(
        &clip,
        pos2(right - width, row.top() + ROW_PADDING_TOP + 1.0),
        &text,
        color,
        None,
    );
}

/// The account row: avatar, address and the switch-account caret.
fn paint_account(ui: &Ui, row: Rect, left: f32, right: f32, view: &View<'_>, inks: &Inks) {
    let (painter, ctx) = (ui.painter(), ui.ctx());
    let palette = exo_ui::palette(ctx);
    // The avatar, initial or photo, on whole pixels.
    let avatar = exo_ui::pixel_snap(Rect::from_min_size(
        pos2(left, row.center().y - AVATAR / 2.0),
        Vec2::splat(AVATAR),
    ));
    match crate::profile_pictures::photo(ctx, view.email) {
        // A 3px ring in the account's color.
        Some(photo) => exo_ui::paint_photo_avatar(painter, avatar, &photo, 3.0, view.color, 1.0),
        // The initial: `#fff`, `#27292d` in Carbon; `dy: .35em`.
        None => exo_ui::paint_initial_avatar(
            painter,
            avatar,
            view.color,
            &crate::accounts::initial(view.email),
            &INITIAL,
            palette.text_base_inverse,
            0.35 * 18.0,
        ),
    }
    // The caret: 6×11 at the row's right edge, centered and raised half a pixel.
    let caret = Rect::from_min_size(pos2(right - 6.0, row.center().y - 5.75), vec2(6.0, 11.0));
    let text_left = left + AVATAR + AVATAR_GAP;
    LABEL.paint_line(
        painter,
        pos2(text_left, row.center().y - 8.0),
        view.email,
        inks.label,
        Some((caret.left() - 4.0 - text_left).max(0.0)),
    );
    let name = "legacy/DEPRECATED_rightCaret";
    exo_ui::paint_icon(
        ui,
        name,
        exo_ui::icon_rect(ui.ctx(), name, caret),
        inks.caret,
    );
}

/// The drawer's colors, which turn white over Inbox Zero's photo.
struct Inks {
    /// A row's label and the account's address.
    label: Color32,
    /// A nested label's name.
    nested: Color32,
    /// The Inbox row's `Important • Other`, and its dot.
    important: Color32,
    dot: Color32,
    /// The Inbox's count.
    count: Color32,
    /// The line between the folders and the labels.
    divider: Color32,
    /// The account's caret.
    caret: Color32,
    /// A folder's `G then {key}` caps.
    keycaps: KeycapColors,
}

impl Inks {
    fn new(palette: &Palette, photo: bool) -> Self {
        let white = |alpha: f32| Color32::from_white_alpha((alpha * 255.0).round() as u8);
        if photo {
            return Self {
                label: white(0.9),
                nested: white(0.6),
                important: white(0.55),
                dot: white(0.55),
                count: white(0.4),
                divider: white(0.09),
                caret: white(0.4),
                keycaps: KeycapColors {
                    background: white(0.15),
                    text: white(0.9),
                    divider: white(0.4),
                },
            };
        }
        Self {
            // Black or white at .75; a nested label's at .5.
            label: palette.ink(0.75, 0.75),
            nested: palette.ink(0.5, 0.5),
            // `Important • Other` at .5, its dot at .25.
            important: palette.ink(0.5, 0.5),
            dot: palette.ink(0.25, 0.25),
            // Black at .35, white at .45.
            count: palette.ink(0.35, 0.45),
            // #ededed, #5f6165 in Carbon.
            divider: match palette.theme {
                Theme::Snow => Color32::from_rgb(0xed, 0xed, 0xed),
                Theme::Carbon => Color32::from_rgb(0x5f, 0x61, 0x65),
            },
            caret: palette.icon_base_default,
            // The caps' `then` is in the subdued text color.
            keycaps: KeycapColors {
                divider: palette.text_base_subdued,
                ..KeycapColors::base(palette)
            },
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use exo_store::LabelKind;

    fn label(id: &str, name: &str) -> Label {
        Label {
            id: id.into(),
            name: name.into(),
            kind: LabelKind::User,
            text_color: None,
            background_color: None,
            hidden: false,
        }
    }

    fn names(lines: &[Line]) -> Vec<String> {
        lines
            .iter()
            .map(|line| match line {
                Line::Folder { name, .. } => (*name).to_owned(),
                Line::Label {
                    name, nested: true, ..
                } => format!("  {name}"),
                Line::Label { name, .. } => name.clone(),
                Line::Header(name) => format!("# {name}"),
                Line::LoadMore => "Load More".into(),
            })
            .collect()
    }

    #[test]
    fn folders_then_labels_nested_under_their_parents() {
        let labels = [
            label("L1", "Projects/Atlas"),
            label("L2", "[News]"),
            label("L3", "Clients"),
            label("L4", "Clients/Calloway"),
            label("L5", "Input/Needs Action"),
            label("L6", "[Superhuman]/AI/Pitch"),
            label("L7", "[Exo]/Muted"),
        ];
        assert_eq!(
            names(&lines(&labels, false, false)),
            [
                "Inbox",
                "Starred",
                "Drafts",
                "Sent",
                "Done",
                "Scheduled",
                "Reminders",
                "Snippets",
                "Spam",
                "Trash",
                "Clients",
                "  Calloway",
                "# Input",
                "  Needs Action",
                "# Projects",
                "  Atlas"
            ]
        );
        assert!(
            names(&lines(&labels, true, false)).contains(&"Muted".to_owned()),
            "once mute is used"
        );
    }

    #[test]
    fn long_label_lists_load_more() {
        let many: Vec<Label> = (0..120)
            .map(|i| label(&format!("L{i}"), &format!("Label {i}")))
            .collect();
        let short = lines(&many, false, false);
        assert_eq!(short.last(), Some(&Line::LoadMore));
        assert_eq!(
            short
                .iter()
                .filter(|line| matches!(line, Line::Label { .. }))
                .count(),
            100
        );
        assert_eq!(
            lines(&many[..105], false, false)
                .iter()
                .filter(|line| matches!(line, Line::Label { .. }))
                .count(),
            105,
            "fewer than ten more show at once"
        );

        let mut folders = Folders::default();
        folders.open(&short, &Place::List(ListId::Inbox));
        folders.key(Key::Bottom, &short);
        assert_eq!(folders.key(Key::Enter, &short), None);
        assert!(folders.all());
    }

    #[test]
    fn opens_on_the_current_view() {
        let labels = [label("L1", "Clients"), label("L2", "Clients/Calloway")];
        let lines = lines(&labels, false, false);
        let mut folders = Folders::default();
        folders.open(&lines, &Place::List(ListId::Important));
        assert_eq!(folders.cursor, 1, "the inbox views open on Inbox");
        folders.open(&lines, &Place::List(ListId::Label("L2".into())));
        assert_eq!(folders.cursor, 12);
        assert_eq!(
            folders.key(Key::Enter, &lines),
            Some(Action::Show(Place::List(ListId::Label("L2".into())), None))
        );
        folders.open(&lines, &Place::Snippets);
        assert_eq!(folders.cursor, 8, "the Snippets folder opens on Snippets");
        assert_eq!(
            folders.key(Key::Enter, &lines),
            Some(Action::Show(Place::Snippets, None))
        );
        folders.open(&lines, &Place::List(ListId::All));
        assert_eq!(folders.cursor, 1, "All Mail isn't listed");
    }

    #[test]
    fn keys_walk_the_account_folders_and_labels() {
        let lines = lines(&[label("L1", "Family")], false, false);
        let mut folders = Folders::default();
        folders.open(&lines, &Place::List(ListId::Inbox));
        folders.key(Key::UpWithin, &lines);
        assert_eq!(folders.cursor, 1, "K stops at the first folder");
        folders.key(Key::Up, &lines);
        assert_eq!(folders.cursor, 0, "↑ reaches the account");
        assert_eq!(
            folders.key(Key::Right, &lines),
            Some(Action::SwitchAccounts),
            "→ on the account opens the Accounts palette"
        );
        assert_eq!(
            folders.key(Key::Enter, &lines),
            Some(Action::SwitchAccounts),
            "and so does Enter"
        );
        folders.key(Key::Down, &lines);
        folders.key(Key::Down, &lines);
        assert_eq!(
            folders.key(Key::Enter, &lines),
            Some(Action::Show(Place::List(ListId::Starred), None))
        );
        folders.key(Key::Bottom, &lines);
        assert_eq!(folders.cursor, 11);
        folders.key(Key::Down, &lines);
        assert_eq!(folders.cursor, 11, "stops at the last label");
        folders.key(Key::Top, &lines);
        assert_eq!(folders.cursor, 1);
        assert_eq!(folders.key(Key::Right, &lines), Some(Action::Close));
        assert_eq!(folders.key(Key::Close, &lines), Some(Action::Close));
    }

    #[test]
    fn clicks_hint_at_the_keyboard() {
        let lines = lines(&[label("L1", "Family")], false, false);
        let mut folders = Folders::default();
        assert_eq!(
            folders.choose(&lines, 4, true),
            Some(Action::Show(
                Place::List(ListId::Sent),
                Some(Hint::GoTo {
                    key: "T",
                    name: "Sent"
                })
            ))
        );
        assert_eq!(
            folders.choose(&lines, 6, true),
            Some(Action::Show(Place::List(ListId::Scheduled), None)),
            "Scheduled has no key to hint at"
        );
        assert_eq!(
            folders.choose(&lines, 7, true),
            Some(Action::Show(
                Place::List(ListId::Reminders),
                Some(Hint::GoTo {
                    key: "H",
                    name: "Reminders"
                })
            ))
        );
        assert_eq!(
            folders.choose(&lines, 8, true),
            Some(Action::Show(
                Place::Snippets,
                Some(Hint::GoTo {
                    key: ";",
                    name: "Snippets"
                })
            ))
        );
        assert_eq!(
            folders.choose(&lines, 11, true),
            Some(Action::Show(
                Place::List(ListId::Label("L1".into())),
                Some(Hint::Label)
            ))
        );
    }
}

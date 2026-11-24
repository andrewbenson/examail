//! Share Availability from a draft. ⌘⇧A and the footer's calendar open a menu at the caret. Its
//! one row, `Insert free times`, opens the week to pick times, and Insert writes them where the
//! draft's caret was, or into a new Reply All or message.

use egui::{Color32, Id, Rect, Ui, pos2, vec2};
use exo_editor::{Editor, Pos, Selection};
use exo_store::{Address, DraftAction};
use exo_ui::{AnchoredCard, AnchoredList, Family, Font, TextStyle, Theme};
use jiff::Timestamp;

use super::{Fields, PendingReply, draft::Open, factory};
use crate::calendar::Target;

/// The menu's one row, after a separator (Exo has no booking pages, which need a server).
pub(super) const ROW: &str = "Insert free times";
/// Typing this many characters past the last one that matched a row closes the menu.
const MISSES: usize = 5;

/// The open menu. What's typed since it opened filters its rows.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct Menu {
    /// Where the caret was when it opened.
    pub start: Pos,
    /// What's typed since, the query.
    pub query: String,
    /// The caret's box when it opened, once drawn.
    pub anchor: Option<Rect>,
    /// Characters typed since the query last matched a row.
    misses: usize,
}

impl Menu {
    /// The menu opening at `start`.
    pub fn at(start: Pos) -> Self {
        Self {
            start,
            query: String::new(),
            anchor: None,
            misses: 0,
        }
    }

    /// Whether `Insert free times` matches the query (`command-score` above zero), as it does
    /// an empty one.
    pub fn shows_row(&self) -> bool {
        self.query.is_empty() || exo_commands::command_score(ROW, &self.query) > 0.0
    }

    /// Where the query ends.
    pub fn end(&self) -> Pos {
        Pos::new(
            self.start.block,
            self.start.offset + self.query.chars().count(),
        )
    }
}

/// Follows the editor this frame, taking what was typed at the caret as the query. Returns
/// whether the menu stays open. It closes once the caret leaves the query or something is
/// selected, or five characters past the last one that matched a row.
pub(super) fn follow(editor: &Editor, menu: &mut Menu, changed: bool) -> bool {
    let selection = editor.selection();
    let caret = selection.focus;
    if !selection.is_collapsed()
        || caret.block != menu.start.block
        || caret.offset < menu.start.offset
    {
        return false;
    }
    if !changed {
        // The caret moving inside the query keeps it.
        return caret.offset <= menu.end().offset;
    }
    let text: String = editor.document().blocks[caret.block]
        .text()
        .chars()
        .skip(menu.start.offset)
        .take(caret.offset - menu.start.offset)
        .collect();
    let grew = text
        .chars()
        .count()
        .saturating_sub(menu.query.chars().count());
    menu.query = text;
    if menu.shows_row() {
        menu.misses = 0;
    } else {
        menu.misses += grew;
    }
    menu.misses < MISSES
}

/// The row's text: 13px/24px, in 32px rows padded `4px 12px`. The box is 272px wide, with
/// `padding-top: 4px`.
const TEXT: TextStyle = TextStyle::new(Font::new(Family::Sans, 400), 13.0, 24.0);
const WIDTH: f32 = 272.0;

/// The menu under the caret at `caret`, with the row's text starting where the caret is. Returns
/// whether its row was clicked, and where the menu is.
pub(super) fn show(ui: &Ui, caret: Rect, id: Id) -> (bool, Rect) {
    let ctx = ui.ctx().clone();
    let palette = exo_ui::palette(&ctx);
    let card = Rect::from_min_size(
        pos2(caret.left() - 12.0, caret.bottom() + 4.0),
        vec2(WIDTH, 4.0 + 1.0 + 32.0),
    );
    // The menu: white, Carbon `#27292d`; the separator `#ededed`, Carbon
    // `#5f6165`; the highlighted row `hsl(220, 45%, 97%)`, Carbon `#464b53`.
    let (fill, separator, selected, text) = match palette.theme {
        Theme::Snow => (
            Color32::WHITE,
            Color32::from_rgb(0xed, 0xed, 0xed),
            Color32::from_rgb(0xf4, 0xf6, 0xfb),
            Color32::from_black_alpha(217),
        ),
        Theme::Carbon => (
            Color32::from_rgb(0x27, 0x29, 0x2d),
            Color32::from_rgb(0x5f, 0x61, 0x65),
            Color32::from_rgb(0x46, 0x4b, 0x53),
            Color32::WHITE,
        ),
    };
    let list = AnchoredList::new(id, card, AnchoredCard::tooltip(fill, selected))
        .order(egui::Order::Foreground)
        .padding(4.0, 0.0)
        .highlighted(Some(1))
        .show_variable(
            &ctx,
            &[1.0, 32.0],
            |index| index == 1,
            |painter, row, index| {
                if index == 0 {
                    painter.rect_filled(row, 0.0, separator);
                } else {
                    TEXT.paint_line(
                        painter,
                        pos2(row.left() + 12.0, row.top() + 4.0),
                        ROW,
                        text,
                        None,
                    );
                }
            },
        );
    (list.clicked == Some(1), list.rect)
}

/// Writes `html` into `open` where its caret is. When the body is empty, `html` becomes the whole
/// body and the caret goes to its end. Otherwise `html` goes after a line break, unless the
/// caret's line is empty, and gets a line break after it.
pub(super) fn insert_at_caret(open: &mut Open, html: &str) {
    insert(&mut open.editor, html);
    open.touch(0.0);
}

fn insert(editor: &mut Editor, html: &str) {
    if editor.is_empty() {
        editor.set_html(html);
        let end = editor.document().end();
        editor.set_selection(Selection::caret(end));
    } else {
        let caret = editor.selection().focus;
        let empty_line = editor
            .document()
            .blocks
            .get(caret.block)
            .is_some_and(|block| block.is_empty());
        editor.insert_html(&format!(
            "{} {html} <br>",
            if empty_line { "" } else { "<br> " }
        ));
    }
}

impl crate::Mail {
    /// The From and recipients of what `target` replies with. They're the draft's, or those of a
    /// Reply All to the open conversation's last message.
    pub(crate) fn compose_reply_fields(&self, target: &Target) -> (Option<Address>, Vec<Address>) {
        match target {
            Target::Draft(id) => self
                .compose
                .drafts
                .iter()
                .find(|open| open.id == *id)
                .map_or((None, Vec::new()), |open| {
                    (
                        open.record.from.clone(),
                        open.record
                            .to
                            .iter()
                            .chain(&open.record.cc)
                            .cloned()
                            .collect(),
                    )
                }),
            Target::Reply(thread) => {
                let detail = self
                    .thread
                    .as_ref()
                    .filter(|view| view.id() == thread)
                    .and_then(crate::thread::ThreadView::detail);
                let Some(message) = detail
                    .and_then(|detail| detail.messages.iter().rfind(|message| !message.is_draft()))
                else {
                    return (None, Vec::new());
                };
                let record = factory::reply(
                    &self.compose_account(),
                    DraftAction::ReplyAll,
                    message,
                    message.has_label("SENT"),
                    &factory::QuotedBody::default(),
                    Timestamp::now(),
                );
                (
                    record.from,
                    record.to.into_iter().chain(record.cc).collect(),
                )
            }
            Target::New => (None, Vec::new()),
        }
    }

    /// The last draft open in compose in `thread`.
    pub(crate) fn compose_thread_draft(&self, thread: &str) -> Option<String> {
        self.compose
            .drafts
            .iter()
            .rfind(|open| open.thread.as_deref() == Some(thread))
            .map(|open| open.id.clone())
    }

    /// Share Availability's Insert. Writes `html` into the draft, a new Reply All in the
    /// conversation, or a new message, and the draft then has the keyboard.
    pub(crate) fn compose_insert_availability(&mut self, target: &Target, html: &str) {
        match target {
            Target::Draft(id) if self.compose.drafts.iter().any(|open| open.id == *id) => {
                if let Some(open) = self.compose.get_mut(id) {
                    insert_at_caret(open, html);
                    open.state.field = super::Field::Body;
                }
                self.compose.focus = Some(id.clone());
            }
            Target::Reply(thread) => {
                if self.thread.as_ref().is_none_or(|view| view.id() != thread) {
                    self.open_thread(thread.clone());
                }
                self.compose.pending = Some(PendingReply {
                    thread: thread.clone(),
                    action: DraftAction::ReplyAll,
                    field: None,
                    pop_out: false,
                    body: Some(html.to_owned()),
                    quote: None,
                    snippet: None,
                });
                self.create_pending_reply();
            }
            _ => self.compose_new(
                Fields {
                    body: html.to_owned(),
                    ..Fields::default()
                },
                None,
            ),
        }
    }

    /// ⌘⇧A or the footer's calendar in the focused draft. Opens the draft's menu at the caret, or
    /// closes it and keeps what was typed into it.
    pub(super) fn compose_availability_menu(&mut self, open: bool) {
        if let Some(draft) = self.compose.focused_mut() {
            if open {
                draft.state.field = super::Field::Body;
                if draft.popups.availability.is_none() {
                    draft.popups.availability = Some(Menu::at(draft.editor.selection().focus));
                }
            } else {
                draft.popups.availability = None;
            }
        }
    }

    /// Whether a reply's calendar icon may show `You can now hit ⌘ shift A…`. It may when Exo is
    /// online, the tip wasn't dismissed, no hint tooltip is up, the palette doesn't have the
    /// keyboard, and the account joined over seven days ago. The form checks the draft and its
    /// other icons.
    pub(super) fn availability_tip_due(&self) -> bool {
        let week_ago = self.now.checked_sub(jiff::Span::new().days(7)).ok();
        let joined = self
            .joined()
            .zip(week_ago)
            .is_some_and(|(joined, week_ago)| joined < week_ago.timestamp());
        joined
            && self.snapshot.connection.is_online()
            && !self.account.settings().has_shown_share_availability_tooltip
            && self.commands.hint_text().is_none()
            && !self.compose.palette_open
    }

    /// Whether `You can now hit ⌘ shift A…` may show on a reply this frame (for tests).
    pub fn compose_availability_tip(&self) -> bool {
        self.compose.availability_tip
    }

    /// The focused draft's Share Availability menu, when it's open: what's typed into it, and
    /// whether its row shows (for tests).
    pub fn compose_availability_query(&self) -> Option<(String, bool)> {
        let menu = self.compose.focused()?.popups.availability.as_ref()?;
        Some((menu.query.clone(), menu.shows_row()))
    }

    /// A press outside the menu closes it, and the query typed into it goes.
    pub(super) fn compose_availability_dismiss(&mut self) {
        if let Some(draft) = self.compose.focused_mut()
            && let Some(menu) = draft.popups.availability.take()
            && !menu.query.is_empty()
        {
            draft.editor.replace(menu.start, menu.end(), "");
        }
    }

    /// The menu's `Insert free times`. Removes the query typed into the menu and opens the week to
    /// pick times for the focused draft.
    pub(super) fn compose_availability_pick(&mut self) {
        self.compose_availability_dismiss();
        let Some(draft) = self.compose.focused_mut() else {
            return;
        };
        let id = draft.id.clone();
        let now = self.now.clone();
        self.calendar_share(Target::Draft(id), &now);
    }
}

#[cfg(test)]
mod tests {
    use exo_editor::Pos;

    use super::*;

    #[test]
    fn an_empty_body_becomes_the_text() {
        let mut editor = Editor::new();
        insert(&mut editor, "Does  Fri Nov 20 work? <br>");
        assert!(editor.html().contains("Does"), "{}", editor.html());
        assert_eq!(editor.selection().focus, editor.document().end());
    }

    #[test]
    fn text_goes_in_on_a_line_of_its_own() {
        let mut editor = Editor::from_html("<div>Hi Tom,</div>");
        editor.set_selection(Selection::caret(Pos::new(0, 7)));
        insert(
            &mut editor,
            "Does  Fri Nov 20 work?<br>- tomorrow (Nov 18): 1:00–1:30 pm <br>",
        );
        // `<br>`s break lines in the caret's block rather than starting blocks, and spaces
        // collapse.
        let blocks: Vec<String> = editor
            .document()
            .blocks
            .iter()
            .map(|block| block.text())
            .collect();
        assert_eq!(
            blocks,
            ["Hi Tom,\nDoes Fri Nov 20 work?\n- tomorrow (Nov 18): 1:00–1:30 pm\n"]
        );
    }
}

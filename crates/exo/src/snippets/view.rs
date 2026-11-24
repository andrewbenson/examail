//! A snippet opened from the folder: the thread pane titled `Edit Private Snippet`, the
//! snippet's name over a card that holds its recipients, subject, body (rendered as a message's
//! is, by a body renderer of its own) and files (a message's cards), and a footer with `Edit` and
//! Discard.

use egui::{Rect, Ui, pos2};
use exo_commands::Region;
use exo_store::{MessageDetail, Snippet};
use exo_ui::{Family, Font, TextStyle};

use super::Action;
use crate::{
    body::{Bodies, Look},
    compose::{
        FOOTER_ACTION_DOWN, FOOTER_ACTION_HEIGHT, ReadOnlyHeader, SNIPPET_NAME_ROOM,
        discard_button, draft_attachments_to_parts, paint_rule, submit_button, synthetic_message,
    },
};

/// The snippet's name: 14px/20px, weight 600, `top: 3px`.
const NAME: TextStyle = TextStyle::new(Font::new(Family::Sans, 600), 14.0, 20.0);
/// The name's line, above the card's top (`margin: -40px 0 20px`, `top: -1px`, the name's 3px).
const NAME_ABOVE: f32 = 26.0;
/// The view's `padding-top: 16px`. The recipients' header starts 4px higher.
const SPACING_TOP: f32 = 16.0;
const HEADER_TOP: f32 = 12.0;
/// The subject's `padding-top: 12px` under recipients, and its `margin-bottom: 8px`, which the
/// body's `margin-top: 20px` swallows.
const SUBJECT_SPACING: f32 = 12.0;
const BODY_MARGIN_TOP: f32 = 20.0;
/// What the body is taken to be until laid out: one line.
const BODY_PLACEHOLDER: f32 = 24.0;
/// The footer: 16px under the body, a 1px rule, `Edit` 20px down, 36px tall, then the card's
/// 20px.
const FOOTER_MARGIN: f32 = 16.0;
const FOOTER_BOTTOM: f32 = 20.0;

/// The view's own body renderer, made when a snippet opens and dropped when the view closes,
/// and what its file cards show.
#[derive(Default)]
pub(crate) struct Preview {
    bodies: Option<Bodies>,
    files: crate::thread::attachments::Files,
}

impl Preview {
    /// Whether nothing is being laid out or painted.
    pub(crate) fn is_idle(&self) -> bool {
        self.bodies.as_ref().is_none_or(Bodies::is_idle)
    }
}

/// The renderer's id for snippet `id`'s body.
fn body_id(id: &str) -> String {
    format!("snippet:{id}")
}

/// `snippet` as a message, for the body renderer and the recipients' header.
fn message(snippet: &Snippet) -> MessageDetail {
    let id = body_id(&snippet.id);
    let mut detail = synthetic_message(
        id.clone(),
        [&snippet.to, &snippet.cc, &snippet.bcc],
        &[],
        &snippet.subject,
        &snippet.body,
    );
    detail.attachments = draft_attachments_to_parts(&id, &snippet.attachments);
    detail
}

/// Where the card's parts sit, from the content's top (the card's plus `message::PADDING_TOP`).
struct Parts {
    header: ReadOnlyHeader,
    /// The file cards' list, under the body.
    files_top: f32,
    footer_top: f32,
    /// The content's height, to the card's bottom less its own 4px.
    height: f32,
}

/// The card's layout for `message` with a body `body` tall, `width` wide.
fn parts(ui: &Ui, message: &MessageDetail, body: f32, width: f32) -> Parts {
    // The content starts the card's own padding down, and the view's spacing replaces that
    // padding.
    let padding = crate::thread::CARD_PADDING_TOP;
    let header = ReadOnlyHeader::layout(
        ui,
        message,
        width,
        HEADER_TOP - padding,
        SPACING_TOP - padding,
        SUBJECT_SPACING,
        BODY_MARGIN_TOP,
    );
    let files_top = header.body_top() + body;
    let files = crate::thread::attachments::height(
        crate::thread::attachments::listed(message).count(),
        width,
    );
    let footer_top = files_top + files + FOOTER_MARGIN;
    let height =
        footer_top + FOOTER_ACTION_HEIGHT + FOOTER_BOTTOM - crate::thread::CARD_PADDING_BOTTOM;
    Parts {
        header,
        files_top,
        footer_top,
        height,
    }
}

impl crate::Mail {
    /// Enter or a click on a row opens the snippet read-only.
    pub(crate) fn open_snippet_view(&mut self, id: &str) {
        self.leave_pane();
        self.snippets.folder.view(id);
    }

    /// Back from the view to the folder. The view's renderer goes too, because nothing would take
    /// what it's still working on.
    pub(crate) fn close_snippet_view(&mut self) {
        self.snippets.folder.close_view();
        self.snippets.preview = Preview::default();
    }

    /// The snippet being viewed, while it's still kept.
    fn viewed_snippet(&self) -> Option<Snippet> {
        let id = self.snippets.folder.viewing()?;
        self.account
            .settings()
            .snippets
            .iter()
            .find(|snippet| snippet.id == id)
            .cloned()
    }

    /// A card in the view opens snippet file `uuid` (in the preview, or as a download).
    pub(crate) fn open_snippet_file(&mut self, uuid: &str, preview: bool) {
        if let Some(file) = self.viewed_snippet().and_then(|snippet| {
            snippet
                .attachments
                .into_iter()
                .find(|file| file.uuid == uuid)
        }) {
            self.open_draft_attachment(file, preview);
        }
    }

    /// A card in the view dragged out from `at`, which drags out snippet file `uuid`.
    pub(crate) fn drag_snippet_file(&mut self, uuid: &str, at: egui::Pos2) {
        let folder = self.account.draft_files().to_owned();
        if let Some(part) = self.viewed_snippet().and_then(|snippet| {
            snippet
                .attachments
                .iter()
                .find(|file| file.uuid == uuid)
                .and_then(|file| crate::attachment_files::Part::of_draft(file, &folder))
        }) {
            let zoom = self.ctx.zoom_factor();
            self.attachment_files
                .drag(&self.account.loader(), part, at, zoom);
        }
    }

    /// The view's `Download all`, which downloads the snippet's files in one zip named for its
    /// subject.
    pub(crate) fn download_snippet_files(&mut self) {
        let Some(snippet) = self.viewed_snippet() else {
            return;
        };
        let folder = self.account.draft_files().to_owned();
        self.attachment_files.download_snippet_files(
            &self.account.loader(),
            &body_id(&snippet.id),
            &snippet.subject,
            &snippet.attachments,
            &folder,
            self.now.datetime(),
        );
    }

    /// Draws the snippet being viewed in `rect` (the content pane).
    pub(crate) fn show_snippet_view(
        &mut self,
        ui: &mut Ui,
        rect: Rect,
        rem: f32,
        window_width: f32,
        now: f64,
    ) {
        let Some(snippet) = self.viewed_snippet() else {
            // Discarded meanwhile.
            self.close_snippet_view();
            return;
        };
        let message = message(&snippet);
        let ctx = ui.ctx().clone();
        let popped_out = self.compose.popped().is_some();
        let width = crate::thread::content_width(rect, rem, window_width, popped_out);
        let scale = ctx.pixels_per_point();
        let theme = exo_ui::theme(&ctx);
        let left = crate::thread::content_left(rect, rem, window_width, popped_out);
        let look = Look {
            width,
            scale,
            theme,
            rem,
            // A snippet has no quote to fold.
            show_quoted: true,
            hide_original: false,
            phase: left - left.floor(),
            background: theme.palette().background_elevated_default.to_array(),
        };
        // The body is shown cleaned: whole, no dates, no color tags, so in Snow its text is the
        // pane's black rather than a message's 90%.
        let bodies = self
            .snippets
            .preview
            .bodies
            .get_or_insert_with(|| Bodies::new(&ctx).cleaned_only().unclassified());
        bodies.set_thread(&[&message], self.account.email(), &self.account.loader());
        bodies.poll(&ctx);
        let body = bodies.height(&message.id, look).unwrap_or(BODY_PLACEHOLDER);
        let parts = parts(ui, &message, body, width);
        let pane = crate::thread::PaneCard {
            title: "Edit Private Snippet",
            height: parts.height,
            above: SNIPPET_NAME_ROOM,
            pop_out_hint: false,
            read_only: true,
        };
        let focused = self.commands.region() == Region::ReadOnlySnippet;
        let name = crate::compose::snippets::name(&snippet).to_owned();
        let mut asked = Vec::new();
        self.attachment_files.update_files(
            &self.account.loader(),
            &mut self.snippets.preview.files,
            &message,
            &snippet.attachments,
            self.account.draft_files(),
        );
        let preview = &mut self.snippets.preview;
        let (actions, ()) = crate::thread::card_pane(
            ui,
            rect,
            rem,
            window_width,
            popped_out,
            &pane,
            |ui, content, column, card| {
                let palette = exo_ui::palette(ui.ctx());
                let painter = ui.painter().clone();
                let (left, top) = (content.left(), content.top());
                // The name over the card, `rgba(0,0,0,.8)` (white in Carbon).
                NAME.paint_line(
                    &painter,
                    pos2(left, card.top() - NAME_ABOVE),
                    if snippet.name.is_empty() { "" } else { &name },
                    palette.ink(0.8, 0.8),
                    Some(content.width()),
                );
                parts.header.paint(
                    &painter,
                    palette,
                    content.min,
                    content.width(),
                    &snippet.subject,
                );
                if let Some(bodies) = &mut preview.bodies {
                    bodies.paint(
                        &painter,
                        &message.id,
                        pos2(left, top + parts.header.body_top()),
                        ui.clip_rect(),
                        1.0,
                    );
                }
                match crate::thread::attachments::show(
                    ui,
                    content,
                    top + parts.files_top,
                    &message,
                    &preview.files,
                ) {
                    Some(crate::thread::attachments::Click::Open {
                        attachment,
                        preview,
                    }) => asked.push(Action::OpenFile {
                        uuid: attachment.part_id,
                        preview,
                    }),
                    Some(crate::thread::attachments::Click::DownloadAll { .. }) => {
                        asked.push(Action::DownloadFiles)
                    }
                    Some(crate::thread::attachments::Click::Drag { attachment, at }) => {
                        asked.push(Action::DragFile {
                            uuid: attachment.part_id,
                            at,
                        })
                    }
                    None => {}
                }
                // The footer: the rule, `Edit` (underlined while the view has the keyboard) and
                // Discard.
                let footer = top + parts.footer_top;
                paint_rule(ui, left, footer, content.width());
                let pane_top = footer + FOOTER_ACTION_DOWN;
                let (edit, _) = submit_button(
                    ui,
                    ui.id().with("snippet-view-edit"),
                    pos2(left, pane_top),
                    "Edit",
                    None,
                    focused,
                );
                if edit.clicked() {
                    asked.push(Action::Edit(Some(snippet.id.clone())));
                }
                let button = discard_button(
                    ui,
                    ui.id().with("snippet-view-discard"),
                    content.right(),
                    pane_top,
                    column,
                );
                if button.clicked() {
                    asked.push(Action::Discard);
                }
            },
        );
        for action in actions {
            match action {
                crate::thread::Action::Back => asked.push(Action::Back),
                crate::thread::Action::Navigate(delta) => asked.push(Action::Move(delta)),
                _ => {}
            }
        }
        for action in asked {
            self.snippets_run(action, now);
        }
    }
}

//! Clipboard paste, file drag-and-drop, inline image insertion, remote image loading, and draft
//! attachment opening for [`crate::Mail`].

use std::path::PathBuf;

use exo_commands::Region;

use super::{
    Alert, AlertThen, Compose, Field, Fields, factory, files, form, images, mailto, outgoing,
};

impl crate::Mail {
    /// The names of the focused draft's files, in order (for tests).
    pub fn compose_attachments(&self) -> Vec<String> {
        self.compose
            .focused()
            .map(|open| {
                form::listed(open)
                    .map(|attachment| attachment.name.clone())
                    .collect()
            })
            .unwrap_or_default()
    }

    /// The focused draft's files on their way to Gmail, by name (for tests).
    pub fn compose_uploading(&self) -> Vec<String> {
        self.compose
            .focused()
            .map(|open| {
                form::listed(open)
                    .filter(|attachment| form::uploading(open, attachment).is_some())
                    .map(|attachment| attachment.name.clone())
                    .collect()
            })
            .unwrap_or_default()
    }

    /// ⌘V with nothing editable focused makes a draft of the clipboard.
    pub(crate) fn compose_paste_events(&mut self, ctx: &egui::Context) {
        self.compose_file_events(ctx);
        if !matches!(
            self.commands.region(),
            Region::ThreadList | Region::ThreadMessages
        ) || self.compose.focus.is_some()
        {
            return;
        }
        let Some(text) = ctx.input(|input| {
            input.events.iter().find_map(|event| {
                if let egui::Event::Paste(text) = event {
                    Some(text.clone())
                } else {
                    None
                }
            })
        }) else {
            return;
        };
        let html = crate::clipboard::read_html(ctx);
        self.compose_paste(html, Some(text));
    }

    /// Files dropped on the window go to the focused draft (or the new-message pane's). One image
    /// goes into the body, and anything else goes on the draft. Exo isn't told where a drop lands,
    /// so an image dropped on the footer still goes in the body. Pasting files, or an image (a
    /// screenshot), into a body puts each in as if dropped alone: images in the body and other
    /// files on the draft, without their names typed. HTML that's just images (an image copied
    /// from a page) pastes as its image. Other HTML is left to the editor.
    fn compose_file_events(&mut self, ctx: &egui::Context) {
        let now = ctx.input(|input| input.time);
        if self.compose_paste_attachments(ctx, now) {
            return;
        }
        let dropped: Vec<PathBuf> = ctx.input(|input| {
            input
                .raw
                .dropped_files
                .iter()
                .map(|file| file.path().to_owned())
                .collect()
        });
        if !dropped.is_empty() {
            self.compose_add_files(&dropped, now);
        }
        let in_body = self
            .compose
            .focused()
            .is_some_and(|open| open.state.field == Field::Body);
        let pasting = in_body && ctx.input(|input| input.events.iter().any(is_paste));
        if !pasting {
            return;
        }
        // The `:shortcode:`s in the clipboard's plain text become emoji.
        ctx.input_mut(|input| {
            for event in &mut input.events {
                if let egui::Event::Paste(text) = event {
                    *text = crate::emoji::replace_codes(text);
                }
            }
        });
        let files = crate::clipboard::read_files(ctx);
        let image = if files.is_empty() {
            crate::clipboard::read_image(ctx)
        } else {
            None
        };
        if files.is_empty() && image.is_none() {
            return;
        }
        if crate::clipboard::read_html(ctx).is_some_and(|html| !is_just_images(&html)) {
            return;
        }
        ctx.input_mut(|input| input.events.retain(|event| !is_paste(event)));
        for file in &files {
            self.compose_add_files(std::slice::from_ref(file), now);
        }
        if let (Some(png), Some(id)) = (image, self.compose.focus.clone()) {
            // Chromium names the clipboard's image `image.png`.
            self.compose_insert_image(&id, "image.png", "image/png", &png, now);
        }
    }

    /// Adds files as if dropped. One image goes in the body, and anything else goes on the draft.
    pub(super) fn compose_add_files(&mut self, paths: &[PathBuf], now: f64) {
        match paths {
            [path] if files::is_inline_image(&path.to_string_lossy()) => {
                let Some(id) = self
                    .compose
                    .focus
                    .clone()
                    .or_else(|| self.compose.pane().map(str::to_owned))
                else {
                    return;
                };
                let name = path
                    .file_name()
                    .map(|name| name.to_string_lossy().into_owned())
                    .unwrap_or_else(|| "untitled".into());
                match std::fs::read(path) {
                    Ok(bytes) => {
                        self.compose_insert_image(&id, &name, files::mime_type(&name), &bytes, now)
                    }
                    Err(error) => {
                        log::warn!("reading an image to insert: {error}");
                        self.compose_alert_notice(&id, files::unreadable(&name));
                    }
                }
            }
            _ => self.compose_attach(paths, now),
        }
    }

    /// Puts an image in draft `id`'s body at its caret, kept as an inline attachment.
    fn compose_insert_image(
        &mut self,
        id: &str,
        name: &str,
        mime_type: &str,
        bytes: &[u8],
        now: f64,
    ) {
        let aliases = self.snapshot.aliases.clone();
        let folder = self.account.draft_files().to_owned();
        let Some(open) = self.compose.get_mut(id) else {
            return;
        };
        let cid = factory::uuid_v4();
        match files::add_inline(
            name,
            mime_type,
            bytes,
            &cid,
            &open.record.attachments,
            &outgoing::html(open, &aliases),
            &folder,
        ) {
            Ok(attachment) => {
                open.record.attachments.push(attachment);
                open.editor.insert_image(&cid);
                open.touch(now);
            }
            Err(notice) => self.compose_alert_notice(id, notice),
        }
    }

    /// Shows `notice` over draft `id`, with OK, titled `Sorry!` unless the notice has a title.
    pub(super) fn compose_alert_notice(&mut self, id: &str, notice: files::Notice) {
        let then = AlertThen::Ok { focus_from: false };
        self.compose.alert = Some(Alert::new(
            Some(notice.title.unwrap_or("Sorry!")),
            notice.text,
            vec!["OK"],
            id.to_owned(),
            then,
        ));
    }

    /// Whether the focused draft's remote images aren't shown yet.
    pub(crate) fn compose_can_show_images(&self) -> bool {
        self.compose
            .focus
            .as_deref()
            .is_some_and(|id| !self.compose.images.is_shown(id))
    }

    /// Show Images for the focused draft, for the session. Returns whether there was one.
    pub(crate) fn compose_show_images(&mut self) -> bool {
        let Some(id) = self.compose.focus.clone() else {
            return false;
        };
        self.compose.images.show(&id);
        if let Some(open) = self.compose.get_mut(&id) {
            open.editor.reload_remote_images();
        }
        true
    }

    /// Runs each frame. It loads the drafts' images and keeps the `data:` images pasted into their
    /// bodies as inline attachments, or takes the paste back when the images would make the
    /// message too large for Gmail.
    pub(crate) fn compose_images(&mut self, ctx: &egui::Context) {
        let now = ctx.input(|input| input.time);
        let aliases = self.snapshot.aliases.clone();
        let folder = self.account.draft_files().to_owned();
        let mut refused = None;
        for open in &mut self.compose.drafts {
            let uploads = open.editor.take_uploads();
            for upload in &uploads {
                let html = outgoing::html(open, &aliases);
                match files::add_inline(
                    &upload.cid,
                    &upload.mime_type,
                    &upload.bytes,
                    &upload.cid,
                    &open.record.attachments,
                    &html,
                    &folder,
                ) {
                    Ok(attachment) => open.record.attachments.push(attachment),
                    Err(notice) => {
                        open.editor.undo();
                        refused = Some((open.id.clone(), notice));
                        break;
                    }
                }
            }
            if !uploads.is_empty() {
                open.touch(now);
            }
        }
        if let Some((id, notice)) = refused {
            self.compose_alert_notice(&id, notice);
        }
        let loader = self.account.loader();
        let sources = images::Sources {
            loader: &loader,
            draft_files: &folder,
            setting: self
                .account
                .settings()
                .remote_content_blocking
                .unwrap_or_default(),
            ctx,
        };
        let Compose { drafts, images, .. } = &mut self.compose;
        images.update(drafts, &sources);
    }

    /// A click on a card of the focused draft opens its file the way a message's attachment
    /// opens. The file is the copy kept beside the account's database, or, for a forward's file
    /// not fetched yet, the original message's part.
    pub(super) fn open_draft_file(&mut self, uuid: &str, preview: bool) {
        let Some(attachment) = self
            .compose
            .focused()
            .and_then(|open| {
                open.record
                    .attachments
                    .iter()
                    .find(|attachment| attachment.uuid == uuid)
            })
            .cloned()
        else {
            return;
        };
        self.open_draft_attachment(attachment, preview);
    }

    /// Opens a draft's (or a snippet's) file, previewed unless asked not to. The file comes from
    /// the draft files, or from Gmail's part when it isn't kept there yet.
    pub(crate) fn open_draft_attachment(
        &mut self,
        attachment: exo_store::DraftAttachment,
        preview: bool,
    ) {
        let kept = self.account.draft_files().join(&attachment.uuid);
        if kept.is_file() {
            self.attachment_files
                .open_file(kept, &attachment.uuid, attachment.name, preview);
            return;
        }
        if let exo_store::AttachmentSource::Gmail {
            message_id,
            attachment_id,
            part_id,
        } = attachment.source
        {
            let part = exo_store::Attachment {
                message_id,
                attachment_id,
                part_id,
                filename: attachment.name,
                mime_type: attachment.mime_type,
                size: u32::try_from(attachment.size).unwrap_or(u32::MAX),
                content_id: None,
                inline: false,
            };
            self.attachment_files
                .open(&self.account.loader(), part, preview);
        }
    }

    /// Adds files to the focused draft, or the new-message pane's, and says why any were turned
    /// away.
    pub(super) fn compose_attach(&mut self, paths: &[PathBuf], now: f64) {
        let Some(id) = self
            .compose
            .focus
            .clone()
            .or_else(|| self.compose.pane().map(str::to_owned))
        else {
            return;
        };
        if paths.is_empty() {
            return;
        }
        let aliases = self.snapshot.aliases.clone();
        let folder = self.account.draft_files().to_owned();
        let Some(open) = self.compose.get_mut(&id) else {
            return;
        };
        let html = outgoing::html(open, &aliases);
        let (added, notice) = files::add(paths, &open.record.attachments, &html, &folder);
        if !added.is_empty() {
            open.record.attachments.extend(added);
            open.touch(now);
        }
        if let Some(notice) = notice {
            self.compose_alert_notice(&id, notice);
        }
    }

    /// Opens a new message to a clicked `mailto:` link.
    pub(crate) fn compose_mailto(&mut self, url: &str, pop_out: bool) {
        match mailto::fields(url) {
            // Shift-click pops it out.
            Some(fields) if pop_out => self.compose_new_popped(fields),
            Some(fields) => self.compose_new(fields, None),
            None => {}
        }
    }

    /// ⌘V outside a text field: a new draft from the clipboard. When the clipboard holds the
    /// message Exo last copied, the draft takes that message's recipients and subject too.
    pub(crate) fn compose_paste(&mut self, html: Option<String>, text: Option<String>) {
        let copied = self
            .copied_message
            .clone()
            .filter(|(copied_html, _)| html.as_deref() == Some(copied_html.as_str()));
        let body = match (copied.as_ref(), &html, &text) {
            (Some((_, fields)), _, _) => fields.body.clone(),
            (None, Some(html), _) => exo_mail::sanitize_fragment(html),
            (None, None, Some(text)) if !text.trim().is_empty() => exo_mail::text_to_html(text),
            _ => return,
        };
        let fields = match copied {
            Some((_, fields)) => Fields { body, ..fields },
            None => Fields {
                body,
                ..Fields::default()
            },
        };
        self.compose_new(fields, None);
    }
}

/// A paste: egui's paste event, which egui-winit makes of ⌘V only while the clipboard has text,
/// or the ⌘V key itself. The key is all that reaches egui when a paste (from the keyboard, or
/// from Edit ▸ Paste) holds nothing but an image or files.
fn is_paste(event: &egui::Event) -> bool {
    match event {
        egui::Event::Paste(_) => true,
        egui::Event::Key {
            key: egui::Key::V,
            pressed: true,
            modifiers,
            ..
        } => modifiers.command && !modifiers.shift && !modifiers.alt && !modifiers.ctrl,
        _ => false,
    }
}

/// HTML with nothing in it but images (an image copied from a page).
fn is_just_images(html: &str) -> bool {
    if html.len() > 4096 {
        return false;
    }
    let document = exo_editor::html::parse(html);
    let runs = || document.blocks.iter().flat_map(|block| block.runs());
    let only_images = document
        .blocks
        .iter()
        .all(|block| block.is_inline() && block.path.is_empty())
        && runs().all(|run| match &run.piece {
            exo_editor::Piece::Image(_) => true,
            exo_editor::Piece::Text(text) => text.trim().is_empty(),
            exo_editor::Piece::Break => false,
        });
    only_images && runs().any(|run| matches!(run.piece, exo_editor::Piece::Image(_)))
}

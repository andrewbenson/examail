//! The app's side of attachments: what the fetches report each frame, the file notifications,
//! the open thread's cards (progress, thumbnails, `Download all`) and the viewer.

use std::path::{Path, PathBuf};

use egui::{Context, Rect};
use exo_store::{Attachment, DraftAttachment, MessageDetail};

use super::notifications::{Click, STEP};
use super::viewer::{Action as ViewerAction, Output as ViewerOutput, Viewer};
use super::{AttachmentFiles, Failure, Key, Part, Target};
use crate::account::Loader;
use crate::thread::ThreadView;

/// The offline alert's text for a file and for `Download all`.
pub(crate) const OFFLINE: &str =
    "This attachment is not available offline.\nPlease check your connection and try again.";
pub(crate) const OFFLINE_ALL: &str =
    "These attachments are not available offline.\nPlease check your connection and try again.";

impl AttachmentFiles {
    /// Brings the open thread's cards up to date with how far its clicked files have come, which
    /// messages are zipping, and the thumbnails that have arrived. Also fetches the thumbnails the
    /// thread asks for (an image card's data loads as it's shown).
    pub(crate) fn update_thread(&mut self, loader: &Loader, thread: &mut ThreadView) {
        let progress = self
            .downloads
            .iter()
            .filter(|download| download.state == super::State::Running)
            .map(|download| (download.key.clone(), download.progress))
            .collect();
        let files = thread.files_mut();
        files.set_progress(progress);
        files.set_zipping(&self.zipping);
        for attachment in thread.thumbnail_requests() {
            let key = (attachment.message_id.clone(), attachment.part_id.clone());
            let ui = self.ui.clone();
            self.fetch_thumbnail(loader, Part::Gmail(attachment), move |image| {
                ui.send(move |mail| {
                    let ctx = mail.attachment_files.ctx.clone();
                    if let Some(thread) = &mut mail.thread {
                        thread.files_mut().set_thumbnail(&ctx, key, image);
                    }
                });
            });
        }
    }

    /// Brings a read-only snippet's cards up to date with whether its files are zipping and the
    /// thumbnails that have arrived, and fetches the thumbnails they ask for. The cards are
    /// `message`'s, made of the snippet's `snippet_files`, which are kept in `folder` (the draft
    /// files) or still in Gmail.
    pub(crate) fn update_files(
        &mut self,
        loader: &Loader,
        files: &mut crate::thread::attachments::Files,
        message: &MessageDetail,
        snippet_files: &[DraftAttachment],
        folder: &Path,
    ) {
        files.set_zipping(&self.zipping);
        self.snippet_thumbnails.drain(files);
        for attachment in files.thumbnail_requests([message]) {
            let key = (attachment.message_id.clone(), attachment.part_id.clone());
            match snippet_files
                .iter()
                .find(|file| file.uuid == attachment.part_id)
                .and_then(|file| Part::of_draft(file, folder))
            {
                Some(part) => {
                    let ctx = self.ctx.clone();
                    let ui = self.snippet_thumbnails.sender(&ctx);
                    self.fetch_thumbnail(loader, part, move |image| {
                        ui.send(move |files| files.set_thumbnail(&ctx, key, image));
                    });
                }
                None => files.set_thumbnail(&self.ctx, key, None),
            }
        }
    }

    /// Fetches `part` and hands its decoded thumbnail (`None` when unreadable) to `done`.
    fn fetch_thumbnail(
        &self,
        loader: &Loader,
        part: Part,
        done: impl FnOnce(Option<egui::ColorImage>) + Send + 'static,
    ) {
        let pixels_per_point = self.ctx.pixels_per_point();
        let decode = move |bytes: Option<Vec<u8>>| {
            done(bytes.and_then(|bytes| super::thumbnails::thumbnail(&bytes, pixels_per_point)));
        };
        // Decoding a photo takes a while, so it gets its own thread and leaves the account's
        // tasks free.
        match part {
            Part::Gmail(attachment) => loader.attachment(&attachment, move |bytes| {
                std::thread::spawn(move || decode(bytes.ok()));
            }),
            Part::Kept(_, path) => {
                std::thread::spawn(move || decode(std::fs::read(path).ok()));
            }
        }
    }

    /// `Download all` under the cards of `message`, the `position`th message in a conversation
    /// about `subject`. It zips the files into the download folder, and the card list says
    /// `Downloading…` until then. Nothing happens while the message is zipping already.
    pub(crate) fn download_all(
        &mut self,
        loader: &Loader,
        message: &MessageDetail,
        subject: &str,
        position: usize,
        now: jiff::civil::DateTime,
    ) {
        let files = crate::thread::attachments::listed(message)
            .cloned()
            .map(Part::Gmail)
            .collect();
        self.zip(
            loader,
            &message.id,
            super::zip::name(subject, position),
            files,
            now,
        );
    }

    /// [`Self::download_all`] under the cards of a read-only snippet (the message `id`) about
    /// `subject`. Its files are kept in `folder` (the draft files) or still in Gmail.
    pub(crate) fn download_snippet_files(
        &mut self,
        loader: &Loader,
        id: &str,
        subject: &str,
        files: &[DraftAttachment],
        folder: &Path,
        now: jiff::civil::DateTime,
    ) {
        let parts = files
            .iter()
            .filter(|file| file.content_id.is_none())
            .filter_map(|file| Part::of_draft(file, folder));
        // The snippet is its thread's only message.
        self.zip(
            loader,
            id,
            super::zip::name(subject, 0),
            parts.collect(),
            now,
        );
    }

    /// Zips `files` as `name` into the download folder, while the cards under `id` say
    /// `Downloading…`.
    fn zip(
        &mut self,
        loader: &Loader,
        id: &str,
        name: String,
        files: Vec<Part>,
        now: jiff::civil::DateTime,
    ) {
        if self.zipping.contains(id) {
            return;
        }
        let Some(folder) = crate::downloads::folder(&self.ctx) else {
            return;
        };
        self.zipping.insert(id.to_owned());
        let (ui, id) = (self.ui.clone(), id.to_owned());
        loader.run(
            move |store| async move {
                let mut fetched = Vec::with_capacity(files.len());
                for file in files {
                    fetched.push(match file {
                        Part::Gmail(file) => (
                            file.filename,
                            store
                                .part_data(
                                    &file.message_id,
                                    file.attachment_id.as_deref(),
                                    &file.part_id,
                                )
                                .await
                                .map_err(|error| {
                                    log::warn!("zipping files: {error}");
                                    Failure::of(&error)
                                })?,
                        ),
                        Part::Kept(name, path) => (
                            name,
                            std::fs::read(&path).map_err(|error| {
                                log::warn!("zipping files: {error}");
                                Failure::Fetch
                            })?,
                        ),
                    });
                }
                Ok::<_, Failure>(fetched)
            },
            move |fetched| {
                std::thread::spawn(move || {
                    let target = Target::Downloads(folder);
                    let saved = fetched.and_then(|files| {
                        target
                            .save(&name, &super::zip::archive(&files, &now))
                            .map_err(|error| target.failed(&error))
                    });
                    ui.send(move |mail| mail.finish_file_zip(id, name, saved));
                });
            },
        );
    }
}

impl crate::Mail {
    /// Copy Attachments, which puts the focused message's files on the clipboard and shows the
    /// hint saying where they go.
    pub(crate) fn copy_attachments(&mut self) {
        let Some(thread) = &self.thread else { return };
        let Some(message) = thread.detail().and_then(|detail| {
            detail
                .messages
                .iter()
                .find(|message| Some(message.id.as_str()) == thread.focused())
        }) else {
            return;
        };
        if !super::has_copied(&message.attachments) {
            return;
        }
        let copied = super::copy::Copied::new(self.account.email(), &message.attachments);
        crate::clipboard::write_attachments(
            &self.ctx,
            &serde_json::to_string(&copied).expect("copied attachments serialize"),
        );
        self.commands.hint(
            exo_commands::Hint::new()
                .text("Attachments copied to clipboard. Hit")
                .keys(["⌘", "V"])
                .text("in a draft to paste them."),
        );
    }

    pub(super) fn finish_file_download(
        &mut self,
        key: &Key,
        outcome: Result<(PathBuf, bool), Failure>,
    ) {
        let now = self.ctx.input(|input| input.time);
        if self.attachment_files.finish(key, outcome, now) {
            self.pickers.inform("Sorry!", OFFLINE.to_owned());
        }
    }

    fn finish_file_zip(&mut self, message: String, name: String, saved: Result<PathBuf, Failure>) {
        let now = self.ctx.input(|input| input.time);
        if self.attachment_files.finish_zip(message, name, saved, now) {
            self.pickers.inform("Sorry!", OFFLINE_ALL.to_owned());
        }
    }

    /// Expires finished file notifications and polls Quick Look.
    pub(crate) fn attachment_files_poll(&mut self, now: f64) {
        self.attachment_files.poll(now);
    }

    /// Draws the file notifications over the toast and does what a click on one asks.
    pub(crate) fn show_file_notifications(&mut self, ctx: &Context, page: Rect, now: f64) {
        let bottom = self.commands.toast_bottom()
            + if self.commands.toast_text().is_some() {
                STEP
            } else {
                0.0
            };
        match self
            .attachment_files
            .show_notifications(ctx, page, bottom, now)
        {
            Some(Click::Show(path)) => self.attachment_files.reveal(&path),
            Some(Click::Settings) => self.run(
                crate::Action::Settings(crate::settings::Action::Open(
                    crate::settings::Dialog::Downloads,
                )),
                now,
            ),
            Some(Click::Preview(path, name)) => self.attachment_files.show(path, name),
            None => {}
        }
    }

    /// Opens Exo's viewer over `attachments`, starting at the `start`th.
    pub(crate) fn preview_attachments(&mut self, attachments: Vec<Attachment>, start: usize) {
        if attachments.is_empty() {
            return;
        }
        let now = self.ctx.input(|input| input.time);
        let images = self
            .account
            .settings()
            .remote_content_blocking
            .unwrap_or_default();
        self.attachment_files.viewer = Some(Viewer::open(
            &self.ctx,
            &self.account.loader(),
            attachments,
            start,
            images,
            now,
        ));
    }

    /// Opens a search result's file card. An image or a message opens in the viewer, along with
    /// the conversation's other previewable files. Anything else previews in Quick Look or
    /// downloads.
    pub(crate) fn open_result_file(&mut self, file: Attachment, preview: bool) {
        if preview && super::is_previewable(&file) {
            let (files, start) = match self
                .inbox
                .threads()
                .iter()
                .find(|thread| thread.files.contains(&file))
            {
                Some(thread) => super::preview_selection(&thread.files, &file),
                None => (vec![file], 0),
            };
            self.preview_attachments(files, start);
        } else {
            self.attachment_files
                .open(&self.account.loader(), file, preview);
        }
    }

    /// Whether the viewer has the keyboard.
    pub(crate) fn previewing(&self) -> bool {
        self.attachment_files
            .viewer
            .as_ref()
            .is_some_and(|viewer| !viewer.is_closing())
    }

    /// Does what the viewer's keys or clicks ask. A download works the way a click on the card's
    /// download icon does.
    pub(crate) fn viewer_run(&mut self, action: ViewerAction, now: f64) {
        let Some(viewer) = self.attachment_files.viewer.as_mut() else {
            return;
        };
        match action {
            ViewerAction::Download => {
                if let Some(attachment) = viewer.current().cloned() {
                    self.attachment_files
                        .open(&self.account.loader(), attachment, false);
                }
            }
            ViewerAction::SaveFile(n) => {
                if let Some((name, bytes)) = viewer.file(n) {
                    self.attachment_files.save_file(name, bytes);
                }
            }
            ViewerAction::Copy => viewer.copy(&self.ctx),
            action => viewer.run(action, now),
        }
    }

    /// Draws the viewer over `page`, if it's open.
    pub(crate) fn show_viewer(&mut self, ctx: &Context, page: Rect, now: f64) {
        let images = self
            .account
            .settings()
            .remote_content_blocking
            .unwrap_or_default();
        let loader = self.account.loader();
        let Some(viewer) = self.attachment_files.viewer.as_mut() else {
            return;
        };
        viewer.set_images(images);
        let clicked = viewer.show(
            ctx,
            page,
            ctx.content_rect().height(),
            &loader,
            self.account.email(),
            now,
            &self.now,
        );
        if viewer.is_gone(now) {
            self.attachment_files.viewer = None;
        }
        match clicked {
            Some(ViewerOutput::Action(action)) => self.viewer_run(action, now),
            // A `mailto:` link's draft opens under the viewer, which closes to show the draft.
            Some(ViewerOutput::Link { url, .. })
                if crate::page_menu::strip_mailto(&url).is_some() =>
            {
                self.viewer_run(ViewerAction::Close, now);
                self.compose_mailto(&url, false);
            }
            Some(ViewerOutput::Link {
                url,
                background: true,
            }) => self.open_url_in_background(&url),
            Some(ViewerOutput::Link {
                url,
                background: false,
            }) => self.open_url(&url),
            Some(ViewerOutput::Drag(at)) => {
                if let Some(attachment) = self
                    .attachment_files
                    .viewer
                    .as_ref()
                    .and_then(Viewer::current)
                    .cloned()
                {
                    self.attachment_files.drag(
                        &loader,
                        Part::Gmail(attachment),
                        at,
                        ctx.zoom_factor(),
                    );
                }
            }
            None => {}
        }
    }

    /// `Download all` under message `id` of the open conversation.
    pub(crate) fn download_all(&mut self, id: &str) {
        let Some(detail) = self.thread.as_ref().and_then(ThreadView::shared_detail) else {
            return;
        };
        let Some(position) = detail.messages.iter().position(|message| message.id == id) else {
            return;
        };
        let loader = self.account.loader();
        self.attachment_files.download_all(
            &loader,
            &detail.messages[position],
            &detail.subject,
            position,
            self.now.datetime(),
        );
    }

    /// Holds the `card`th card of the open conversation's last message as downloading at
    /// `percent`, with its bar and notification showing (for screenshots).
    pub fn hold_download(&mut self, card: usize, percent: f32) {
        let Some(detail) = self.thread.as_ref().and_then(ThreadView::shared_detail) else {
            return;
        };
        let Some(attachment) = detail
            .messages
            .last()
            .and_then(|message| crate::thread::attachments::listed(message).nth(card))
            .cloned()
        else {
            return;
        };
        self.attachment_files.hold(&attachment, percent);
    }

    /// The file the viewer shows, while it's open (for tests).
    pub fn previewed(&self) -> Option<&str> {
        self.attachment_files
            .viewer
            .as_ref()
            .filter(|viewer| !viewer.is_closing())
            .and_then(Viewer::current)
            .map(|file| file.filename.as_str())
    }

    /// The file notifications' texts, oldest first (for tests).
    pub fn file_notifications(&self) -> Vec<String> {
        self.attachment_files
            .downloads()
            .iter()
            .map(|download| match &download.state {
                super::State::Failed(failure) => failure.text(&download.name),
                _ => download.name.clone(),
            })
            .collect()
    }
}

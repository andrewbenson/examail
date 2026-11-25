//! Clicked attachments on disk. A click previews the file in Quick Look from a cache folder, or
//! downloads it to the folder the Downloads setting names ([`crate::downloads`]). Off macOS a
//! preview is a download opened in its default app.

mod copy;
#[cfg(target_os = "macos")]
pub(crate) mod drag;
mod eml;
mod mail;

pub(crate) use copy::{Copied, OTHER_ACCOUNT};

/// What Copy Attachments put on the clipboard, if the clipboard holds it.
pub(crate) fn pasted(ctx: &egui::Context) -> Option<Copied> {
    let mut copied: Copied = crate::clipboard::read_attachments(ctx)
        .and_then(|json| serde_json::from_str(&json).ok())?;
    copied.attachments.retain(copy::File::has_safe_ids);
    (!copied.attachments.is_empty()).then_some(copied)
}

/// Whether Copy Attachments has anything to copy from a message with `attachments`.
pub(crate) fn has_copied(attachments: &[Attachment]) -> bool {
    copy::copied(attachments).next().is_some()
}
mod notifications;
#[cfg(target_os = "macos")]
mod quick_look;
mod thumbnails;
pub(crate) mod viewer;
mod zip;

use std::{
    collections::HashSet,
    fs::File,
    io::{ErrorKind, Write as _},
    path::{Path, PathBuf},
    time::{Duration, Instant},
};

use exo_store::Attachment;

use crate::account::{Loader, UiSender, UiTasks};

/// Extensions that only ever download.
const ARCHIVES: &[&str] = &[
    "7z", "a", "aar", "ar", "ace", "arj", "apk", "arc", "ark", "br", "bz", "bz2", "cab", "chm",
    "cpgz", "cpio", "deb", "dmg", "ear", "egg", "epub", "gz", "iso", "jar", "lha", "lrz", "lz",
    "lz4", "lzh", "lzma", "lzo", "lzop", "mar", "par2", "pea", "pet", "pkg", "rar", "rpm", "rz",
    "s7z", "shar", "sit", "sitx", "tar", "tbz", "tbz2", "tgz", "tlz", "txz", "tzo", "war", "whl",
    "xpi", "xz", "z", "zip", "zipx", "zoo", "zpaq", "zst",
];
/// A preview opens only when its download finished this soon after the click. A slower download
/// just lands in the cache.
const PREVIEW_WITHIN: Duration = Duration::from_secs(8);
/// The preview cache's folder in the temp directory. It holds a folder per account, which signing
/// out deletes.
const CACHE_DIR: &str = "ExoAttachmentPreviewCache";

/// Where `email`'s previews are kept unless [`AttachmentFiles::set_cache`] says otherwise.
pub(crate) fn default_cache(email: &str) -> PathBuf {
    std::env::temp_dir()
        .join(CACHE_DIR)
        .join(cache_component(email))
}

/// Identifies an attachment across fetches by its message and MIME part, because Gmail's
/// attachment ids change on every fetch.
pub(crate) type Key = (String, String);

/// Once a download is over, its notification fades out after 6 s and goes at 8 s.
const FADE_AFTER: f64 = 6.0;
const GONE_AFTER: f64 = 8.0;
/// A preview's notification goes this soon after the preview opens.
const PREVIEW_CLEARS_AFTER: f64 = 1.5;

/// Where a file comes from.
#[derive(Clone)]
pub(crate) enum Part {
    /// A Gmail message's part.
    Gmail(Attachment),
    /// A file kept on disk, under its name.
    Kept(String, PathBuf),
}

impl Part {
    /// Where draft file `file` comes from: kept in `folder` (the draft files), or else still in
    /// Gmail. `None` for a file of this Mac's that isn't kept.
    pub(crate) fn of_draft(file: &exo_store::DraftAttachment, folder: &Path) -> Option<Self> {
        let kept = is_safe_id(&file.uuid)
            .then(|| folder.join(&file.uuid))
            .filter(|kept| within_cache(folder, kept) && kept.is_file());
        if let Some(kept) = kept {
            return Some(Self::Kept(file.name.clone(), kept));
        }
        match &file.source {
            exo_store::AttachmentSource::Gmail {
                message_id,
                attachment_id,
                part_id,
            } if is_safe_id(message_id)
                && is_safe_id(part_id)
                && attachment_id.as_deref().is_none_or(is_safe_id) =>
            {
                Some(Self::Gmail(Attachment {
                    message_id: message_id.clone(),
                    attachment_id: attachment_id.clone(),
                    part_id: part_id.clone(),
                    filename: file.name.clone(),
                    mime_type: file.mime_type.clone(),
                    size: u32::try_from(file.size).unwrap_or(u32::MAX),
                    content_id: None,
                    inline: false,
                }))
            }
            exo_store::AttachmentSource::Gmail { .. } | exo_store::AttachmentSource::Local => None,
        }
    }

    /// The file's name.
    pub(crate) fn name(&self) -> &str {
        match self {
            Self::Gmail(attachment) => &attachment.filename,
            Self::Kept(name, _) => name,
        }
    }
}

/// Why a file didn't arrive.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Failure {
    /// No network, and no copy kept from before. Exo shows the offline alert, not a notification.
    Offline,
    /// Gmail didn't hand the bytes over (rate limited, refused).
    Fetch,
    /// The bytes came but writing them failed this way.
    Save(ErrorKind),
}

impl Failure {
    /// What the file notification says when the file named `name` failed. It says
    /// `Failed downloading {name}` when the bytes never came, and the copy for a failed save
    /// otherwise, so it's clear which went wrong.
    pub(crate) fn text(self, name: &str) -> String {
        match self {
            Self::Offline | Self::Fetch => format!("Failed downloading {name}"),
            Self::Save(ErrorKind::PermissionDenied | ErrorKind::ReadOnlyFilesystem) => {
                "Download failed: not allowed to save to the selected download folder".into()
            }
            Self::Save(ErrorKind::StorageFull) => "Download failed: disk space insufficient".into(),
            Self::Save(_) => "Download failed: could not save file".into(),
        }
    }

    fn of(error: &exo_store::Error) -> Self {
        if matches!(error, exo_store::Error::Offline(_)) {
            Self::Offline
        } else {
            Self::Fetch
        }
    }
}

/// Where a download is.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum State {
    /// Being fetched.
    Running,
    /// Saved at this path.
    Saved(PathBuf),
    /// It didn't arrive.
    Failed(Failure),
}

/// One file's download, shown by the bar on its card and by its notification.
#[derive(Clone, Debug)]
pub(crate) struct Download {
    pub(crate) key: Key,
    /// The file's name.
    pub(crate) name: String,
    pub(crate) state: State,
    /// How much has arrived, 0–100. It never goes back.
    pub(crate) progress: f32,
    /// When it ended, which starts its notification's fade.
    ended: Option<f64>,
    /// When a preview opened, its notification goes at this time, before the fade would end.
    clear_at: Option<f64>,
}

impl Download {
    /// Whether its notification is fading out at `now`, which it does from 6 s after the download
    /// ended.
    pub(crate) fn fading(&self, now: f64) -> bool {
        self.ended.is_some_and(|ended| now >= ended + FADE_AFTER)
    }
}

/// Clicked attachments being fetched, saved and shown.
pub(crate) struct AttachmentFiles {
    ctx: egui::Context,
    ui: UiSender<crate::Mail>,
    /// The preview cache's folder.
    cache: PathBuf,
    /// Downloads in the order they started, while their notifications show. A click on a file that
    /// is being fetched does nothing.
    downloads: Vec<Download>,
    /// Decoded thumbnails arriving for a read-only snippet's cards.
    snippet_thumbnails: UiTasks<crate::thread::attachments::Files>,
    /// Messages whose `Download all` is zipping.
    zipping: HashSet<String>,
    /// Exo's viewer, while it's open.
    viewer: Option<viewer::Viewer>,
    /// The last cards dragged out, kept because a drop may still call in their promises.
    #[cfg(target_os = "macos")]
    drags: std::collections::VecDeque<objc2::rc::Retained<drag::Source>>,
    #[cfg(target_os = "macos")]
    quick_look: quick_look::QuickLook,
}

impl AttachmentFiles {
    /// Shows `email`'s files on `ctx`'s window.
    pub(crate) fn new(ctx: &egui::Context, email: &str, ui: UiSender<crate::Mail>) -> Self {
        Self {
            ctx: ctx.clone(),
            ui,
            cache: default_cache(email),
            downloads: Vec::new(),
            snippet_thumbnails: UiTasks::default(),
            zipping: HashSet::new(),
            viewer: None,
            #[cfg(target_os = "macos")]
            drags: std::collections::VecDeque::new(),
            #[cfg(target_os = "macos")]
            quick_look: quick_look::QuickLook::default(),
        }
    }

    /// Keeps previews in `folder` rather than the temp directory's [`CACHE_DIR`].
    pub(crate) fn set_cache(&mut self, folder: PathBuf) {
        self.cache = folder;
    }

    /// The folder previews are kept in.
    pub(crate) fn cache(&self) -> &Path {
        &self.cache
    }

    /// The downloads whose notifications show, oldest first.
    pub(crate) fn downloads(&self) -> &[Download] {
        &self.downloads
    }

    fn running(&self, key: &Key) -> bool {
        self.downloads
            .iter()
            .any(|download| &download.key == key && download.state == State::Running)
    }

    /// Starts a download of `name` under `key`. A notification already showing for `key` now says
    /// so in place of what it said before.
    fn start(&mut self, key: Key, name: String) {
        let download = Download {
            key,
            name,
            state: State::Running,
            progress: 0.0,
            ended: None,
            clear_at: None,
        };
        match self
            .downloads
            .iter_mut()
            .find(|kept| kept.key == download.key)
        {
            Some(kept) => *kept = download,
            None => self.downloads.push(download),
        }
    }

    /// Handles a click on `attachment`. It previews the file when `preview` is set (no ⌘ or ⇧, not
    /// the download icon) and the file isn't an archive, and otherwise saves it to the download
    /// folder. A preview already in the cache shows at once, without a notification.
    pub(crate) fn open(&mut self, loader: &Loader, attachment: Attachment, preview: bool) {
        let key = (attachment.message_id.clone(), attachment.part_id.clone());
        if self.running(&key) {
            return;
        }
        let preview = preview && !is_archive(&attachment.filename);
        let cached = cache_path(&self.cache, &attachment)
            .filter(|path| preview && within_cache(&self.cache, path));
        let target = match cached {
            Some(path) if path.is_file() => {
                self.show(path, attachment.filename);
                return;
            }
            Some(path) => Target::Cache(path),
            None if preview && cfg!(target_os = "macos") => return,
            None => {
                let Some(folder) = crate::downloads::folder(&self.ctx) else {
                    return;
                };
                Target::Downloads(folder)
            }
        };
        self.start(key.clone(), attachment.filename.clone());
        let started = Instant::now();
        let progress = {
            let (ui, key) = (self.ui.clone(), key.clone());
            move |percent| {
                let key = key.clone();
                ui.send(move |mail| mail.attachment_files.set_progress(&key, percent));
            }
        };
        let ui = self.ui.clone();
        let name = attachment.filename.clone();
        loader.download(&attachment, progress, move |bytes| {
            // The loader has logged why a fetch failed.
            let saved = bytes
                .map_err(|error| Failure::of(&error))
                .and_then(|bytes| {
                    target
                        .save(&name, &bytes)
                        .map_err(|error| target.failed(&error))
                });
            let outcome = saved.map(|path| (path, preview && started.elapsed() < PREVIEW_WITHIN));
            ui.send(move |mail| mail.finish_file_download(&key, outcome));
        });
    }

    /// Handles a click on a draft's file, which is kept at `path` under `id` rather than its
    /// `name`. The file is previewed from a copy named `name` in the cache (Quick Look goes by the
    /// name), or copied to the download folder, as [`Self::open`] does with a message's file.
    pub(crate) fn open_file(&mut self, path: PathBuf, id: &str, name: String, preview: bool) {
        let key = ("draft".to_owned(), id.to_owned());
        if self.running(&key) {
            return;
        }
        let preview = preview && !is_archive(&name);
        let cached = cfg!(target_os = "macos")
            .then(|| draft_cache_file(&self.cache, id, &name))
            .filter(|cached| preview && within_cache(&self.cache, cached));
        let target = match cached {
            // A draft's file never changes under its id.
            Some(cached) if cached.is_file() => {
                self.show(cached, name);
                return;
            }
            Some(cached) => Target::Cache(cached),
            None if preview && cfg!(target_os = "macos") => return,
            None => {
                let Some(folder) = crate::downloads::folder(&self.ctx) else {
                    return;
                };
                Target::Downloads(folder)
            }
        };
        self.start(key.clone(), name.clone());
        let ui = self.ui.clone();
        std::thread::spawn(move || {
            let saved = std::fs::read(&path)
                .and_then(|bytes| target.save(&name, &bytes))
                .map_err(|error| target.failed(&error));
            let outcome = saved.map(|path| (path, preview));
            ui.send(move |mail| mail.finish_file_download(&key, outcome));
        });
    }

    fn set_progress(&mut self, key: &Key, percent: f32) {
        if let Some(download) = self
            .downloads
            .iter_mut()
            .find(|download| &download.key == key)
        {
            download.progress = download.progress.max(percent);
        }
    }

    /// Records the end of a file fetch, opening Quick Look when `preview` holds and returning
    /// `true` when an offline failure should show the offline alert instead of a notification.
    pub(super) fn finish(
        &mut self,
        key: &Key,
        outcome: Result<(PathBuf, bool), Failure>,
        now: f64,
    ) -> bool {
        let Some(at) = self
            .downloads
            .iter()
            .position(|download| &download.key == key)
        else {
            return false;
        };
        match outcome {
            Ok((path, preview)) => {
                let download = &mut self.downloads[at];
                download.state = State::Saved(path.clone());
                download.progress = 100.0;
                download.ended = Some(now);
                if preview {
                    download.clear_at = Some(now + PREVIEW_CLEARS_AFTER);
                    let name = download.name.clone();
                    self.show(path, name);
                }
                false
            }
            // Offline, Exo shows an alert instead, and the notification clears at once.
            Err(Failure::Offline) => {
                self.downloads.remove(at);
                true
            }
            Err(failure) => {
                let download = &mut self.downloads[at];
                download.state = State::Failed(failure);
                download.ended = Some(now);
                false
            }
        }
    }

    /// Records the end of a `Download all` zip, returning `true` when it failed offline.
    pub(super) fn finish_zip(
        &mut self,
        message: String,
        name: String,
        saved: Result<PathBuf, Failure>,
        now: f64,
    ) -> bool {
        self.zipping.remove(&message);
        let state = match saved {
            Ok(path) => State::Saved(path),
            Err(Failure::Offline) => return true,
            Err(failure) => State::Failed(failure),
        };
        self.start(("zip".to_owned(), message), name);
        if let Some(download) = self.downloads.last_mut() {
            download.state = state;
            download.progress = 100.0;
            download.ended = Some(now);
        }
        false
    }

    /// Lets notifications go whose time is up and polls Quick Look. Call once a frame, on the UI
    /// thread, at `now` (the frame's time).
    pub(crate) fn poll(&mut self, now: f64) {
        self.downloads.retain(|download| {
            download.clear_at.is_none_or(|at| now < at)
                && download.ended.is_none_or(|ended| now < ended + GONE_AFTER)
        });
        if let Some(next) = self
            .downloads
            .iter()
            .flat_map(|download| {
                [
                    download.clear_at,
                    download.ended.map(|ended| ended + FADE_AFTER),
                    download.ended.map(|ended| ended + GONE_AFTER),
                ]
            })
            .flatten()
            .filter(|at| *at > now)
            .reduce(f64::min)
        {
            self.ctx.request_repaint_after_secs((next - now) as f32);
        }
        #[cfg(target_os = "macos")]
        if let Some(mtm) = objc2::MainThreadMarker::new()
            && let Some(wait) = self.quick_look.poll(mtm)
        {
            self.ctx.request_repaint_after(wait);
        }
    }

    /// Drags the card of file `part` out from `at` (in egui points, which `zoom` scales to the
    /// window's). The drag carries a promise of the file for wherever it's dropped.
    #[cfg(target_os = "macos")]
    pub(crate) fn drag(&mut self, loader: &Loader, part: Part, at: egui::Pos2, zoom: f32) {
        /// Promises kept for drops that haven't asked yet. A drop asks at once, so a few will do.
        const KEPT: usize = 8;
        if let Some(source) = drag::start(loader, part, at, zoom) {
            if self.drags.len() == KEPT {
                self.drags.pop_front();
            }
            self.drags.push_back(source);
        }
    }

    /// Off macOS, cards don't drag out.
    #[cfg(not(target_os = "macos"))]
    pub(crate) fn drag(&mut self, _loader: &Loader, _part: Part, _at: egui::Pos2, _zoom: f32) {}

    /// Saves `bytes` as `name` in the download folder, with a notification, the way a message's
    /// attachments download.
    pub(crate) fn save_file(&mut self, name: String, bytes: Vec<u8>) {
        let Some(folder) = crate::downloads::folder(&self.ctx) else {
            return;
        };
        let key = (
            "file".to_owned(),
            format!("{name}-{}", self.downloads.len()),
        );
        self.start(key.clone(), name.clone());
        let ui = self.ui.clone();
        std::thread::spawn(move || {
            let target = Target::Downloads(folder);
            let saved = target
                .save(&name, &bytes)
                .map_err(|error| target.failed(&error));
            let outcome = saved.map(|path| (path, false));
            ui.send(move |mail| mail.finish_file_download(&key, outcome));
        });
    }

    /// Shows `attachment` as downloading with `percent` of it arrived, and keeps it that way, so
    /// screenshots can show its card's bar and its notification.
    pub(crate) fn hold(&mut self, attachment: &Attachment, percent: f32) {
        self.start(
            (attachment.message_id.clone(), attachment.part_id.clone()),
            attachment.filename.clone(),
        );
        if let Some(download) = self.downloads.last_mut() {
            download.progress = percent;
        }
    }

    /// Lets the notification of the download under `key` go (its ×, or an action that ran).
    pub(crate) fn clear(&mut self, key: &Key) {
        self.downloads.retain(|download| &download.key != key);
    }

    /// Previews the file at `path` in Quick Look, titled `title`.
    #[cfg(target_os = "macos")]
    pub(crate) fn show(&mut self, path: PathBuf, title: String) {
        // eframe runs the UI on the main thread, but tests drive it from other threads.
        let Some(mtm) = objc2::MainThreadMarker::new() else {
            return;
        };
        if let Some(wait) = self.quick_look.preview(mtm, path, title) {
            self.ctx.request_repaint_after(wait);
        }
    }

    /// Opens the file at `path` in its default app.
    #[cfg(not(target_os = "macos"))]
    pub(crate) fn show(&mut self, path: PathBuf, _title: String) {
        self.ctx
            .open_url(egui::OpenUrl::new_tab(format!("file://{}", path.display())));
    }

    /// Shows the saved file at `path` in Finder, selected (the file notification's Show).
    #[cfg(target_os = "macos")]
    pub(crate) fn reveal(&self, path: &Path) {
        use objc2_app_kit::NSWorkspace;
        use objc2_foundation::{NSArray, NSURL};
        let Some(url) = NSURL::from_file_path(path) else {
            return;
        };
        NSWorkspace::sharedWorkspace()
            .activateFileViewerSelectingURLs(&NSArray::from_retained_slice(&[url]));
    }

    /// Opens the folder holding the saved file at `path`.
    #[cfg(not(target_os = "macos"))]
    pub(crate) fn reveal(&self, path: &Path) {
        if let Some(folder) = path.parent() {
            self.ctx.open_url(egui::OpenUrl::new_tab(format!(
                "file://{}",
                folder.display()
            )));
        }
    }
}

/// Where a fetched attachment goes.
enum Target {
    /// This file in the preview cache.
    Cache(PathBuf),
    /// A free name in this folder.
    Downloads(PathBuf),
}

impl Target {
    /// Writes `bytes`, naming the file `name` in the download folder, quarantines it as a
    /// download, and returns where it went.
    fn save(&self, name: &str, bytes: &[u8]) -> std::io::Result<PathBuf> {
        let path = match self {
            Self::Cache(path) => {
                // Written aside and renamed, so concurrent saves never race and a write cut short
                // is never taken for a cached file.
                fastframe_file::write(path, bytes)?;
                path.clone()
            }
            Self::Downloads(folder) => {
                std::fs::create_dir_all(folder)?;
                let (path, mut file) = create_unique(folder, name)?;
                file.write_all(bytes)?;
                path
            }
        };
        #[cfg(target_os = "macos")]
        quarantine(&path);
        Ok(path)
    }

    /// Logs that saving a file here failed with `error`, and returns the [`Failure`] it was.
    fn failed(&self, error: &std::io::Error) -> Failure {
        let folder = match self {
            Self::Cache(path) => path.parent().unwrap_or(path),
            Self::Downloads(folder) => folder,
        };
        // macOS keeps a program out of ~/Downloads, answering EPERM, unless the app it runs under
        // (the terminal, for a bare binary) is allowed in.
        let hint = if matches!(self, Self::Downloads(_))
            && error.kind() == ErrorKind::PermissionDenied
        {
            "; check that the app Examail runs under may use the Downloads folder (System Settings › Privacy & Security › Files and Folders)"
        } else {
            ""
        };
        log::error!(
            "saving an attachment in {}: {error}{hint}",
            folder.display()
        );
        Failure::Save(error.kind())
    }
}

/// The image types shown as images.
const IMAGE_TYPES: &[&str] = &[
    "image/jpg",
    "image/jpeg",
    "image/jif",
    "image/jfif",
    "image/gif",
    "image/png",
    "image/bmp",
    "image/webp",
];

/// Whether `attachment` is an image, meaning its type is one of [`IMAGE_TYPES`] or its extension
/// is one of their subtypes.
pub(crate) fn is_image(attachment: &Attachment) -> bool {
    let extension = extension(&attachment.filename);
    IMAGE_TYPES.contains(&attachment.mime_type.to_ascii_lowercase().as_str())
        || extension.as_deref().is_some_and(|extension| {
            IMAGE_TYPES
                .iter()
                .any(|kind| kind.strip_prefix("image/") == Some(extension))
        })
}

/// Whether `attachment` is a message.
pub(crate) fn is_eml(attachment: &Attachment) -> bool {
    extension(&attachment.filename).as_deref() == Some("eml")
        || attachment.mime_type.eq_ignore_ascii_case("message/rfc822")
}

/// Whether a click previews `attachment` in Exo's own viewer, which shows images and messages.
/// PDFs and everything else go to Quick Look.
pub(crate) fn is_previewable(attachment: &Attachment) -> bool {
    is_image(attachment) || is_eml(attachment)
}

/// Filters `among` to the attachments Exo's own viewer previews ([`is_previewable`]), and returns
/// them alongside the index of `target` (matched by `(message_id, part_id)`, defaulting to `0`).
pub(crate) fn preview_selection<'a>(
    among: impl IntoIterator<Item = &'a Attachment>,
    target: &Attachment,
) -> (Vec<Attachment>, usize) {
    let files: Vec<Attachment> = among
        .into_iter()
        .filter(|candidate| is_previewable(candidate))
        .cloned()
        .collect();
    let start = files
        .iter()
        .position(|candidate| {
            candidate.message_id == target.message_id && candidate.part_id == target.part_id
        })
        .unwrap_or(0);
    (files, start)
}

/// `filename`'s extension in lowercase: the non-empty part after the last dot when it contains no
/// whitespace.
pub(crate) fn extension(filename: &str) -> Option<String> {
    filename
        .rsplit_once('.')
        .map(|(_, extension)| extension)
        .filter(|extension| !extension.is_empty() && !extension.chars().any(char::is_whitespace))
        .map(str::to_lowercase)
}

/// Whether `name` is an archive, meaning its extension (after the last dot, without whitespace) is
/// one of [`ARCHIVES`].
fn is_archive(name: &str) -> bool {
    extension(name)
        .as_deref()
        .is_some_and(|extension| ARCHIVES.contains(&extension))
}

/// Where `attachment`'s preview is cached in `cache`. Each message part gets its own folder, so
/// the file keeps its name (Quick Look and "Open with" show it). Off macOS there's no cache.
fn cache_path(cache: &Path, attachment: &Attachment) -> Option<PathBuf> {
    cfg!(target_os = "macos").then(|| cache_file(cache, attachment))
}

/// [`cache_path`] on any platform.
fn cache_file(cache: &Path, attachment: &Attachment) -> PathBuf {
    cache
        .join(format!(
            "{}-{}",
            cache_component(&attachment.message_id),
            cache_component(&attachment.part_id)
        ))
        .join(file_name(&attachment.filename))
}

/// Where draft file `id` named `name` is cached in `cache`.
fn draft_cache_file(cache: &Path, id: &str, name: &str) -> PathBuf {
    cache
        .join(format!("draft-{}", cache_component(id)))
        .join(file_name(name))
}

/// Whether `id` is a single safe identifier without path separators, `..`, or control/formatting
/// characters.
pub(crate) fn is_safe_id(id: &str) -> bool {
    !id.is_empty()
        && !id.contains("..")
        && !id
            .chars()
            .any(|c| matches!(c, '/' | '\\') || c.is_control() || is_format(c))
}

/// Sanitizes an identifier into a single preview-cache folder segment.
fn cache_component(id: &str) -> String {
    const MAX_COMPONENT: usize = 120;
    let safe: String = id
        .chars()
        .map(|c| {
            if c.is_control() || is_format(c) || "/\\\"*:<>?|~".contains(c) {
                '_'
            } else {
                c
            }
        })
        .collect::<String>()
        .replace("..", "__");
    let safe = safe.trim_matches(|c: char| c == '.' || c.is_whitespace());
    let safe = if safe.is_empty() { "part" } else { safe };
    let mut end = safe.len().min(MAX_COMPONENT);
    while !safe.is_char_boundary(end) {
        end -= 1;
    }
    safe[..end].to_owned()
}

/// Whether `path` lies strictly beneath `cache` using normal path components only.
fn within_cache(cache: &Path, path: &Path) -> bool {
    path.starts_with(cache)
        && path.strip_prefix(cache).is_ok_and(|relative| {
            let mut any = false;
            for component in relative.components() {
                if !matches!(component, std::path::Component::Normal(_)) {
                    return false;
                }
                any = true;
            }
            any
        })
}

/// The longest file name macOS allows, in UTF-8 bytes.
const MAX_NAME: usize = 255;

/// `name` made safe to name a file on disk. Path separators, control and invisible formatting
/// characters (a right-to-left override can pass `fdp.exe` off as `exe.pdf`) and `"*:<>?|~` become
/// `_`. The dots and spaces around the name go (no hidden files), and the name is cut to fit
/// [`MAX_NAME`].
fn file_name(name: &str) -> String {
    let safe: String = name
        .chars()
        .map(|c| {
            if c.is_control() || is_format(c) || "/\\\"*:<>?|~".contains(c) {
                '_'
            } else {
                c
            }
        })
        .collect();
    let safe = safe.trim_matches(|c: char| c == '.' || c.is_whitespace());
    with_suffix(if safe.is_empty() { "attachment" } else { safe }, "")
}

/// Unicode's invisible formatting characters (category Cf), kept out of file names.
fn is_format(c: char) -> bool {
    matches!(c, '\u{ad}' | '\u{600}'..='\u{605}' | '\u{61c}' | '\u{6dd}' | '\u{70f}' | '\u{890}' | '\u{891}' | '\u{8e2}' | '\u{180e}' | '\u{200b}'..='\u{200f}' | '\u{202a}'..='\u{202e}' | '\u{2060}'..='\u{2064}' | '\u{2066}'..='\u{206f}' | '\u{feff}' | '\u{fff9}'..='\u{fffb}' | '\u{110bd}' | '\u{110cd}' | '\u{13430}'..='\u{1343f}' | '\u{1bca0}'..='\u{1bca3}' | '\u{1d173}'..='\u{1d17a}' | '\u{e0001}' | '\u{e0020}'..='\u{e007f}')
}

/// `name` with `suffix` before its extension, its stem cut short so it fits [`MAX_NAME`].
fn with_suffix(name: &str, suffix: &str) -> String {
    let (stem, extension) = match name.rsplit_once('.') {
        Some((stem, extension))
            if !stem.is_empty() && extension.len() + suffix.len() < MAX_NAME / 2 =>
        {
            (stem, &name[stem.len()..])
        }
        _ => (name, ""),
    };
    let mut end = stem.len().min(MAX_NAME - suffix.len() - extension.len());
    while !stem.is_char_boundary(end) {
        end -= 1;
    }
    format!("{}{suffix}{extension}", &stem[..end])
}

/// A new file in `folder` named for `name`, or, when that name is taken, `name (1)`, `name (2)`…
/// with the number before the extension, as browsers name downloads. It never opens a file that's
/// there, or follows a link someone left under that name.
fn create_unique(folder: &Path, name: &str) -> std::io::Result<(PathBuf, File)> {
    let name = file_name(name);
    let mut n = 0;
    loop {
        let path = folder.join(if n == 0 {
            name.clone()
        } else {
            with_suffix(&name, &format!(" ({n})"))
        });
        match File::create_new(&path) {
            Ok(file) => return Ok((path, file)),
            Err(error) if error.kind() == ErrorKind::AlreadyExists => n += 1,
            Err(error) => return Err(error),
        }
    }
}

/// Marks a saved file as downloaded (quarantined), so Gatekeeper checks an app or script someone
/// mailed before it runs.
#[cfg(target_os = "macos")]
fn quarantine(path: &Path) {
    use objc2_foundation::{NSDictionary, NSURL, NSURLQuarantinePropertiesKey, ns_string};
    let Some(url) = NSURL::from_file_path(path) else {
        return;
    };
    let properties = NSDictionary::from_slices(
        &[
            ns_string!("LSQuarantineAgentName"),
            ns_string!("LSQuarantineType"),
        ],
        &[
            ns_string!("Examail"),
            ns_string!("LSQuarantineTypeWebDownload"),
        ],
    );
    // SAFETY: the quarantine properties are a dictionary of strings, the type the key takes.
    if let Err(error) = unsafe {
        url.setResourceValue_forKey_error(Some(&properties), NSURLQuarantinePropertiesKey)
    } {
        log::warn!("quarantining a download: {error}");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn archives_only_download() {
        assert!(is_archive("photos.zip"));
        assert!(is_archive("Backup.TAR.GZ"));
        assert!(!is_archive("Board deck.pdf"));
        assert!(!is_archive("zip"));
        assert!(!is_archive("notes.zip copy"));
    }

    #[test]
    fn previews_are_cached_by_message_part() {
        let attachment = |attachment_id: &str, filename: &str| Attachment {
            message_id: "18f3a".into(),
            attachment_id: Some(attachment_id.into()),
            part_id: "1.2".into(),
            filename: filename.into(),
            mime_type: "application/pdf".into(),
            size: 3,
            content_id: None,
            inline: false,
        };
        let cache = Path::new("/tmp/cache");
        // Gmail's attachment id changes on every fetch, but the cached file doesn't.
        assert_eq!(
            cache_file(cache, &attachment("a", "Board.pdf")),
            cache_file(cache, &attachment("b", "Board.pdf"))
        );
        assert_eq!(
            cache_file(cache, &attachment("a", "Board.pdf")),
            Path::new("/tmp/cache/18f3a-1.2/Board.pdf")
        );
        assert_eq!(
            cache_file(cache, &attachment("a", "../../.zshrc")),
            Path::new("/tmp/cache/18f3a-1.2/_.._.zshrc")
        );
    }

    #[test]
    fn names_files_the_way_chromium_does() {
        assert_eq!(file_name("Board deck.pdf"), "Board deck.pdf");
        assert_eq!(file_name("/etc/passwd"), "_etc_passwd");
        assert_eq!(file_name("a:b|c?.txt"), "a_b_c_.txt");
        assert_eq!(
            file_name("invoice\u{202e}fdp.exe"),
            "invoice_fdp.exe",
            "no right-to-left disguise"
        );
        assert_eq!(file_name(" .hidden "), "hidden", "no hidden files");
        assert_eq!(file_name(".."), "attachment");
        let long = file_name(&format!("{}.pdf", "é".repeat(200)));
        assert!(long.len() <= MAX_NAME && long.ends_with("é.pdf"), "{long}");
        assert!(with_suffix(&long, " (12)").len() <= MAX_NAME);
        assert!(with_suffix(&long, " (12)").ends_with(" (12).pdf"));
    }

    #[test]
    #[cfg(unix)]
    fn downloads_never_overwrite_or_follow_links() {
        let folder =
            std::env::temp_dir().join(format!("exo-downloads-test-{}", std::process::id()));
        std::fs::create_dir_all(&folder).unwrap();
        let outside = folder.join("outside");
        std::os::unix::fs::symlink(&outside, folder.join("deck.pdf")).unwrap();
        let first = Target::Downloads(folder.clone())
            .save("deck.pdf", b"one")
            .unwrap();
        let second = Target::Downloads(folder.clone())
            .save("deck.pdf", b"two")
            .unwrap();
        assert_eq!(
            first,
            folder.join("deck (1).pdf"),
            "a dangling link keeps its name and isn't followed"
        );
        assert_eq!(second, folder.join("deck (2).pdf"));
        assert!(!outside.exists());
        assert_eq!(std::fs::read(&second).unwrap(), b"two");
        std::fs::remove_dir_all(folder).unwrap();
    }

    #[test]
    fn the_error_says_whether_gmail_or_the_disk_failed() {
        assert_eq!(
            Failure::Fetch.text("deck.pdf"),
            "Failed downloading deck.pdf"
        );
        assert_eq!(
            Failure::Save(ErrorKind::PermissionDenied).text("deck.pdf"),
            "Download failed: not allowed to save to the selected download folder"
        );
        assert_eq!(
            Failure::Save(ErrorKind::StorageFull).text("deck.pdf"),
            "Download failed: disk space insufficient"
        );
        assert_eq!(
            Failure::Save(ErrorKind::Other).text("deck.pdf"),
            "Download failed: could not save file"
        );
    }

    #[test]
    #[cfg(unix)]
    fn a_folder_that_refuses_writes_is_a_permission_failure() {
        // Covers what macOS answers (EPERM) when it keeps Exo out of ~/Downloads. A read-only
        // folder answers EACCES, which is the same kind.
        use std::os::unix::fs::PermissionsExt as _;
        let folder =
            std::env::temp_dir().join(format!("exo-downloads-denied-{}", std::process::id()));
        std::fs::create_dir_all(&folder).unwrap();
        std::fs::set_permissions(&folder, std::fs::Permissions::from_mode(0o555)).unwrap();
        let target = Target::Downloads(folder.clone());
        let error = target.save("deck.pdf", b"pdf").unwrap_err();
        assert_eq!(
            target.failed(&error),
            Failure::Save(ErrorKind::PermissionDenied)
        );
        let eperm = std::io::Error::from_raw_os_error(1);
        assert_eq!(
            target.failed(&eperm),
            Failure::Save(ErrorKind::PermissionDenied),
            "EPERM"
        );
        std::fs::set_permissions(&folder, std::fs::Permissions::from_mode(0o755)).unwrap();
        std::fs::remove_dir_all(folder).unwrap();
    }

    #[test]
    fn a_cached_preview_is_reused() {
        let cache = std::env::temp_dir().join(format!("exo-preview-test-{}", std::process::id()));
        let path = cache.join("18f3a-1.2").join("Board.pdf");
        assert!(!path.is_file());
        assert_eq!(
            Target::Cache(path.clone())
                .save("Board.pdf", b"pdf")
                .unwrap(),
            path
        );
        assert!(
            path.is_file(),
            "the next click previews it without fetching"
        );
        assert_eq!(std::fs::read(&path).unwrap(), b"pdf");
        std::fs::remove_dir_all(cache).unwrap();
    }

    #[test]
    fn preview_cache_paths_never_escape_cache_folder() {
        let root = std::env::temp_dir().join(format!(
            "exo-preview-traversal-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        let cache = root.join("cache");
        let secret = root.join("secret.txt");
        std::fs::create_dir_all(&cache).unwrap();
        std::fs::write(&secret, b"keep").unwrap();

        for (message_id, part_id) in [
            ("../../outside", "1"),
            ("18f3a", "../../secret"),
            ("..\\..\\win", "..\\part"),
            ("..", "."),
        ] {
            let attachment = Attachment {
                message_id: message_id.into(),
                attachment_id: Some("att".into()),
                part_id: part_id.into(),
                filename: "../../secret.txt".into(),
                mime_type: "text/plain".into(),
                size: 4,
                content_id: None,
                inline: false,
            };
            let path = cache_file(&cache, &attachment);
            assert!(within_cache(&cache, &path), "{path:?}");
        }

        for draft_id in ["../../secret", "../outside/id", "..\\escape", ".."] {
            let path = draft_cache_file(&cache, draft_id, "../secret.txt");
            assert!(within_cache(&cache, &path), "{path:?}");

            let draft = exo_store::DraftAttachment {
                uuid: draft_id.into(),
                name: "secret.txt".into(),
                mime_type: "text/plain".into(),
                size: 4,
                content_id: None,
                source: exo_store::AttachmentSource::Local,
            };
            assert!(Part::of_draft(&draft, &cache).is_none());
        }

        assert!(!within_cache(&cache, &cache));
        assert!(!within_cache(&cache, &secret));
        assert!(!within_cache(&cache, &cache.join("..").join("secret.txt")));
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn preview_selection_filters_to_images_and_emls_and_tracks_target_index() {
        let part = |part_id: &str, filename: &str, mime_type: &str| Attachment {
            message_id: "m1".into(),
            attachment_id: None,
            part_id: part_id.into(),
            filename: filename.into(),
            mime_type: mime_type.into(),
            size: 10,
            content_id: None,
            inline: false,
        };
        let pdf = part("1", "deck.pdf", "application/pdf");
        let png = part("2", "chart.png", "image/png");
        let eml = part("3", "forwarded.eml", "message/rfc822");
        let all = [pdf, png.clone(), eml.clone()];
        let (files, start) = preview_selection(&all, &eml);
        assert_eq!(files, vec![png, eml]);
        assert_eq!(start, 1);
    }

    #[test]
    fn extension_lowercases_and_rejects_whitespace_or_empty() {
        assert_eq!(extension("Report.PDF").as_deref(), Some("pdf"));
        assert_eq!(
            extension("Room block confirmed.eml").as_deref(),
            Some("eml")
        );
        assert_eq!(extension("Backup.TAR.GZ").as_deref(), Some("gz"));
        assert_eq!(extension("no_extension"), None);
        assert_eq!(extension("trailing."), None);
        assert_eq!(extension("notes.zip copy"), None);
        assert_eq!(extension("notes.pdf\t"), None);
    }
}

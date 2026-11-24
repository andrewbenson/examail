//! What Google says about people beyond their names: the photos, companies and job titles of the
//! accounts' saved contacts and Workspace colleagues, which the contact pane, the calendar's
//! guests and mail notifications show.
//!
//! Each account hands over its people whenever Google lists them ([`Photos::publish`]). A photo
//! loads the first time something draws it ([`photo`]): from the account's photo cache on disk,
//! else from Google, saving it there. Only Google's photo hosts are ever asked, so a contact
//! can't make Exo load an image from anywhere else.

use std::{
    collections::HashMap,
    path::{Path, PathBuf},
    sync::{Arc, Mutex, PoisonError},
    time::Duration,
};

use exo_store::GoogleContact;
use exo_ui::AvatarPhoto;
use sha2::{Digest, Sha256};

use crate::{
    account::Loader,
    avatar_cache::{AvatarCache, Lookup},
};

pub(crate) mod listing;

/// The size photos are asked for (Google's `=s{n}`), and brands' logos drawn at: the contact
/// pane's 56px at 2×.
pub(crate) const SIZE: u32 = 112;
/// Where the photos handed to notifications are written, in the temp directory.
const NOTIFICATION_PHOTOS: &str = "ExoNotificationPhotos";
/// How long a notification waits for a photo that isn't saved yet, or a brand's logo.
pub(crate) const NOTIFICATION_WAIT: Duration = Duration::from_secs(5);
const PHOTOS_CACHE_ID: &str = "exo::people::photos";

/// What Google says about someone, besides their name.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Person {
    /// Their photo's URL, as Google lists it.
    pub photo: Option<String>,
    pub company: Option<String>,
    pub title: Option<String>,
}

/// Where an account's people's photos come from and where they're kept.
#[derive(Clone)]
pub(crate) struct Photos {
    ctx: egui::Context,
    account: String,
    loader: Loader,
    /// The photo cache.
    dir: PathBuf,
    /// Gmail is served from this Mac (mock-servers), so its photos are too.
    local: bool,
}

/// The people the accounts listed. It lives in the context's data, so whatever draws a person can
/// find it.
#[derive(Default)]
struct Registry {
    /// Each account's people that have something to show, keyed by lower-cased account address and
    /// then by lower-cased contact address, with where their photos come from.
    accounts: HashMap<String, (Photos, HashMap<String, Person>)>,
}

#[derive(Clone, Default)]
struct ActiveAccount(Option<String>);

type Shared = Arc<Mutex<Registry>>;

fn registry(ctx: &egui::Context) -> Shared {
    ctx.data_mut(|data| {
        data.get_temp_mut_or_default::<Shared>(egui::Id::new("exo::people"))
            .clone()
    })
}

fn active_id() -> egui::Id {
    egui::Id::new("exo::people::active")
}

fn active_account(ctx: &egui::Context) -> Option<String> {
    ctx.data(|data| data.get_temp::<ActiveAccount>(active_id()))
        .and_then(|active| active.0)
}

/// Records which signed-in account is drawing on `ctx` this frame, scoping [`person`] and
/// [`photo`] so contacts never leak across accounts.
pub(crate) fn set_active_account(ctx: &egui::Context, account: &str) {
    let lower = account.to_lowercase();
    ctx.data_mut(|data| data.insert_temp(active_id(), ActiveAccount(Some(lower))));
}

fn photo_cache(ctx: &egui::Context) -> AvatarCache<String> {
    AvatarCache::of(ctx, PHOTOS_CACHE_ID)
}

impl Photos {
    /// `account`'s photos, fetched by `loader` and kept in `dir`. With `local`, they may come
    /// from this Mac.
    pub(crate) fn new(
        ctx: &egui::Context,
        account: &str,
        loader: Loader,
        dir: PathBuf,
        local: bool,
    ) -> Self {
        Self {
            ctx: ctx.clone(),
            account: account.to_owned(),
            loader,
            dir,
            local,
        }
    }

    /// The account's address.
    pub(crate) fn account(&self) -> &str {
        &self.account
    }

    /// Shows `contacts`, the account's latest listing, from now on. An address takes the first
    /// photo, and the first company and title, its entries have.
    pub(crate) fn publish<'a>(&self, contacts: impl IntoIterator<Item = &'a GoogleContact>) {
        let mut people: HashMap<String, Person> = HashMap::new();
        for contact in contacts {
            if contact.photo.is_none() && contact.company.is_none() && contact.title.is_none() {
                continue;
            }
            let person = people.entry(contact.email.to_lowercase()).or_default();
            if person.photo.is_none() {
                person.photo.clone_from(&contact.photo);
            }
            if person.company.is_none() && person.title.is_none() {
                person.company.clone_from(&contact.company);
                person.title.clone_from(&contact.title);
            }
        }
        let shared = registry(&self.ctx);
        let mut registry = shared.lock().unwrap_or_else(PoisonError::into_inner);
        registry
            .accounts
            .insert(self.account.to_lowercase(), (self.clone(), people));
        drop(registry);
        // Photos that couldn't be had are asked for again.
        photo_cache(&self.ctx).clear_missing();
        self.ctx.request_repaint();
    }
}

/// `account` closed: its people and brand logos show no more.
pub(crate) fn forget(ctx: &egui::Context, account: &str) {
    let lower = account.to_lowercase();
    let shared = registry(ctx);
    let mut registry = shared.lock().unwrap_or_else(PoisonError::into_inner);
    registry.accounts.remove(&lower);
    drop(registry);
    ctx.data_mut(|data| {
        if data
            .get_temp::<ActiveAccount>(active_id())
            .and_then(|active| active.0)
            .as_deref()
            == Some(lower.as_str())
        {
            data.insert_temp(active_id(), ActiveAccount(None));
        }
    });
    crate::brands::forget(ctx, account);
}

/// What Google says about `email` in `account`.
pub(crate) fn person_for(ctx: &egui::Context, account: &str, email: &str) -> Option<Person> {
    let shared = registry(ctx);
    let registry = shared.lock().unwrap_or_else(PoisonError::into_inner);
    let (_, people) = registry.accounts.get(&account.to_lowercase())?;
    people.get(&email.to_lowercase()).cloned()
}

/// What Google says about `email` in the active account (or the lone registered account when no
/// active account has been marked yet).
pub fn person(ctx: &egui::Context, email: &str) -> Option<Person> {
    if let Some(active) = active_account(ctx) {
        return person_for(ctx, &active, email);
    }
    let shared = registry(ctx);
    let registry = shared.lock().unwrap_or_else(PoisonError::into_inner);
    if registry.accounts.len() != 1 {
        return None;
    }
    let email = email.to_lowercase();
    registry
        .accounts
        .values()
        .next()
        .and_then(|(_, people)| people.get(&email).cloned())
}

/// `email`'s photo in `account`, once it's loaded. The first ask starts loading it.
pub(crate) fn photo_for(ctx: &egui::Context, account: &str, email: &str) -> Option<AvatarPhoto> {
    let (photos, url) = {
        let shared = registry(ctx);
        let registry = shared.lock().unwrap_or_else(PoisonError::into_inner);
        let (photos, people) = registry.accounts.get(&account.to_lowercase())?;
        let url = people.get(&email.to_lowercase())?.photo.as_deref()?;
        (photos.clone(), sized(url))
    };
    photo_from_cache(ctx, photos, url)
}

/// `email`'s photo in the active account (or the lone registered account when no active account
/// has been marked yet), once it's loaded. The first ask starts loading it.
pub fn photo(ctx: &egui::Context, email: &str) -> Option<AvatarPhoto> {
    if let Some(active) = active_account(ctx) {
        return photo_for(ctx, &active, email);
    }
    let (photos, url) = {
        let shared = registry(ctx);
        let registry = shared.lock().unwrap_or_else(PoisonError::into_inner);
        if registry.accounts.len() != 1 {
            return None;
        }
        let email = email.to_lowercase();
        let (photos, people) = registry.accounts.values().next()?;
        let url = people.get(&email)?.photo.as_deref()?;
        (photos.clone(), sized(url))
    };
    photo_from_cache(ctx, photos, url)
}

fn photo_from_cache(ctx: &egui::Context, photos: Photos, url: String) -> Option<AvatarPhoto> {
    let cache = photo_cache(ctx);
    match cache.get_or_claim(url.clone()) {
        Lookup::Shown(photo) => Some(photo),
        Lookup::Claimed => {
            load(cache, photos, url);
            None
        }
        Lookup::Unavailable => None,
    }
}

/// Paints `email`'s avatar filling `rect`: the default avatar (a `small` one's colours, as the
/// calendar's guests have), with their photo (else the on-screen account's logo for their brand)
/// over it once it's loaded, snapped to the screen's pixels as a browser paints a background.
pub(crate) fn paint_avatar(painter: &egui::Painter, rect: egui::Rect, email: &str, small: bool) {
    let brand = crate::brands::active_logo(painter.ctx(), email);
    let photo = photo(painter.ctx(), email);
    paint_avatar_photo(painter, rect, photo.or(brand), small);
}

/// [`paint_avatar`] scoped explicitly to `account`, with an explicit brand logo fallback (`None`
/// when the message on screen didn't pass DMARC).
pub(crate) fn paint_avatar_with_brand_for(
    painter: &egui::Painter,
    rect: egui::Rect,
    account: &str,
    email: &str,
    brand: Option<&AvatarPhoto>,
    small: bool,
) {
    let photo = photo_for(painter.ctx(), account, email).or_else(|| brand.cloned());
    paint_avatar_photo(painter, rect, photo, small);
}

fn paint_avatar_photo(
    painter: &egui::Painter,
    rect: egui::Rect,
    photo: Option<AvatarPhoto>,
    small: bool,
) {
    exo_ui::paint_default_avatar(painter, rect, small);
    if let Some(photo) = photo {
        let rect = egui::emath::GuiRounding::round_to_pixels(rect, painter.pixels_per_point());
        exo_ui::paint_photo(painter, rect, &photo);
    }
}

/// A copy of `email`'s photo for the notification `key` to show: the account's own when `email` is
/// one of the accounts, else a contact's, from the cache or, when it isn't there, from Google if it
/// comes within [`NOTIFICATION_WAIT`]. macOS moves the copy into its own store.
pub(crate) async fn notification_photo(
    ctx: &egui::Context,
    email: &str,
    key: &str,
) -> Option<PathBuf> {
    let photo = match crate::profile_pictures::saved(ctx, email) {
        Some(photo) => photo.to_vec(),
        None => contact_photo(ctx, email).await?,
    };
    notification_copy(&photo, key)
}

/// A copy of `photo` for the notification `key` to show, which macOS moves into its own store.
pub(crate) fn notification_copy(photo: &[u8], key: &str) -> Option<PathBuf> {
    // macOS knows an image's type by its extension.
    let format = image::guess_format(photo).ok()?;
    let extension = format.extensions_str().first()?;
    let file = std::env::temp_dir()
        .join(NOTIFICATION_PHOTOS)
        .join(format!("{}.{extension}", file_name(key)));
    save(&file, photo)
        .inspect_err(|error| log::warn!("copying a photo for a notification: {error}"))
        .ok()?;
    Some(file)
}

/// `email`'s photo, if Google has one of them: the saved copy, else Google's within
/// [`NOTIFICATION_WAIT`], which is saved and shows wherever they're drawn.
async fn contact_photo(ctx: &egui::Context, email: &str) -> Option<Vec<u8>> {
    let email = email.to_lowercase();
    let (photos, url) = {
        let shared = registry(ctx);
        let registry = shared.lock().unwrap_or_else(PoisonError::into_inner);
        registry.accounts.values().find_map(|(photos, people)| {
            let url = people.get(&email)?.photo.as_deref()?;
            Some((photos.clone(), sized(url)))
        })?
    };
    let cache = photo_cache(ctx);
    let shown = cache.is_shown(&url);
    let photo = tokio::time::timeout(NOTIFICATION_WAIT, fetch(&photos, &url))
        .await
        .inspect_err(|_| log::info!("a notification goes without its photo, which is slow to come"))
        .ok()??;
    if !shown {
        cache.finish_decode(url, "contact photo", &photo);
    }
    Some(photo)
}

/// Loads the photo at `url` into `cache`, or marks it missing.
fn load(cache: AvatarCache<String>, photos: Photos, url: String) {
    let (source, asked) = (photos.clone(), url.clone());
    photos.loader.run(
        move |_| async move { fetch(&source, &asked).await },
        move |photo| cache.finish_decode(url, "contact photo", &photo.unwrap_or_default()),
    );
}

/// The photo at `url`: the saved copy, else Google's, which is saved. `None` when Google doesn't
/// give an image, or `url` isn't one of its photos.
async fn fetch(photos: &Photos, url: &str) -> Option<Vec<u8>> {
    let file = photos.dir.join(file_name(url));
    if let Ok(saved) = std::fs::read(&file) {
        return Some(saved);
    }
    if !is_google_photo(url, photos.local) {
        log::warn!("not loading a contact photo from {url}");
        return None;
    }
    let photo = photos.loader.get(url).await?;
    image::load_from_memory(&photo).ok()?;
    if let Err(error) = save(&file, &photo) {
        log::warn!("saving a contact photo: {error}");
    }
    Some(photo)
}

/// Writes `bytes` to `file` through a unique temporary file, so concurrent saves never race and a
/// crash never leaves half a photo.
pub(crate) fn save(file: &Path, bytes: &[u8]) -> std::io::Result<()> {
    fastframe_file::write(file, bytes)
}

/// `url` asking for [`SIZE`] pixels in place of the size Google lists (`=s100`).
fn sized(url: &str) -> String {
    match url.rsplit_once("=s") {
        Some((base, size)) if !size.is_empty() && size.bytes().all(|b| b.is_ascii_digit()) => {
            format!("{base}=s{SIZE}")
        }
        _ => url.to_owned(),
    }
}

/// A file name for `text` (a photo's or logo's URL, a notification's key).
pub(crate) fn file_name(text: &str) -> String {
    use std::fmt::Write as _;
    let digest = Sha256::digest(text.as_bytes());
    let mut hex = String::with_capacity(64);
    for byte in digest {
        let _ = write!(hex, "{byte:02x}");
    }
    hex
}

/// Whether `url` is one of Google's photos (`https://lh3.googleusercontent.com/…`), or, with
/// `local`, from this Mac (mock-servers serves its own).
fn is_google_photo(url: &str, local: bool) -> bool {
    let Ok(url) = url::Url::parse(url) else {
        return false;
    };
    match url.host() {
        Some(url::Host::Domain(host)) if url.scheme() == "https" => {
            host == "googleusercontent.com" || host.ends_with(".googleusercontent.com")
        }
        Some(url::Host::Ipv4(ip)) => local && ip.is_loopback(),
        Some(url::Host::Domain("localhost")) => local,
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_googles_photo_hosts_are_asked() {
        let photo = "https://lh3.googleusercontent.com/a-/ALV-UjX=s100";
        assert!(is_google_photo(photo, false));
        assert!(!is_google_photo(
            "http://lh3.googleusercontent.com/a-/ALV-UjX=s100",
            false
        ));
        assert!(!is_google_photo(
            "https://googleusercontent.com.example.com/a=s100",
            false
        ));
        assert!(!is_google_photo("https://example.com/me.jpg", false));
        assert!(!is_google_photo(
            "http://127.0.0.1:8787/__mock/photo/1",
            false
        ));
        assert!(is_google_photo(
            "http://127.0.0.1:8787/__mock/photo/1",
            true
        ));
    }

    #[test]
    fn photos_are_asked_for_at_twice_the_largest_avatar() {
        assert_eq!(
            sized("https://lh3.googleusercontent.com/a-/ALV-UjX=s100"),
            "https://lh3.googleusercontent.com/a-/ALV-UjX=s112"
        );
        assert_eq!(
            sized("https://lh3.googleusercontent.com/a/photo.jpg"),
            "https://lh3.googleusercontent.com/a/photo.jpg"
        );
        assert_eq!(
            file_name("abc"),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
    }

    #[test]
    fn notification_copies_keep_their_image_extension_without_partial_races() {
        let mut png = std::io::Cursor::new(Vec::new());
        image::RgbaImage::from_pixel(2, 2, image::Rgba([10, 20, 30, 255]))
            .write_to(&mut png, image::ImageFormat::Png)
            .expect("png");
        let png = png.into_inner();
        let key = format!(
            "notice-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        );
        let written = notification_copy(&png, &key).expect("copied");
        assert_eq!(
            written.extension().and_then(|ext| ext.to_str()),
            Some("png")
        );
        assert_eq!(std::fs::read(&written).expect("read"), png);
        let _ = std::fs::remove_file(written);
    }

    #[test]
    fn contacts_and_photos_stay_isolated_per_active_account() {
        let ctx = egui::Context::default();
        let dir = std::env::temp_dir().join(format!(
            "exo-people-isolation-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        let endpoints =
            gmail::Endpoints::mock(&"http://127.0.0.1:9/".parse().expect("mock origin"));
        let account = crate::account::Account::open(
            &dir,
            crate::account::Credentials {
                email: "alice@work.example".into(),
                endpoints,
                tokens: Arc::new(gmail::StaticToken("dev".into())),
            },
            ctx.clone(),
        )
        .expect("account opens");

        let mut png = std::io::Cursor::new(Vec::new());
        image::RgbaImage::from_pixel(2, 2, image::Rgba([40, 80, 120, 255]))
            .write_to(&mut png, image::ImageFormat::Png)
            .expect("png");
        let png = png.into_inner();

        let secret_url = "https://lh3.googleusercontent.com/secret=s100";
        let friend_url = "https://lh3.googleusercontent.com/friend=s100";
        let cache = photo_cache(&ctx);
        cache.finish_decode(sized(secret_url), "contact photo", &png);
        cache.finish_decode(sized(friend_url), "contact photo", &png);

        let photos_a = Photos::new(
            &ctx,
            "Alice@Work.Example",
            account.loader(),
            dir.join("a"),
            false,
        );
        let photos_b = Photos::new(
            &ctx,
            "bob@personal.example",
            account.loader(),
            dir.join("b"),
            false,
        );

        let secret = GoogleContact {
            person: "people/c1".into(),
            email: "secret@client.example".into(),
            name: Some("Secret Client".into()),
            photo: Some(secret_url.into()),
            company: Some("ClientCo".into()),
            title: Some("CEO".into()),
        };
        let friend = GoogleContact {
            person: "people/c2".into(),
            email: "friend@home.example".into(),
            name: Some("Best Friend".into()),
            photo: Some(friend_url.into()),
            company: Some("HomeCo".into()),
            title: None,
        };

        // Single registered account without explicit active account still resolves (unit-test compat).
        photos_a.publish([&secret]);
        assert_eq!(
            person(&ctx, "secret@client.example")
                .and_then(|p| p.company)
                .as_deref(),
            Some("ClientCo")
        );
        assert!(photo(&ctx, "secret@client.example").is_some());

        // With two accounts and no active account marked, unscoped lookups refuse to guess.
        photos_b.publish([&friend]);
        assert!(person(&ctx, "secret@client.example").is_none());
        assert!(photo(&ctx, "secret@client.example").is_none());

        // Scoping to account B hides account A's contacts and photos.
        set_active_account(&ctx, "bob@personal.example");
        assert!(person(&ctx, "secret@client.example").is_none());
        assert!(photo(&ctx, "secret@client.example").is_none());
        assert_eq!(
            person(&ctx, "friend@home.example")
                .and_then(|p| p.company)
                .as_deref(),
            Some("HomeCo")
        );
        assert!(photo(&ctx, "friend@home.example").is_some());

        // Explicit account-scoped lookups work regardless of which account is active.
        assert_eq!(
            person_for(&ctx, "alice@work.example", "secret@client.example")
                .and_then(|p| p.company)
                .as_deref(),
            Some("ClientCo")
        );
        assert!(photo_for(&ctx, "alice@work.example", "secret@client.example").is_some());
        assert!(person_for(&ctx, "bob@personal.example", "secret@client.example").is_none());
        assert!(photo_for(&ctx, "bob@personal.example", "secret@client.example").is_none());

        // Switching active account to A flips visibility cleanly, and forget clears active.
        set_active_account(&ctx, "alice@work.example");
        assert!(person(&ctx, "secret@client.example").is_some());
        assert!(person(&ctx, "friend@home.example").is_none());

        forget(&ctx, "alice@work.example");
        assert!(active_account(&ctx).is_none());
        assert!(person_for(&ctx, "alice@work.example", "secret@client.example").is_none());

        drop(account);
        let _ = std::fs::remove_dir_all(&dir);
    }
}

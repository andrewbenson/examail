//! The accounts' own Google profile photos, which show in the folder drawer's account row, the
//! Accounts palette and the Accounts dialog, with each account's initial on its color standing
//! in while there's none to show.
//!
//! Exo asks Google for the account's photo when an account comes on screen
//! ([`ProfilePictures::refresh`]). The photo is saved with the account list
//! ([`exo_auth::Auth::set_profile_picture`]), so the next launch shows it before Google answers,
//! and it goes when the account signs out. A fetch that fails keeps what was saved, and says
//! nothing.

use std::{
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
        mpsc,
    },
    time::Duration,
};

use exo_auth::Auth;
use exo_ui::AvatarPhoto;

use crate::avatar_cache::{self, AvatarCache};

/// How long the photo may take to download.
const DOWNLOAD_TIMEOUT: Duration = Duration::from_secs(30);
/// People is asked again while it can't be reached, up to 7 tries, a second apart and twice as
/// long each time, at most 32 s.
const ATTEMPTS: u32 = 7;
const FIRST_WAIT: Duration = Duration::from_secs(1);
const LONGEST_WAIT: Duration = Duration::from_secs(32);

const CACHE_ID: &str = "exo::profile_pictures";

/// `email`'s profile photo, once there's one to show.
pub fn photo(ctx: &egui::Context, email: &str) -> Option<AvatarPhoto> {
    AvatarCache::<String, Arc<[u8]>>::of(ctx, CACHE_ID).photo(email)
}

/// The file `email`'s profile photo was decoded from, once there's one to show.
pub(crate) fn saved(ctx: &egui::Context, email: &str) -> Option<Arc<[u8]>> {
    AvatarCache::<String, Arc<[u8]>>::of(ctx, CACHE_ID).extra(email)
}

/// Shows saved photos, refreshes them from Google and lets them go.
pub(crate) struct ProfilePictures {
    ctx: egui::Context,
    cache: AvatarCache<String, Arc<[u8]>>,
    runtime: Option<Arc<tokio::runtime::Runtime>>,
    /// Shows saved photos one after another, so the photo saved last is the one shown. It's
    /// started with the first.
    loader: Option<mpsc::Sender<Load>>,
    /// Reused HTTP client for downloading avatar images, keyed by `serves_gmail_locally`.
    http: Option<(bool, crate::remote_content::Client)>,
    /// Loads and fetches under way.
    busy: Arc<AtomicUsize>,
}

/// Show this account's saved photo, or its initial without one.
struct Load {
    auth: Auth,
    email: String,
    _busy: Busy,
}

/// Counts as work under way until it's dropped.
struct Busy(Arc<AtomicUsize>);

impl Busy {
    fn new(count: &Arc<AtomicUsize>) -> Self {
        count.fetch_add(1, Ordering::SeqCst);
        Self(count.clone())
    }
}

impl Drop for Busy {
    fn drop(&mut self) {
        self.0.fetch_sub(1, Ordering::SeqCst);
    }
}

/// Why a photo couldn't be had.
#[derive(Debug, thiserror::Error)]
enum Error {
    #[error("asking People for it: {0}")]
    People(#[from] gmail::Error),
    #[error("downloading it: {0}")]
    Download(#[from] crate::remote_content::Error),
    #[error("it isn't an image: {0}")]
    Image(#[from] image::ImageError),
}

impl ProfilePictures {
    /// The photos `ctx`'s avatars draw.
    #[cfg(test)]
    pub(crate) fn new(ctx: &egui::Context) -> Self {
        Self {
            ctx: ctx.clone(),
            cache: AvatarCache::of(ctx, CACHE_ID),
            runtime: None,
            loader: None,
            http: None,
            busy: Arc::default(),
        }
    }

    /// The photos `ctx`'s avatars draw, fetching remote updates on `runtime`.
    pub(crate) fn with_runtime(ctx: &egui::Context, runtime: Arc<tokio::runtime::Runtime>) -> Self {
        Self {
            ctx: ctx.clone(),
            cache: AvatarCache::of(ctx, CACHE_ID),
            runtime: Some(runtime),
            loader: None,
            http: None,
            busy: Arc::default(),
        }
    }

    /// Whether nothing is being loaded or fetched. A fetch waiting to try again doesn't count.
    /// For screenshots and tests.
    pub(crate) fn is_idle(&self) -> bool {
        self.busy.load(Ordering::SeqCst) == 0
    }

    /// Shows the saved photos of `emails` that weren't asked for yet.
    pub(crate) fn load<'a>(&mut self, auth: &Auth, emails: impl IntoIterator<Item = &'a str>) {
        for email in emails {
            if self.cache.claim(email.to_owned()) {
                self.show_saved(auth, email);
            }
        }
    }

    /// `email` signed out, which deleted its photo, so its initial shows from now on.
    pub(crate) fn forget(&mut self, auth: &Auth, email: &str) {
        self.cache.unclaim(email);
        self.show_saved(auth, email);
    }

    /// Asks Google for `email`'s photo, off the UI thread, and saves and shows what it gets, which
    /// is the People API's primary photo, or none when that's Google's generated default.
    /// Meanwhile, and when Google can't be reached, the saved one shows.
    pub(crate) fn refresh(&mut self, auth: &Auth, email: &str, tokens: exo_auth::Tokens) {
        self.load(auth, [email]);
        let runtime = match &self.runtime {
            Some(runtime) => runtime.clone(),
            None => match crate::account::shared_runtime() {
                Ok(runtime) => runtime,
                Err(error) => {
                    log::error!("starting a profile photo's fetch: {error}");
                    return;
                }
            },
        };
        let local_gmail = crate::remote_content::serves_gmail_locally(auth.endpoints());
        let http = match &self.http {
            Some((cached_local, client)) if *cached_local == local_gmail => client.clone(),
            _ => {
                let client = crate::remote_content::Client::new(DOWNLOAD_TIMEOUT, local_gmail);
                self.http = Some((local_gmail, client.clone()));
                client
            }
        };
        let (loader, busy) = (self.loader().clone(), Busy::new(&self.busy));
        let (auth, email) = (auth.clone(), email.to_owned());
        runtime.spawn(async move {
            if fetch_and_save(&auth, &email, tokens, &http, &busy.0).await {
                // Busy until it shows.
                let _ = loader.send(Load {
                    _busy: Busy::new(&busy.0),
                    auth,
                    email,
                });
            }
        });
    }

    fn show_saved(&mut self, auth: &Auth, email: &str) {
        let load = Load {
            auth: auth.clone(),
            email: email.to_owned(),
            _busy: Busy::new(&self.busy),
        };
        // Fails only when the loader couldn't start, which it logged.
        let _ = self.loader().send(load);
    }

    /// The loader, started the first time.
    fn loader(&mut self) -> &mpsc::Sender<Load> {
        self.loader.get_or_insert_with(|| {
            let (sender, loads) = mpsc::channel::<Load>();
            let (ctx, cache) = (self.ctx.clone(), self.cache.clone());
            let load = move || {
                for Load { auth, email, _busy } in loads {
                    let photo = auth
                        .profile_picture(&email)
                        .inspect_err(|error| log::warn!("reading a profile photo: {error}"))
                        .ok()
                        .flatten();
                    let texture = photo.and_then(|photo| {
                        let decoded = avatar_cache::try_decode(&ctx, "profile photo", &photo)
                            .inspect_err(|error| {
                                log::warn!("a saved profile photo isn't an image: {error}")
                            })
                            .ok()?;
                        Some((decoded, Arc::from(photo)))
                    });
                    cache.set(email, texture);
                }
            };
            if let Err(error) = std::thread::Builder::new()
                .name("exo-profile-photos".into())
                .spawn(load)
            {
                log::error!("starting the profile photos' loader: {error}");
            }
            sender
        })
    }
}

/// Fetches `email`'s photo and saves it. Returns whether that changed the saved one.
async fn fetch_and_save(
    auth: &Auth,
    email: &str,
    tokens: exo_auth::Tokens,
    http: &crate::remote_content::Client,
    busy: &Arc<AtomicUsize>,
) -> bool {
    let google = gmail::Client::new(auth.endpoints().clone(), tokens);
    let photo = match fetch(&google, http, busy).await {
        Ok(photo) => photo,
        Err(error) => {
            log::warn!("fetching a profile photo: {error}");
            return false;
        }
    };
    let (auth, email) = (auth.clone(), email.to_owned());
    tokio::task::spawn_blocking(move || {
        if let Some(bytes) = &photo
            && let Err(error) = image::load_from_memory(bytes)
        {
            log::warn!("fetching a profile photo: {}", Error::Image(error));
            return false;
        }
        if auth
            .profile_picture(&email)
            .is_ok_and(|saved| saved == photo)
        {
            return false;
        }
        match auth.set_profile_picture(&email, photo.as_deref()) {
            Ok(()) => true,
            // It signed out meanwhile.
            Err(exo_auth::Error::UnknownAccount(_)) => false,
            Err(error) => {
                log::error!("saving a profile photo: {error}");
                false
            }
        }
    })
    .await
    .unwrap_or(false)
}

/// The account's photo, which is People's primary photo unless it's Google's generated default.
/// Its URL asks for 100px (`=s100`), which loads as it is and is scaled down to the avatar.
async fn fetch(
    google: &gmail::Client,
    http: &crate::remote_content::Client,
    busy: &Arc<AtomicUsize>,
) -> Result<Option<Vec<u8>>, Error> {
    let me = people_photos(google, busy).await?;
    let primary = me.photos.into_iter().find(|photo| {
        photo
            .metadata
            .as_ref()
            .and_then(|metadata| metadata.primary)
            == Some(true)
    });
    let Some(url) = primary
        .filter(|photo| photo.default != Some(true))
        .and_then(|photo| photo.url)
    else {
        return Ok(None);
    };
    let response = http
        .get(&url)
        .await?
        .error_for_status()
        .map_err(crate::remote_content::Error::from)?;
    let photo = response
        .bytes()
        .await
        .map_err(crate::remote_content::Error::from)?
        .to_vec();
    Ok(Some(photo))
}

/// `people/me`'s photos, asked again while Google can't be reached. Waiting isn't work under way.
async fn people_photos(
    google: &gmail::Client,
    busy: &Arc<AtomicUsize>,
) -> Result<gmail::people::Person, gmail::Error> {
    let (mut attempt, mut wait) = (1, FIRST_WAIT);
    loop {
        match google.people().me("photos").await {
            Err(gmail::Error::Http(error)) if attempt < ATTEMPTS => {
                log::debug!("asking for a profile photo again in {wait:?}: {error}");
                busy.fetch_sub(1, Ordering::SeqCst);
                tokio::time::sleep(wait).await;
                busy.fetch_add(1, Ordering::SeqCst);
                (attempt, wait) = (attempt + 1, (wait * 2).min(LONGEST_WAIT));
            }
            result => return result,
        }
    }
}

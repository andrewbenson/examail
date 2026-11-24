//! The accounts on this device: signing them in and out, and their tokens.

use std::collections::HashMap;
use std::io;
use std::path::Path;
use std::sync::{Arc, Mutex, MutexGuard};

use gmail::Endpoints;
use gmail::oauth::OAuthClient;
use url::Url;

use crate::accounts::{Registry, normalize_email};
use crate::profile_pictures::ProfilePictures;
use crate::sign_in::{self, Grant, Purpose};
use crate::{Account, ClientConfig, Error, SecretStore, Tokens};

/// Exo's signed-in accounts. Cheap to clone, and clones share everything.
#[derive(Clone)]
pub struct Auth(Arc<Inner>);

struct Inner {
    client: ClientConfig,
    oauth: OAuthClient,
    secrets: Box<dyn SecretStore>,
    pictures: ProfilePictures,
    /// Serializes multi-step secret-store and account-list updates (`save`, `forget`,
    /// `sign_in_calendar`, `forget_calendar`). Always acquired before `state`.
    ops: Mutex<()>,
    state: Mutex<State>,
}

struct State {
    registry: Registry,
    /// One provider per account, so a sign-in reaches the clients already using it.
    tokens: HashMap<String, Tokens>,
}

impl std::fmt::Debug for Auth {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Auth")
            .field("client", &self.0.client)
            .finish_non_exhaustive()
    }
}

impl Auth {
    /// The accounts saved in `state_dir` (Exo's state folder), signing in with `client` and
    /// keeping refresh tokens in `secrets` (the [`Keychain`](crate::Keychain)).
    ///
    /// # Errors
    /// [`Error::Accounts`] when the account list can't be read.
    pub fn open(
        client: ClientConfig,
        state_dir: &Path,
        secrets: impl SecretStore,
    ) -> Result<Self, Error> {
        Ok(Self(Arc::new(Inner {
            oauth: client.oauth(),
            client,
            secrets: Box::new(secrets),
            pictures: ProfilePictures::new(state_dir),
            ops: Mutex::new(()),
            state: Mutex::new(State {
                registry: Registry::load(state_dir)?,
                tokens: HashMap::new(),
            }),
        })))
    }

    /// The Google APIs the accounts' clients talk to.
    #[must_use]
    pub fn endpoints(&self) -> &Endpoints {
        self.0.client.endpoints()
    }

    /// The accounts, in the user's order. Signed-out ones (whose [`Tokens`] need a new sign-in)
    /// stay listed.
    #[must_use]
    pub fn accounts(&self) -> Vec<Account> {
        self.state().registry.accounts().to_vec()
    }

    /// The account to open on launch: the one used last, or else the first not signed out.
    #[must_use]
    pub fn default_account(&self) -> Option<Account> {
        self.state().registry.default_account().cloned()
    }

    /// `email` is the account on screen now, so launch opens it next time.
    ///
    /// # Errors
    /// [`Error::UnknownAccount`], or [`Error::Accounts`] when the list can't be saved.
    pub fn use_account(&self, email: &str) -> Result<(), Error> {
        let email = normalize_email(email);
        self.state().registry.use_account(&email)
    }

    /// Google refused `email`'s grant ([`Tokens::needs_sign_in`]). The account stays listed but
    /// opens the sign-in screen until it signs in again.
    ///
    /// # Errors
    /// [`Error::UnknownAccount`], or [`Error::Accounts`] when the list can't be saved.
    pub fn mark_signed_out(&self, email: &str) -> Result<(), Error> {
        let email = normalize_email(email);
        self.state().registry.sign_out(&email)
    }

    /// Sets the color `email` shows in, by name. `None` computes it from the address.
    ///
    /// # Errors
    /// [`Error::UnknownAccount`], or [`Error::Accounts`] when the list can't be saved.
    pub fn set_color(&self, email: &str, color: Option<String>) -> Result<(), Error> {
        let email = normalize_email(email);
        self.state().registry.set_color(&email, color)
    }

    /// `email`'s profile photo as last saved ([`Auth::set_profile_picture`]), or `None` without
    /// one.
    ///
    /// # Errors
    /// [`Error::ProfilePicture`] when it can't be read.
    pub fn profile_picture(&self, email: &str) -> Result<Option<Vec<u8>>, Error> {
        let email = normalize_email(email);
        self.0.pictures.get(&email)
    }

    /// Saves `email`'s profile photo, the image as Google serves it. `None` means Google has none
    /// for it. The photo stays until it's replaced or the account signs out ([`Auth::forget`]).
    ///
    /// # Errors
    /// [`Error::UnknownAccount`], also for an account that signed out while its photo was
    /// fetched, or [`Error::ProfilePicture`] when it can't be written.
    pub fn set_profile_picture(&self, email: &str, photo: Option<&[u8]>) -> Result<(), Error> {
        let email = normalize_email(email);
        // This holds the list's lock, so a sign-out can't come between the check and the write.
        let state = self.state();
        if !state.registry.contains(&email) {
            return Err(Error::UnknownAccount(email));
        }
        self.0.pictures.set(&email, photo)
    }

    /// Signs an account in: opens Google's consent screen with `open` (which should show the URL
    /// in the system browser), waits up to [`TIMEOUT`](crate::TIMEOUT) for the redirect, and saves
    /// the account. A new account goes at the end of the list. An account that's already listed
    /// (a re-authentication) keeps its place, and its [`Tokens`] pick up the new grant. Either
    /// way it becomes the account used last.
    ///
    /// `login_hint` preselects an account at Google (`Sign in as {email}`). The user can still
    /// pick another one, which is the account returned. Drop the future to cancel.
    ///
    /// # Errors
    /// [`Error::AccessDenied`] when the user declines (the sign-in screen says nothing then),
    /// [`Error::TimedOut`], [`Error::MissingScopes`], [`Error::NoMailbox`], and the transport
    /// and storage errors. Redirects carrying another attempt's `state` are turned away without
    /// ending this one.
    pub async fn sign_in(
        &self,
        login_hint: Option<&str>,
        open: impl FnOnce(&Url) -> io::Result<()>,
    ) -> Result<Account, Error> {
        sign_in::authorize(
            &self.0.oauth,
            self.endpoints(),
            Purpose::Mail,
            login_hint,
            open,
            |grant| self.save(grant),
        )
        .await
    }

    /// Signs a Google account in for the calendar of mail account `owner`. It's the same flow as
    /// [`Auth::sign_in`], asking for [`CALENDAR_SCOPES`](crate::CALENDAR_SCOPES) alone. The grant
    /// belongs to `owner` and is kept until [`Auth::forget_calendar`] or until `owner` signs out.
    /// Signing the account in again replaces the grant, and its [`Tokens`] pick the new one up.
    /// Returns the address Google signed in. When that's `owner`'s own address, nothing is saved,
    /// because the mail grant already covers its calendar.
    ///
    /// # Errors
    /// [`Error::WrongAccount`] when `login_hint` names an account and the user picked another,
    /// [`Error::UnknownAccount`] for an `owner` that isn't listed, and the errors of
    /// [`Auth::sign_in`].
    pub async fn sign_in_calendar(
        &self,
        owner: &str,
        login_hint: Option<&str>,
        open: impl FnOnce(&Url) -> io::Result<()>,
    ) -> Result<String, Error> {
        let owner = normalize_email(owner);
        if !self.state().registry.contains(&owner) {
            return Err(Error::UnknownAccount(owner));
        }
        sign_in::authorize(
            &self.0.oauth,
            self.endpoints(),
            Purpose::Calendar,
            login_hint,
            open,
            |grant| {
                let email = normalize_email(&grant.account.email);
                if let Some(expected) = login_hint
                    .filter(|expected| !normalize_email(expected).eq_ignore_ascii_case(&email))
                {
                    return Err(Error::WrongAccount {
                        expected: expected.to_owned(),
                    });
                }
                if email == owner {
                    return Ok(email);
                }
                let _op = self.ops();
                if !self.state().registry.contains(&owner) {
                    return Err(Error::UnknownAccount(owner));
                }
                let key = calendar_key(&owner, &email);
                self.0.secrets.set(&key, &grant.refresh_token)?;
                let mut state = self.state();
                state.registry.add_calendar(&owner, &email)?;
                state
                    .tokens
                    .entry(key)
                    .or_insert_with(|| Tokens::new(&email, self.0.oauth.clone(), None))
                    .replace(grant.refresh_token, grant.access);
                log::info!("signed a calendar account in");
                Ok(email)
            },
        )
        .await
    }

    /// The access tokens of calendar account `email`, for `owner`'s Calendar calls. Every call
    /// returns the same provider. Reads the Keychain the first time, as [`Auth::tokens`] does.
    ///
    /// # Errors
    /// [`Error::UnknownAccount`] when `owner` holds no grant for `email`, or
    /// [`Error::Secrets`].
    pub fn calendar_tokens(&self, owner: &str, email: &str) -> Result<Tokens, Error> {
        let owner = normalize_email(owner);
        let email = normalize_email(email);
        let key = calendar_key(&owner, &email);
        self.load_tokens(&key, &email, |state| {
            state
                .registry
                .accounts()
                .iter()
                .any(|account| account.email == owner && account.calendars.contains(&email))
        })
    }

    /// Forgets `owner`'s grant for calendar account `email`. The grant's Keychain item is deleted
    /// and its [`Tokens`] stop working. Google's grant is left alone, as [`Auth::forget`] leaves
    /// it.
    ///
    /// # Errors
    /// [`Error::UnknownAccount`] for an `owner` that isn't listed, or a storage error.
    pub fn forget_calendar(&self, owner: &str, email: &str) -> Result<(), Error> {
        let owner = normalize_email(owner);
        let email = normalize_email(email);
        let _op = self.ops();
        self.forget_calendar_locked(&owner, &email)
    }

    fn forget_calendar_locked(&self, owner: &str, email: &str) -> Result<(), Error> {
        let key = calendar_key(owner, email);
        self.0.secrets.delete(&key)?;
        let mut state = self.state();
        if let Some(tokens) = state.tokens.remove(&key) {
            tokens.revoke();
        }
        state.registry.remove_calendar(owner, email)
    }

    fn save(&self, grant: Grant) -> Result<Account, Error> {
        let Grant {
            mut account,
            refresh_token,
            access,
        } = grant;
        account.email = normalize_email(&account.email);
        let email = account.email.clone();
        let _op = self.ops();
        self.0.secrets.set(&email, &refresh_token)?;
        let mut state = self.state();
        state.registry.sign_in(account)?;
        state
            .tokens
            .entry(email.clone())
            .or_insert_with(|| Tokens::new(&email, self.0.oauth.clone(), None))
            .replace(refresh_token, access);
        log::info!("signed an account in");
        let saved = state
            .registry
            .accounts()
            .iter()
            .find(|known| known.email == email);
        Ok(saved.cloned().expect("the account was just saved"))
    }

    /// `email`'s access tokens, for its Gmail, Calendar and People clients. Every call for an
    /// account returns the same provider. Reads the Keychain the first time, which can block on
    /// an access prompt, so call it off the UI thread.
    ///
    /// # Errors
    /// [`Error::UnknownAccount`], or [`Error::Secrets`] when the Keychain can't be read.
    pub fn tokens(&self, email: &str) -> Result<Tokens, Error> {
        let email = normalize_email(email);
        self.load_tokens(&email, &email, |state| state.registry.contains(&email))
    }

    /// Returns the cached [`Tokens`] for `key` (belonging to `email`), reading the secret store
    /// once under `ops` if needed. `holds` checks whether the account or calendar grant is still
    /// registered; `state` is never held across the secret-store read.
    fn load_tokens(
        &self,
        key: &str,
        email: &str,
        holds: impl Fn(&State) -> bool,
    ) -> Result<Tokens, Error> {
        let cached = || -> Result<Option<Tokens>, Error> {
            let state = self.state();
            if !holds(&state) {
                return Err(Error::UnknownAccount(email.to_owned()));
            }
            Ok(state.tokens.get(key).cloned())
        };
        if let Some(tokens) = cached()? {
            return Ok(tokens);
        }
        let _op = self.ops();
        if let Some(tokens) = cached()? {
            return Ok(tokens);
        }
        let grant = self.0.secrets.get(key)?;
        let mut state = self.state();
        if !holds(&state) {
            return Err(Error::UnknownAccount(email.to_owned()));
        }
        Ok(state
            .tokens
            .entry(key.to_owned())
            .or_insert_with(|| Tokens::new(email, self.0.oauth.clone(), grant))
            .clone())
    }

    /// Signs `email` out of Exo: forgets its refresh token and its calendar accounts' grants,
    /// drops it from the list and deletes its profile photo. Its [`Tokens`] stop working. Google's
    /// grant is left alone, because nothing here holds it any more, and revoking it would also
    /// sign out other copies of Exo on the same OAuth client. The account's local mail store is
    /// the caller's to delete.
    ///
    /// # Errors
    /// [`Error::UnknownAccount`], or a storage error.
    pub fn forget(&self, email: &str) -> Result<(), Error> {
        let email = normalize_email(email);
        let _op = self.ops();
        // Its calendar accounts' grants go with it.
        let calendars = self
            .state()
            .registry
            .accounts()
            .iter()
            .find(|account| account.email == email)
            .map(|account| account.calendars.clone())
            .unwrap_or_default();
        for calendar in calendars {
            self.forget_calendar_locked(&email, &calendar)?;
        }
        // The secret goes first, so a failure after that leaves a listed account that needs a
        // sign-in, never a refresh token nobody can see.
        self.0.secrets.delete(&email)?;
        let removed = {
            let mut state = self.state();
            if let Some(tokens) = state.tokens.remove(&email) {
                tokens.revoke();
            }
            let removed = state.registry.remove(&email)?;
            // A photo that can't be deleted doesn't keep the account signed in.
            if removed && let Err(error) = self.0.pictures.set(&email, None) {
                log::error!("deleting a signed-out account's profile photo: {error}");
            }
            removed
        };
        if !removed {
            return Err(Error::UnknownAccount(email));
        }
        log::info!("removed an account");
        Ok(())
    }

    /// Moves `email` to position `to` in the list (the end when `to` is past it).
    ///
    /// # Errors
    /// [`Error::UnknownAccount`], or [`Error::Accounts`] when the list can't be saved.
    pub fn move_account(&self, email: &str, to: usize) -> Result<(), Error> {
        let email = normalize_email(email);
        self.state().registry.move_to(&email, to)
    }

    fn ops(&self) -> MutexGuard<'_, ()> {
        self.0
            .ops
            .lock()
            .expect("account ops lock is never poisoned")
    }

    fn state(&self) -> MutexGuard<'_, State> {
        self.0
            .state
            .lock()
            .expect("account state is never poisoned")
    }
}

/// The key that `owner`'s grant for calendar account `email` is kept under, in the
/// [`SecretStore`] and the tokens. It's separate from `email`'s own mail grant, if it has one.
fn calendar_key(owner: &str, email: &str) -> String {
    format!("calendar {email} for {owner}")
}

#[cfg(test)]
mod tests {
    use std::sync::Barrier;

    use super::*;
    use crate::MemorySecrets;
    use crate::tokens::Access;

    fn grant(email: &str, refresh: &str) -> Grant {
        Grant {
            account: Account {
                email: email.into(),
                name: Some("Ada".into()),
                ..Account::default()
            },
            refresh_token: refresh.into(),
            access: Access::new("ya29.test".into(), jiff::Timestamp::now(), 3600),
        }
    }

    #[test]
    fn normalizes_account_and_calendar_emails_across_entry_points() {
        let dir = tempfile::tempdir().unwrap();
        let secrets = Arc::new(MemorySecrets::default());
        let client = ClientConfig::new("id", "secret", Endpoints::google());
        let auth = Auth::open(client, dir.path(), secrets.clone()).unwrap();

        let saved = auth.save(grant("  Ada@Example.COM ", "r-mail")).unwrap();
        assert_eq!(saved.email, "ada@example.com");
        assert_eq!(
            secrets.get("ada@example.com").unwrap().as_deref(),
            Some("r-mail")
        );

        assert!(auth.tokens("ADA@example.com").is_ok());
        auth.use_account("  ADA@Example.com ").unwrap();
        auth.set_color("Ada@Example.COM", Some("sea".into()))
            .unwrap();
        auth.set_profile_picture("ADA@EXAMPLE.COM", Some(b"img"))
            .unwrap();
        assert_eq!(
            auth.profile_picture("  ada@example.com ")
                .unwrap()
                .as_deref(),
            Some(&b"img"[..])
        );

        // Simulate adding a calendar grant with normalized keys, then querying mixed-case.
        let cal_key = calendar_key("ada@example.com", "cal@example.com");
        secrets.set(&cal_key, "r-cal").unwrap();
        auth.state()
            .registry
            .add_calendar("ada@example.com", "cal@example.com")
            .unwrap();
        assert!(
            auth.calendar_tokens("ADA@example.com", "Cal@Example.COM")
                .is_ok()
        );
        auth.forget_calendar("ADA@example.com", "CAL@example.com")
            .unwrap();
        assert_eq!(secrets.get(&cal_key).unwrap(), None);

        auth.mark_signed_out("ADA@example.com").unwrap();
        assert!(auth.accounts()[0].signed_out);

        auth.forget("  ADA@EXAMPLE.COM ").unwrap();
        assert!(auth.accounts().is_empty());
        assert_eq!(secrets.get("ada@example.com").unwrap(), None);
    }

    #[test]
    fn serializes_concurrent_save_and_forget_so_secrets_and_registry_agree() {
        #[derive(Clone)]
        struct PausedSecrets {
            inner: Arc<MemorySecrets>,
            entered_set: Arc<Barrier>,
            allow_set: Arc<Barrier>,
        }

        impl SecretStore for PausedSecrets {
            fn get(&self, account: &str) -> Result<Option<String>, Error> {
                self.inner.get(account)
            }

            fn set(&self, account: &str, secret: &str) -> Result<(), Error> {
                self.inner.set(account, secret)?;
                self.entered_set.wait();
                self.allow_set.wait();
                Ok(())
            }

            fn delete(&self, account: &str) -> Result<(), Error> {
                self.inner.delete(account)
            }
        }

        let dir = tempfile::tempdir().unwrap();
        let entered_set = Arc::new(Barrier::new(2));
        let allow_set = Arc::new(Barrier::new(2));
        let secrets = PausedSecrets {
            inner: Arc::new(MemorySecrets::default()),
            entered_set: entered_set.clone(),
            allow_set: allow_set.clone(),
        };
        let client = ClientConfig::new("id", "secret", Endpoints::google());
        let auth = Auth::open(client, dir.path(), secrets.clone()).unwrap();

        std::thread::scope(|scope| {
            let auth_save = auth.clone();
            let saver = scope.spawn(move || {
                auth_save.save(grant("ada@example.com", "r1")).unwrap();
            });

            // Wait until `save` has written the secret, before it touches `state.registry`.
            entered_set.wait();

            // Start `forget` concurrently; `ops` must make it wait until `save` finishes!
            let auth_forget = auth.clone();
            let forgetter = scope.spawn(move || auth_forget.forget("ada@example.com"));

            std::thread::sleep(std::time::Duration::from_millis(20));
            allow_set.wait();

            saver.join().unwrap();
            forgetter.join().unwrap().unwrap();
        });

        // Because `forget` waited for `save` to finish, both secret and registry entry are gone.
        assert!(auth.accounts().is_empty());
        assert_eq!(secrets.inner.get("ada@example.com").unwrap(), None);
    }

    #[tokio::test]
    async fn sign_in_calendar_rejects_unknown_owner_before_oauth() {
        use std::sync::atomic::{AtomicBool, Ordering};

        let dir = tempfile::tempdir().unwrap();
        let client = ClientConfig::new("id", "secret", Endpoints::google());
        let auth = Auth::open(client, dir.path(), MemorySecrets::default()).unwrap();

        let opened = AtomicBool::new(false);
        let result = auth
            .sign_in_calendar("  Missing@Example.COM ", Some("cal@example.com"), |_| {
                opened.store(true, Ordering::SeqCst);
                Ok(())
            })
            .await;

        assert!(
            matches!(result, Err(Error::UnknownAccount(ref email)) if email == "missing@example.com"),
            "got {result:?}"
        );
        assert!(
            !opened.load(Ordering::SeqCst),
            "must not launch OAuth for an unknown owner"
        );
    }

    #[test]
    fn tokens_and_calendar_tokens_read_secret_store_once_and_reject_unknown() {
        use std::sync::atomic::{AtomicUsize, Ordering};

        #[derive(Clone, Default)]
        struct CountingSecrets {
            inner: Arc<MemorySecrets>,
            gets: Arc<AtomicUsize>,
        }

        impl SecretStore for CountingSecrets {
            fn get(&self, account: &str) -> Result<Option<String>, Error> {
                self.gets.fetch_add(1, Ordering::SeqCst);
                self.inner.get(account)
            }

            fn set(&self, account: &str, secret: &str) -> Result<(), Error> {
                self.inner.set(account, secret)
            }

            fn delete(&self, account: &str) -> Result<(), Error> {
                self.inner.delete(account)
            }
        }

        let dir = tempfile::tempdir().unwrap();
        let secrets = CountingSecrets::default();
        let client = ClientConfig::new("id", "secret", Endpoints::google());
        {
            let auth = Auth::open(client.clone(), dir.path(), secrets.clone()).unwrap();
            auth.save(grant("ada@example.com", "r-mail")).unwrap();
            let cal_key = calendar_key("ada@example.com", "cal@example.com");
            secrets.inner.set(&cal_key, "r-cal").unwrap();
            auth.state()
                .registry
                .add_calendar("ada@example.com", "cal@example.com")
                .unwrap();
        }

        // Reopen with an empty in-memory token cache so both paths must hit `secrets.get` once.
        let auth = Auth::open(client, dir.path(), secrets.clone()).unwrap();
        assert_eq!(secrets.gets.load(Ordering::SeqCst), 0);

        assert!(matches!(
            auth.tokens("unknown@example.com"),
            Err(Error::UnknownAccount(ref email)) if email == "unknown@example.com"
        ));
        assert!(matches!(
            auth.calendar_tokens("ada@example.com", "other@example.com"),
            Err(Error::UnknownAccount(ref email)) if email == "other@example.com"
        ));
        assert_eq!(secrets.gets.load(Ordering::SeqCst), 0);

        let mail_a = auth.tokens("ada@example.com").unwrap();
        let mail_b = auth.tokens("ADA@example.com").unwrap();
        assert_eq!(secrets.gets.load(Ordering::SeqCst), 1);
        assert!(!mail_a.needs_sign_in());
        assert!(!mail_b.needs_sign_in());

        let cal_a = auth
            .calendar_tokens("ada@example.com", "cal@example.com")
            .unwrap();
        let cal_b = auth
            .calendar_tokens("ADA@example.com", "CAL@example.com")
            .unwrap();
        assert_eq!(secrets.gets.load(Ordering::SeqCst), 2);
        assert!(!cal_a.needs_sign_in());
        assert!(!cal_b.needs_sign_in());
    }
}

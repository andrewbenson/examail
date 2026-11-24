//! Examail Cloud: the service that brings reminders back and sends scheduled mail while Exo is
//! closed. Each account hands it its jobs and keeps them up to date; Exo stays the fallback for
//! them, doing one itself only once the service has had [`exo_store::CLOUD_GRACE`] to
//! ([`exo_store`]'s `cloud` module).
//!
//! It's on when `cloud.json` beside the accounts folder names the service
//! (`{"url": "https://…"}`), and for an account once that account has connected it in the browser
//! (`<url>/v1/connect`). The service's gateway admits Exo by the account's Google ID token, and
//! the service knows this Mac by a device token kept in the Keychain.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::time::Duration;

use exo_auth::SecretStore as _;
use exo_store::{CloudJob, Store};
use serde_json::{Value, json};

/// How often the jobs are compared with the service's when nothing changes.
const RECHECK: Duration = Duration::from_secs(60);
/// How long after a change the jobs are sent, so a burst of edits is sent once.
const SETTLE: Duration = Duration::from_secs(2);
/// How long to wait while the service can't be used (not connected, offline, no ID token).
const IDLE: Duration = Duration::from_secs(300);
/// How often the service is asked whether it can still act for the account.
const CHECK_EVERY: Duration = Duration::from_secs(600);

/// The header that carries the device token.
const DEVICE: &str = "x-examail-device";

/// Where Examail Cloud is, from `cloud.json` beside `accounts` (the stores' folder).
#[derive(Debug, Clone)]
pub struct Config {
    pub url: url::Url,
}

impl Config {
    /// The config, when there's one that reads.
    #[must_use]
    pub fn load(accounts: &Path) -> Option<Self> {
        let path: PathBuf = accounts.parent()?.join("cloud.json");
        let text = std::fs::read_to_string(&path).ok()?;
        let value: Value = serde_json::from_str(&text)
            .inspect_err(|error| log::warn!("{} doesn't read: {error}", path.display()))
            .ok()?;
        let url = value["url"].as_str()?.parse().ok()?;
        Some(Self { url })
    }

    /// Where the account connects the service in the browser.
    #[must_use]
    pub fn connect_url(&self, email: &str) -> String {
        let mut url = self.url.join("v1/connect").expect("a fixed path");
        url.query_pairs_mut().append_pair("login_hint", email);
        url.to_string()
    }
}

/// What the account's loop needs.
pub struct Access {
    pub config: Config,
    pub email: String,
    pub tokens: exo_auth::Tokens,
}

/// Why a call to the service didn't go.
#[derive(Debug)]
enum Failure {
    /// The account has no ID token: its grant predates `openid`, so it signs in again.
    NoIdToken,
    /// The service doesn't know this Mac's device token (any more).
    UnknownDevice,
    /// The account hasn't connected the service.
    NotConnected,
    /// Anything else: offline, the service down, a refusal.
    Other(String),
}

impl std::fmt::Display for Failure {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NoIdToken => write!(f, "the account has no ID token; sign it in again"),
            Self::UnknownDevice => write!(f, "the service doesn't know this Mac"),
            Self::NotConnected => write!(f, "the account hasn't connected Examail Cloud"),
            Self::Other(message) => write!(f, "{message}"),
        }
    }
}

struct Client {
    http: reqwest::Client,
    access: Access,
    device: Option<String>,
}

impl Client {
    async fn call(
        &self,
        method: reqwest::Method,
        path: &str,
        body: Option<&Value>,
    ) -> Result<Value, Failure> {
        let id_token = match self.access.tokens.id_token().await {
            Ok(Some(token)) => token,
            Ok(None) => return Err(Failure::NoIdToken),
            Err(error) => return Err(Failure::Other(format!("an ID token: {error}"))),
        };
        let url = self
            .access
            .config
            .url
            .join(path)
            .map_err(|error| Failure::Other(error.to_string()))?;
        let mut request = self.http.request(method, url).bearer_auth(id_token);
        if let Some(device) = &self.device {
            request = request.header(DEVICE, device);
        }
        if let Some(body) = body {
            request = request.json(body);
        }
        let response = request
            .send()
            .await
            .map_err(|error| Failure::Other(format!("reaching Examail Cloud: {error}")))?;
        let status = response.status();
        let text = response.text().await.unwrap_or_default();
        if status.is_success() {
            return Ok(serde_json::from_str(&text).unwrap_or(Value::Null));
        }
        let message: String = serde_json::from_str::<Value>(&text)
            .ok()
            .and_then(|value| value["error"].as_str().map(str::to_owned))
            .unwrap_or_else(|| text.chars().take(200).collect());
        Err(match status.as_u16() {
            401 if self.device.is_some() && message.contains("device") => Failure::UnknownDevice,
            409 => Failure::NotConnected,
            _ => Failure::Other(format!("Examail Cloud answered {status}: {message}")),
        })
    }

    /// This Mac's device token, registering it first when it has none.
    async fn ensure_device(&mut self) -> Result<(), Failure> {
        if self.device.is_none() {
            self.device = exo_auth::CloudKeychain
                .get(&self.access.email)
                .map_err(|error| Failure::Other(format!("the Keychain: {error}")))?;
        }
        if self.device.is_some() {
            return Ok(());
        }
        let name = std::process::Command::new("scutil")
            .args(["--get", "ComputerName"])
            .output()
            .ok()
            .and_then(|out| String::from_utf8(out.stdout).ok())
            .map(|name| name.trim().to_owned())
            .filter(|name| !name.is_empty())
            .unwrap_or_else(|| "Mac".into());
        let answer = self
            .call(
                reqwest::Method::POST,
                "v1/devices",
                Some(&json!({ "name": name })),
            )
            .await?;
        let token = answer["token"]
            .as_str()
            .ok_or_else(|| Failure::Other("the service sent no device token".into()))?
            .to_owned();
        exo_auth::CloudKeychain
            .set(&self.access.email, &token)
            .map_err(|error| Failure::Other(format!("the Keychain: {error}")))?;
        log::info!(
            "this Mac is now an Examail Cloud device for {}",
            self.access.email
        );
        self.device = Some(token);
        Ok(())
    }

    fn forget_device(&mut self) {
        self.device = None;
        if let Err(error) = exo_auth::CloudKeychain.delete(&self.access.email) {
            log::warn!("forgetting the Examail Cloud device token: {error}");
        }
    }
}

/// Keeps the account's jobs with Examail Cloud until the account closes.
pub async fn run(store: Store, access: Access) {
    let mut client = Client {
        http: reqwest::Client::builder()
            .timeout(Duration::from_secs(30))
            .build()
            .expect("a client builds"),
        access,
        device: None,
    };
    let mut changes = store.changes();
    // What was last sent for each key, so an unchanged job isn't sent again, and whether the
    // service holds it (it's waiting, or it's done).
    let mut sent: HashMap<String, (Value, bool)> = HashMap::new();
    let mut checked: Option<std::time::Instant> = None;
    let mut told_no_id = false;
    loop {
        let wait = match round(&store, &mut client, &mut sent, &mut checked).await {
            Ok(()) => RECHECK,
            Err(Failure::NoIdToken) => {
                if !told_no_id {
                    log::warn!(
                        "Examail Cloud needs {} signed in again, to add an ID token",
                        client.access.email
                    );
                    told_no_id = true;
                }
                let _ = store.set_handed_off([]).await;
                IDLE
            }
            Err(Failure::NotConnected) => {
                log::info!(
                    "{} hasn't connected Examail Cloud: {}",
                    client.access.email,
                    client.access.config.connect_url(&client.access.email)
                );
                sent.clear();
                let _ = store.set_handed_off([]).await;
                IDLE
            }
            Err(Failure::UnknownDevice) => {
                client.forget_device();
                sent.clear();
                checked = None;
                SETTLE
            }
            Err(Failure::Other(message)) => {
                // What the service holds it still holds; Exo does those itself after the grace.
                log::info!("Examail Cloud for {}: {message}", client.access.email);
                RECHECK
            }
        };
        // A change is sent once things settle; otherwise look again after `wait`.
        tokio::select! {
            changed = changes.changed() => {
                if changed.is_err() {
                    return;
                }
                tokio::time::sleep(SETTLE).await;
                changes.borrow_and_update();
            }
            () = tokio::time::sleep(wait) => {}
        }
    }
}

/// One comparison of the account's jobs with the service's: sends what's new or changed, takes
/// back what's gone, and tells the store which the service holds.
async fn round(
    store: &Store,
    client: &mut Client,
    sent: &mut HashMap<String, (Value, bool)>,
    checked: &mut Option<std::time::Instant>,
) -> Result<(), Failure> {
    client.ensure_device().await?;
    if checked.is_none_or(|at| at.elapsed() >= CHECK_EVERY) {
        let answer = client.call(reqwest::Method::GET, "v1/check", None).await?;
        if answer["ok"] != true {
            // Google no longer honors the service's grant: connect again.
            return Err(Failure::NotConnected);
        }
        *checked = Some(std::time::Instant::now());
    }
    let jobs = store
        .cloud_jobs()
        .await
        .map_err(|error| Failure::Other(format!("reading the jobs: {error}")))?;
    let remote = client.call(reqwest::Method::GET, "v1/jobs", None).await?;
    let remote: HashMap<String, Value> = remote["jobs"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|job| Some((job["key"].as_str()?.to_owned(), job.clone())))
        .collect();
    let wanted: HashSet<&str> = jobs.iter().map(|job| job.key.as_str()).collect();

    for CloudJob { key, request } in &jobs {
        let unchanged = sent.get(key).is_some_and(|(last, _)| last == request);
        if unchanged && remote.contains_key(key) {
            continue;
        }
        let path = format!("v1/jobs/{}", encode(key));
        match client
            .call(reqwest::Method::PUT, &path, Some(request))
            .await
        {
            Ok(answer) => {
                // A job the service failed is Exo's to do; one waiting or done is the service's.
                let held = answer["job"]["status"] != "failed";
                sent.insert(key.clone(), (request.clone(), held));
            }
            Err(Failure::Other(message)) => {
                // Refused (too old, say): Exo does it on time.
                log::warn!("handing {key} to Examail Cloud: {message}");
                sent.remove(key);
            }
            Err(failure) => return Err(failure),
        }
    }
    // Jobs Exo no longer has (done, called off, unscheduled, edited into another key) come off.
    for key in remote.keys().filter(|key| !wanted.contains(key.as_str())) {
        let path = format!("v1/jobs/{}", encode(key));
        match client.call(reqwest::Method::DELETE, &path, None).await {
            Ok(_) => {
                sent.remove(key);
            }
            Err(Failure::Other(message)) => log::warn!("taking {key} back: {message}"),
            Err(failure) => return Err(failure),
        }
    }
    sent.retain(|key, _| wanted.contains(key.as_str()));
    // The service's view of a job it holds can also have moved on (it failed one since).
    let held = sent.iter().filter_map(|(key, (_, held))| {
        let failed_since = remote
            .get(key)
            .is_some_and(|job| job["status"] == "failed" && sent_matches(job, key, sent));
        (*held && !failed_since).then(|| key.clone())
    });
    store
        .set_handed_off(held.collect::<Vec<_>>())
        .await
        .map_err(|error| Failure::Other(format!("recording the hand-off: {error}")))?;
    Ok(())
}

/// Whether the service's `job` is the one last sent for `key` (same time), rather than an older
/// version the PUT just replaced.
fn sent_matches(job: &Value, key: &str, sent: &HashMap<String, (Value, bool)>) -> bool {
    let Some((request, _)) = sent.get(key) else {
        return false;
    };
    let at = |value: &Value| {
        value
            .as_str()
            .and_then(|text| text.parse::<jiff::Timestamp>().ok())
            .map(|at| at.as_second())
    };
    at(&job["at"]).is_some() && at(&job["at"]) == at(&request["at"])
}

/// `key` for a URL path. Keys are `[A-Za-z0-9-_.:]`, of which only `:` is worth escaping.
fn encode(key: &str) -> String {
    key.replace(':', "%3A")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_config_names_the_service() {
        let dir = std::env::temp_dir().join(format!("exo-cloud-config-{}", std::process::id()));
        let accounts = dir.join("accounts");
        std::fs::create_dir_all(&accounts).unwrap();
        let _ = std::fs::remove_file(dir.join("cloud.json"));
        assert!(Config::load(&accounts).is_none(), "off without a config");
        std::fs::write(
            dir.join("cloud.json"),
            r#"{"url": "https://relay.example/"}"#,
        )
        .unwrap();
        let config = Config::load(&accounts).expect("reads");
        assert_eq!(
            config.connect_url("maya@example.com"),
            "https://relay.example/v1/connect?login_hint=maya%40example.com"
        );
    }

    #[test]
    fn a_failed_job_is_the_one_sent_only_when_its_time_matches() {
        let mut sent = HashMap::new();
        sent.insert(
            "send:d1".to_owned(),
            (json!({ "at": "2026-11-23T09:00:00Z" }), true),
        );
        assert!(sent_matches(
            &json!({ "at": "2026-11-23T09:00:00Z" }),
            "send:d1",
            &sent
        ));
        assert!(!sent_matches(
            &json!({ "at": "2026-11-23T08:00:00Z" }),
            "send:d1",
            &sent
        ));
        assert_eq!(encode("remind:abc"), "remind%3Aabc");
    }
}

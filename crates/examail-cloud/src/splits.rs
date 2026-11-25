//! Split labels: each of the account's splits as a Gmail label, `[Exo]/<split name>`, put on
//! the mail that arrives into it, so Gmail on the web and the phone can show a split too.
//!
//! The splits come from the synced settings (`splitInboxesV2`), each with the Gmail query the app
//! compiled for it (`matcher.gmailQuery`), the same one its backfill searches with. Gmail's search
//! takes a little while to index new mail, so an arrival waits in `routes` until a search finds
//! it by its `Message-ID`, and then each split's query, narrowed to that message, says whether it
//! belongs. Labels are only ever added here: the service is their one writer, and nothing reads
//! them back to route with.
//!
//! A split's label is known by the split's external id (`split_labels`), so renaming the split
//! renames its label; a split that's gone or turned off just stops getting one.

use std::collections::BTreeMap;

use anyhow::Result;
use jiff::{SignedDuration, Timestamp};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::App;
use crate::firestore::Precondition;
use crate::jobs;
use crate::watch::Arrival;

pub const ROUTES: &str = "routes";
pub const LABELS: &str = "split_labels";
/// What split labels are named under.
const PARENT: &str = "[Exo]";
/// How long to wait for Gmail's search between looks.
const WAIT: SignedDuration = SignedDuration::from_secs(45);
/// How many looks before giving up on a message the search never finds.
const LOOKS: u32 = 10;
/// The most routes one tick works through.
const BATCH: u32 = 50;

/// A message waiting for Gmail's search, to be given its split labels.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Route {
    pub sub: String,
    pub message_id: String,
    pub thread_id: String,
    /// When to look next, as [`jobs::stamp`] writes it.
    pub due: String,
    pub looks: u32,
}

/// An account's split labels, label id by split external id, with each label's name as made.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(default)]
pub struct Labels {
    pub labels: BTreeMap<String, SplitLabel>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(default)]
pub struct SplitLabel {
    pub id: String,
    pub name: String,
}

/// One split, as the settings say.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Split {
    pub external_id: String,
    pub name: String,
    pub gmail_query: String,
}

/// The splits in `settings` that are on and have a query, in the settings' order.
pub fn splits(settings: &Value) -> Vec<Split> {
    let v2 = &settings["splitInboxesV2"];
    let Some(all) = v2["splitInboxes"].as_object() else {
        return Vec::new();
    };
    let order: Vec<&str> = v2["splitInboxesOrder"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(Value::as_str)
        .collect();
    let mut keys: Vec<&String> = all.keys().collect();
    keys.sort_by_key(|key| order.iter().position(|id| id == key).unwrap_or(usize::MAX));
    keys.into_iter()
        .filter_map(|key| {
            let split = &all[key];
            if split["isDisabled"] == true {
                return None;
            }
            let name = split["matcher"]["name"].as_str()?.trim();
            let query = split["matcher"]["gmailQuery"].as_str()?.trim();
            if name.is_empty() || query.is_empty() {
                return None;
            }
            Some(Split {
                external_id: split["externalId"].as_str().unwrap_or(key).to_owned(),
                name: name.to_owned(),
                gmail_query: query.to_owned(),
            })
        })
        .collect()
}

/// The label a split's mail gets: `[Exo]/<name>`, with `/` in the name made safe, since it would
/// nest.
pub fn label_name(split: &Split) -> String {
    format!("{PARENT}/{}", split.name.replace('/', "∕"))
}

/// Queues `arrival` to be given its split labels once Gmail's search has it.
pub async fn queue(app: &App, sub: &str, arrival: &Arrival) -> Result<()> {
    let route = Route {
        sub: sub.to_owned(),
        message_id: arrival.id.clone(),
        thread_id: arrival.thread_id.clone(),
        due: jobs::stamp(Timestamp::now() + WAIT),
        looks: 0,
    };
    let id = format!("{sub}:{}", arrival.id);
    match app
        .store
        .db()
        .set(ROUTES, &id, &route, Precondition::Missing)
        .await
    {
        Ok(()) | Err(crate::firestore::Error::Exists | crate::firestore::Error::Gone) => Ok(()),
        Err(crate::firestore::Error::Other(error)) => Err(error),
    }
}

/// The tick's part: gives the routes that are due their labels.
pub async fn tick(app: &App) -> Result<()> {
    let now = Timestamp::now();
    let due = app
        .store
        .db()
        .at_most(ROUTES, "due", &Value::from(jobs::stamp(now)), BATCH)
        .await?;
    let mut accounts: BTreeMap<String, Option<(gmail::Client, Vec<Split>, Labels)>> =
        BTreeMap::new();
    for document in due {
        let Ok(mut route) = serde_json::from_value::<Route>(document.fields.clone()) else {
            app.store.db().delete(ROUTES, &document.id).await?;
            continue;
        };
        // Taken for this look; another tick at once finds it moved ahead.
        let looked = route.looks;
        route.looks += 1;
        route.due = jobs::stamp(now + WAIT);
        if !app
            .store
            .db()
            .set_if_unchanged(ROUTES, &document.id, &route, &document.update_time)
            .await?
        {
            continue;
        }
        if !accounts.contains_key(&route.sub) {
            let account = account(app, &route.sub).await.unwrap_or_else(|error| {
                log::warn!("split labels for {}: {error:#}", route.sub);
                None
            });
            accounts.insert(route.sub.clone(), account);
        }
        let Some(Some((gmail, splits, labels))) = accounts.get_mut(&route.sub) else {
            app.store.db().delete(ROUTES, &document.id).await?;
            continue;
        };
        match label(app, gmail, splits, labels, &route).await {
            Ok(true) => app.store.db().delete(ROUTES, &document.id).await?,
            Ok(false) if looked + 1 >= LOOKS => {
                log::warn!(
                    "search never found {}'s message {}",
                    route.sub,
                    route.message_id
                );
                app.store.db().delete(ROUTES, &document.id).await?;
            }
            Ok(false) => {}
            Err(error) => log::warn!(
                "split labels for {}'s message {}: {error:#}",
                route.sub,
                route.message_id
            ),
        }
    }
    Ok(())
}

/// The account's Gmail, splits and split labels, or `None` when it has no splits or can't be
/// acted for.
async fn account(app: &App, sub: &str) -> Result<Option<(gmail::Client, Vec<Split>, Labels)>> {
    let Some((settings, _)) = crate::settings::load(app, sub).await? else {
        return Ok(None);
    };
    let splits = splits(&settings);
    if splits.is_empty() {
        return Ok(None);
    }
    let Some(gmail) = jobs::account_gmail(app, sub).await? else {
        return Ok(None);
    };
    let labels: Labels = app.store.db().get(LABELS, sub).await?.unwrap_or_default();
    Ok(Some((gmail, splits, labels)))
}

/// Gives `route`'s message the labels of the splits it belongs to. `false` while Gmail's search
/// doesn't have it yet.
async fn label(
    app: &App,
    gmail: &gmail::Client,
    splits: &[Split],
    labels: &mut Labels,
    route: &Route,
) -> Result<bool> {
    let message = match gmail
        .gmail()
        .message(&route.message_id, gmail::Format::Metadata, &["Message-ID"])
        .await
    {
        Ok(message) => message,
        Err(error) if jobs::not_found(&error) => return Ok(true),
        Err(error) => return Err(error.into()),
    };
    if !message.label_ids.iter().any(|label| label == "INBOX") {
        return Ok(true);
    }
    // The sender writes the Message-ID, so only a plain one goes into a search, quoted.
    let Some(rfc822) = message
        .header("Message-ID")
        .map(|id| id.trim().trim_matches(|c| c == '<' || c == '>').to_owned())
        .filter(|id| plain_message_id(id))
    else {
        return Ok(true);
    };
    let narrowed = |query: &str| format!("rfc822msgid:\"{rfc822}\" {query}");
    let id = route.message_id.as_str();
    if !found(gmail, &narrowed("in:anywhere"), id).await? {
        return Ok(false);
    }
    let mut add = Vec::new();
    for split in splits {
        if found(gmail, &narrowed(&format!("({})", split.gmail_query)), id).await? {
            add.push(label_id(app, gmail, &route.sub, split, labels).await?);
        }
    }
    if !add.is_empty() {
        let request = gmail::ModifyMessageRequest {
            add_label_ids: add,
            ..gmail::ModifyMessageRequest::default()
        };
        gmail
            .gmail()
            .modify_message(&route.message_id, &request)
            .await?;
    }
    Ok(true)
}

/// Whether the search `query` finds message `id` itself.
async fn found(gmail: &gmail::Client, query: &str, id: &str) -> Result<bool> {
    let query = gmail::ListQuery {
        q: Some(query.to_owned()),
        max_results: Some(10),
        ..gmail::ListQuery::default()
    };
    Ok(gmail
        .gmail()
        .messages(&query)
        .await?
        .messages
        .iter()
        .any(|message| message.id.as_deref() == Some(id)))
}

/// A Message-ID safe to search by: 1–200 of letters, digits and `.@_+=$%-`, with an `@`.
pub fn plain_message_id(id: &str) -> bool {
    (1..=200).contains(&id.len())
        && id.contains('@')
        && id
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || ".@_+=$%-".contains(c))
}

/// The id of `split`'s label: the one made for it before (renamed if the split was), else one
/// found by its name, else a new one.
async fn label_id(
    app: &App,
    gmail: &gmail::Client,
    sub: &str,
    split: &Split,
    labels: &mut Labels,
) -> Result<String> {
    let name = label_name(split);
    if let Some(known) = labels.labels.get(&split.external_id).cloned() {
        if known.name == name {
            return Ok(known.id);
        }
        let patch = gmail::Label {
            name: Some(name.clone()),
            ..gmail::Label::default()
        };
        match gmail.gmail().patch_label(&known.id, &patch).await {
            Ok(_) => {
                remember(app, sub, labels, split, &known.id, &name).await?;
                return Ok(known.id);
            }
            // Deleted in Gmail, or the name is taken: find or make it below.
            Err(error) => log::info!("renaming a split label: {error}"),
        }
    }
    let existing = gmail.gmail().labels().await?;
    let id = match existing
        .labels
        .iter()
        .find(|label| label.name.as_deref() == Some(name.as_str()))
        .and_then(|label| label.id.clone())
    {
        Some(id) => id,
        None => {
            let label = gmail::Label {
                name: Some(name.clone()),
                label_list_visibility: Some(gmail::LabelListVisibility::LabelShow),
                message_list_visibility: Some(gmail::MessageListVisibility::Show),
                ..gmail::Label::default()
            };
            gmail
                .gmail()
                .create_label(&label)
                .await?
                .id
                .ok_or_else(|| anyhow::anyhow!("Gmail made a label without an id"))?
        }
    };
    remember(app, sub, labels, split, &id, &name).await?;
    Ok(id)
}

async fn remember(
    app: &App,
    sub: &str,
    labels: &mut Labels,
    split: &Split,
    id: &str,
    name: &str,
) -> Result<()> {
    labels.labels.insert(
        split.external_id.clone(),
        SplitLabel {
            id: id.to_owned(),
            name: name.to_owned(),
        },
    );
    Ok(app
        .store
        .db()
        .set(LABELS, sub, labels, Precondition::None)
        .await?)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn splits_come_from_the_settings_in_order_skipping_off_and_empty_ones() {
        let settings = json!({ "splitInboxesV2": {
            "splitInboxes": {
                "b": { "externalId": "b", "matcher": { "name": "Team", "gmailQuery": "from:x.com" } },
                "a": { "externalId": "a", "matcher": { "name": "News", "gmailQuery": "list:*" } },
                "c": { "externalId": "c", "isDisabled": true,
                       "matcher": { "name": "Off", "gmailQuery": "is:starred" } },
                "d": { "externalId": "d", "matcher": { "name": "VIP", "gmailQuery": "" } }
            },
            "splitInboxesOrder": ["inbox", "important", "b", "a", "c", "d", "other"]
        }});
        let names: Vec<String> = splits(&settings)
            .into_iter()
            .map(|split| split.name)
            .collect();
        assert_eq!(names, ["Team", "News"]);
        assert!(splits(&json!({})).is_empty());
    }

    #[test]
    fn only_a_plain_message_id_is_searched_by() {
        assert!(plain_message_id("CAGYk1Y-x.y=z@mail.gmail.com"));
        assert!(!plain_message_id("a@b) OR (in:anywhere"));
        assert!(!plain_message_id("a\"@b"));
        assert!(!plain_message_id("no-at-sign"));
    }

    #[test]
    fn a_label_is_named_under_exo_without_nesting() {
        let split = Split {
            external_id: "a".into(),
            name: "Clients/Big".into(),
            gmail_query: "x".into(),
        };
        assert_eq!(label_name(&split), "[Exo]/Clients∕Big");
    }
}

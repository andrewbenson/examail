//! Settings sync: the app's account settings document, kept here so every Mac of the account
//! has the same splits, snippets and the rest, and so the service can act on them (split labels,
//! notifications) while every Mac is closed.
//!
//! The service keeps one document per account and a version that counts its writes. The app
//! reads it (`GET`), merges it with its own, and writes the merge back naming the version it read
//! (`PUT`). A write whose version is stale is refused with `412` and the current document, which
//! the app merges again.

use std::collections::BTreeMap;
use std::sync::Arc;

use anyhow::anyhow;
use axum::Json;
use axum::extract::State;
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::App;
use crate::api::{Error, Result, identify_device};
use crate::store::{SETTINGS, Settings};

/// Firestore holds a document of at most 1 MiB, fields and all.
const LARGEST: usize = 900 * 1024;

/// What the service needs to act on an account's settings that the settings don't say, which
/// the app sends with them.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(default)]
pub struct Context {
    /// The account's own addresses: its primary one and its send-as aliases.
    pub addresses: Vec<String>,
    /// The account's labels, name by id.
    pub labels: BTreeMap<String, String>,
}

#[derive(Debug, Deserialize)]
pub struct Put {
    /// The version the document was merged with: `0` when there was none.
    pub base: i64,
    pub document: Value,
    #[serde(default)]
    pub context: Context,
}

/// The account's settings, and what the service needs to act on them, when synced.
pub async fn load(app: &App, sub: &str) -> anyhow::Result<Option<(Value, Context)>> {
    let Some(settings) = app.store.db().get::<Settings>(SETTINGS, sub).await? else {
        return Ok(None);
    };
    let document = serde_json::from_str(&settings.document)?;
    let context = serde_json::from_str(&settings.context).unwrap_or_default();
    Ok(Some((document, context)))
}

pub async fn get(State(app): State<Arc<App>>, headers: HeaderMap) -> Result<Json<Value>> {
    let (identity, _) = identify_device(&app, &headers).await?;
    let settings: Option<Settings> = app.store.db().get(SETTINGS, &identity.sub).await?;
    Ok(Json(answer(settings.as_ref())?))
}

pub async fn put(
    State(app): State<Arc<App>>,
    headers: HeaderMap,
    Json(put): Json<Put>,
) -> Result<Response> {
    let (identity, _) = identify_device(&app, &headers).await?;
    if !put.document.is_object() {
        return Err(Error::new(
            StatusCode::BAD_REQUEST,
            anyhow!("the settings are a JSON object"),
        ));
    }
    let text = serde_json::to_string(&put.document)?;
    let context = serde_json::to_string(&put.context)?;
    if text.len() + context.len() > LARGEST {
        return Err(Error::new(
            StatusCode::PAYLOAD_TOO_LARGE,
            anyhow!("the settings are larger than {LARGEST} bytes"),
        ));
    }
    let current = app.store.db().get_document(SETTINGS, &identity.sub).await?;
    let read: Option<Settings> = current
        .as_ref()
        .map(|document| serde_json::from_value(document.fields.clone()))
        .transpose()?;
    let version = read.as_ref().map_or(0, |settings| settings.version);
    if version != put.base {
        return Ok(stale(read.as_ref())?);
    }
    let next = Settings {
        document: text,
        version: version + 1,
        context,
        updated_at: jiff::Timestamp::now().to_string(),
    };
    let written = match &current {
        Some(document) => {
            app.store
                .db()
                .set_if_unchanged(SETTINGS, &identity.sub, &next, &document.update_time)
                .await?
        }
        None => match app
            .store
            .db()
            .set(
                SETTINGS,
                &identity.sub,
                &next,
                crate::firestore::Precondition::Missing,
            )
            .await
        {
            Ok(()) => true,
            Err(crate::firestore::Error::Exists) => false,
            Err(crate::firestore::Error::Other(error)) => return Err(error.into()),
        },
    };
    if !written {
        // Another Mac wrote between the read and the write.
        let settings: Option<Settings> = app.store.db().get(SETTINGS, &identity.sub).await?;
        return Ok(stale(settings.as_ref())?);
    }
    Ok(Json(json!({ "version": next.version })).into_response())
}

/// The settings as the app reads them: `{version, document}`, `document` null when none.
fn answer(settings: Option<&Settings>) -> anyhow::Result<Value> {
    Ok(match settings {
        Some(settings) => json!({
            "version": settings.version,
            "document": serde_json::from_str::<Value>(&settings.document)?,
        }),
        None => json!({ "version": 0, "document": null }),
    })
}

/// `412`, with the settings as they are now, to merge with again.
fn stale(settings: Option<&Settings>) -> anyhow::Result<Response> {
    Ok((StatusCode::PRECONDITION_FAILED, Json(answer(settings)?)).into_response())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn no_settings_read_as_version_zero() {
        assert_eq!(
            answer(None).unwrap(),
            json!({ "version": 0, "document": null })
        );
        let settings = Settings {
            document: r#"{"theme":"carbon"}"#.into(),
            version: 3,
            context: String::new(),
            updated_at: String::new(),
        };
        assert_eq!(
            answer(Some(&settings)).unwrap(),
            json!({ "version": 3, "document": { "theme": "carbon" } })
        );
    }

    #[test]
    fn a_context_reads_with_parts_missing() {
        let context: Context = serde_json::from_str(r#"{"addresses":["a@example.com"]}"#).unwrap();
        assert_eq!(context.addresses, ["a@example.com"]);
        assert!(context.labels.is_empty());
    }
}

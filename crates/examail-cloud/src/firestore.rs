//! A small client for Firestore's REST API: documents as JSON, read, written, deleted and
//! queried by field. Documents are plain JSON objects here; [`encode`] and [`decode`] translate
//! them to Firestore's typed values.

use anyhow::{Context, Result, bail};
use reqwest::{Method, StatusCode};
use serde::Serialize;
use serde::de::DeserializeOwned;
use serde_json::{Map, Value, json};

use crate::google::Google;

#[derive(Clone)]
pub struct Firestore {
    http: reqwest::Client,
    google: Google,
    /// `projects/{project}/databases/{database}/documents`.
    root: String,
}

/// Whether a write may make the document, replace it, or both.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Precondition {
    /// Create or replace.
    None,
    /// Only create; fails with [`Error::Exists`] when the document is there.
    Missing,
}

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("the document already exists")]
    Exists,
    #[error(transparent)]
    Other(#[from] anyhow::Error),
}

impl Firestore {
    pub fn new(http: reqwest::Client, google: Google, project: &str, database: &str) -> Self {
        Self {
            http,
            google,
            root: format!("projects/{project}/databases/{database}/documents"),
        }
    }

    /// The document `collection/id`, or `None` when there's none.
    pub async fn get<T: DeserializeOwned>(&self, collection: &str, id: &str) -> Result<Option<T>> {
        let response = self.request(Method::GET, &format!("{collection}/{id}"), None).await?;
        if response.status() == StatusCode::NOT_FOUND {
            return Ok(None);
        }
        let document = checked(response).await?;
        Ok(Some(serde_json::from_value(decode_document(&document))?))
    }

    /// Writes `value` as the document `collection/id`.
    pub async fn set<T: Serialize>(
        &self,
        collection: &str,
        id: &str,
        value: &T,
        precondition: Precondition,
    ) -> Result<(), Error> {
        let fields = encode_fields(&serde_json::to_value(value).map_err(anyhow::Error::from)?)?;
        let path = match precondition {
            Precondition::None => format!("{collection}/{id}"),
            Precondition::Missing => format!("{collection}/{id}?currentDocument.exists=false"),
        };
        let response = self
            .request(Method::PATCH, &path, Some(json!({ "fields": fields })))
            .await?;
        if precondition == Precondition::Missing
            && matches!(response.status(), StatusCode::CONFLICT | StatusCode::BAD_REQUEST)
        {
            let text = response.text().await.unwrap_or_default();
            if text.contains("ALREADY_EXISTS") || text.contains("FAILED_PRECONDITION") {
                return Err(Error::Exists);
            }
            return Err(anyhow::anyhow!("writing {collection}/{id}: {text}").into());
        }
        checked(response).await?;
        Ok(())
    }

    /// Deletes `collection/id`, if it's there.
    pub async fn delete(&self, collection: &str, id: &str) -> Result<()> {
        let response = self
            .request(Method::DELETE, &format!("{collection}/{id}"), None)
            .await?;
        if response.status() != StatusCode::NOT_FOUND {
            checked(response).await?;
        }
        Ok(())
    }

    /// The ids of the documents in `collection` whose `field` equals `value`.
    pub async fn ids_where(&self, collection: &str, field: &str, value: &Value) -> Result<Vec<String>> {
        let query = json!({
            "structuredQuery": {
                "from": [{ "collectionId": collection }],
                "where": { "fieldFilter": {
                    "field": { "fieldPath": field },
                    "op": "EQUAL",
                    "value": encode(value)?,
                } },
                "select": { "fields": [{ "fieldPath": "__name__" }] },
            }
        });
        let url = format!("https://firestore.googleapis.com/v1/{}:runQuery", self.root);
        let response = self
            .http
            .post(url)
            .bearer_auth(self.google.access_token().await?)
            .json(&query)
            .send()
            .await
            .context("querying Firestore")?;
        let rows = checked(response).await?;
        Ok(rows
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(|row| row["document"]["name"].as_str())
            .filter_map(|name| name.rsplit('/').next())
            .map(str::to_owned)
            .collect())
    }

    async fn request(&self, method: Method, path: &str, body: Option<Value>) -> Result<reqwest::Response> {
        let url = format!("https://firestore.googleapis.com/v1/{}/{path}", self.root);
        let mut request = self
            .http
            .request(method, url)
            .bearer_auth(self.google.access_token().await?);
        if let Some(body) = body {
            request = request.json(&body);
        }
        request.send().await.context("calling Firestore")
    }
}

async fn checked(response: reqwest::Response) -> Result<Value> {
    let status = response.status();
    let text = response.text().await.unwrap_or_default();
    if !status.is_success() {
        bail!("Firestore answered {status}: {text}");
    }
    if text.is_empty() {
        return Ok(Value::Null);
    }
    Ok(serde_json::from_str(&text)?)
}

/// A JSON object as Firestore's `fields`.
fn encode_fields(value: &Value) -> Result<Map<String, Value>> {
    let Value::Object(object) = value else {
        bail!("a document is an object");
    };
    object
        .iter()
        .map(|(key, value)| Ok((key.clone(), encode(value)?)))
        .collect()
}

/// A JSON value as a Firestore value. Integers stay integers; RFC 3339 strings stay strings
/// (times are compared as strings, which sort correctly in UTC).
pub fn encode(value: &Value) -> Result<Value> {
    Ok(match value {
        Value::Null => json!({ "nullValue": null }),
        Value::Bool(flag) => json!({ "booleanValue": flag }),
        Value::Number(number) if number.is_i64() || number.is_u64() => {
            json!({ "integerValue": number.to_string() })
        }
        Value::Number(number) => json!({ "doubleValue": number.as_f64() }),
        Value::String(text) => json!({ "stringValue": text }),
        Value::Array(items) => json!({ "arrayValue": {
            "values": items.iter().map(encode).collect::<Result<Vec<_>>>()?
        } }),
        Value::Object(_) => json!({ "mapValue": { "fields": encode_fields(value)? } }),
    })
}

fn decode_document(document: &Value) -> Value {
    decode_fields(&document["fields"])
}

fn decode_fields(fields: &Value) -> Value {
    Value::Object(
        fields
            .as_object()
            .into_iter()
            .flatten()
            .map(|(key, value)| (key.clone(), decode(value)))
            .collect(),
    )
}

/// A Firestore value as JSON.
pub fn decode(value: &Value) -> Value {
    let Some((kind, inner)) = value.as_object().and_then(|object| object.iter().next()) else {
        return Value::Null;
    };
    match kind.as_str() {
        "booleanValue" => inner.clone(),
        "integerValue" => inner
            .as_str()
            .and_then(|text| text.parse::<i64>().ok())
            .map_or(Value::Null, Value::from),
        "doubleValue" => inner.clone(),
        "stringValue" | "timestampValue" | "referenceValue" => inner.clone(),
        "arrayValue" => Value::Array(
            inner["values"]
                .as_array()
                .into_iter()
                .flatten()
                .map(decode)
                .collect(),
        ),
        "mapValue" => decode_fields(&inner["fields"]),
        _ => Value::Null,
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::{decode, encode};

    #[test]
    fn values_survive_the_round_trip() {
        let value = json!({
            "name": "Examail",
            "count": 3,
            "ratio": 0.5,
            "on": true,
            "nothing": null,
            "list": ["a", 1],
            "nested": { "at": "2026-11-22T10:00:00Z" },
        });
        assert_eq!(decode(&encode(&value).unwrap()), value);
    }

    #[test]
    fn integers_are_sent_as_strings() {
        assert_eq!(encode(&json!(42)).unwrap(), json!({ "integerValue": "42" }));
    }
}

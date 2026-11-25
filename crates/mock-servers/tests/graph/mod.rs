//! Shared helpers for the Microsoft Graph tests: a running mock, raw HTTP access with Graph's
//! headers, and a validator that checks JSON against Microsoft's published Graph metadata
//! (`metadata.xml`, a trimmed copy of the v1.0 CSDL), independent of the mock's own code.

#![allow(dead_code)] // Each test binary uses a different subset of these helpers.

use std::collections::HashMap;
use std::path::PathBuf;

use mock_servers::{Config, DEV_ACCESS_TOKEN, MockServer};
use reqwest::Method;
use reqwest::header::HeaderMap;
use serde_json::{Value, json};

/// A mock and a client that talks to its Graph side.
pub struct Graph {
    pub server: MockServer,
    pub http: reqwest::Client,
    pub token: String,
}

/// A response: status, headers and body (JSON, or `Null` when it isn't).
pub struct Reply {
    pub status: u16,
    pub headers: HeaderMap,
    pub json: Value,
    pub bytes: Vec<u8>,
}

impl Graph {
    pub async fn start() -> Self {
        Self::with(Config::default()).await
    }

    pub async fn with(config: Config) -> Self {
        let server = MockServer::start(config).await.expect("mock starts");
        Self {
            server,
            http: reqwest::Client::builder()
                .redirect(reqwest::redirect::Policy::none())
                .build()
                .expect("a client"),
            token: DEV_ACCESS_TOKEN.to_owned(),
        }
    }

    /// `path` on the mock (`/v1.0/me`), or a link it gave (absolute) as is.
    pub fn url(&self, path: &str) -> String {
        if path.starts_with("http") {
            path.to_owned()
        } else {
            format!(
                "{}{}",
                self.server.url().as_str().trim_end_matches('/'),
                path
            )
        }
    }

    pub async fn request(
        &self,
        method: Method,
        path: &str,
        headers: &[(&str, &str)],
        body: Option<(&str, Vec<u8>)>,
    ) -> Reply {
        let mut request = self
            .http
            .request(method, self.url(path))
            .bearer_auth(&self.token);
        for (name, value) in headers {
            request = request.header(*name, *value);
        }
        if let Some((content_type, body)) = body {
            request = request.header("Content-Type", content_type).body(body);
        }
        let response = request.send().await.expect("request");
        let status = response.status().as_u16();
        let headers = response.headers().clone();
        let bytes = response.bytes().await.unwrap_or_default().to_vec();
        Reply {
            status,
            headers,
            json: serde_json::from_slice(&bytes).unwrap_or(Value::Null),
            bytes,
        }
    }

    pub async fn get(&self, path: &str) -> Reply {
        self.request(Method::GET, path, &[], None).await
    }

    pub async fn get_with(&self, path: &str, headers: &[(&str, &str)]) -> Reply {
        self.request(Method::GET, path, headers, None).await
    }

    pub async fn json(&self, method: Method, path: &str, body: Value) -> Reply {
        self.request(
            method,
            path,
            &[],
            Some(("application/json", body.to_string().into_bytes())),
        )
        .await
    }

    pub async fn json_with(
        &self,
        method: Method,
        path: &str,
        headers: &[(&str, &str)],
        body: Value,
    ) -> Reply {
        self.request(
            method,
            path,
            headers,
            Some(("application/json", body.to_string().into_bytes())),
        )
        .await
    }

    /// Calls a `/__mock/` endpoint.
    pub async fn admin(&self, path: &str, body: Value) -> u16 {
        self.http
            .post(self.url(path))
            .json(&body)
            .send()
            .await
            .expect("admin request")
            .status()
            .as_u16()
    }

    /// Follows `@odata.nextLink`s from `path`, returning every item and the last page.
    pub async fn pages(&self, path: &str, headers: &[(&str, &str)]) -> (Vec<Value>, Value) {
        let mut items = Vec::new();
        let mut next = path.to_owned();
        loop {
            let reply = self.get_with(&next, headers).await;
            assert_eq!(reply.status, 200, "{next}: {}", reply.json);
            items.extend(reply.json["value"].as_array().cloned().unwrap_or_default());
            match reply.json["@odata.nextLink"].as_str() {
                Some(link) => next = link.to_owned(),
                None => return (items, reply.json),
            }
        }
    }

    /// The ID of the first message matching `filter` (an OData `$filter`) in `folder`.
    pub async fn first(&self, folder: &str, filter: &str) -> Value {
        let path = format!(
            "/v1.0/me/mailFolders/{folder}/messages?$filter={}",
            urlencode(filter)
        );
        let reply = self.get(&path).await;
        assert_eq!(reply.status, 200, "{path}: {}", reply.json);
        reply.json["value"][0].clone()
    }
}

pub fn urlencode(text: &str) -> String {
    url::form_urlencoded::byte_serialize(text.as_bytes()).collect()
}

/// Microsoft's Graph metadata, as the schema of record.
pub struct Metadata {
    types: HashMap<String, Type>,
}

#[derive(Debug, Default)]
struct Type {
    base: Option<String>,
    /// Property name → (type, nullable).
    properties: HashMap<String, (String, bool)>,
    members: Vec<String>,
    is_enum: bool,
}

impl Metadata {
    pub fn load() -> Self {
        let path: PathBuf = [env!("CARGO_MANIFEST_DIR"), "tests", "graph", "metadata.xml"]
            .iter()
            .collect();
        let text = std::fs::read_to_string(&path).expect("metadata present");
        let document = roxmltree::Document::parse(&text).expect("metadata is XML");
        let mut types = HashMap::new();
        for node in document.descendants().filter(|n| {
            matches!(
                n.tag_name().name(),
                "EntityType" | "ComplexType" | "EnumType"
            )
        }) {
            let mut kind = Type {
                base: node.attribute("BaseType").map(local),
                is_enum: node.tag_name().name() == "EnumType",
                ..Type::default()
            };
            for child in node.children().filter(roxmltree::Node::is_element) {
                let name = child.attribute("Name").unwrap_or_default().to_owned();
                match child.tag_name().name() {
                    "Property" | "NavigationProperty" => {
                        let nullable = child.attribute("Nullable") != Some("false");
                        kind.properties.insert(
                            name,
                            (
                                child.attribute("Type").unwrap_or_default().to_owned(),
                                nullable,
                            ),
                        );
                    }
                    "Member" => kind.members.push(name),
                    _ => {}
                }
            }
            types.insert(node.attribute("Name").unwrap_or_default().to_owned(), kind);
        }
        Self { types }
    }

    /// Panics with every way `value` breaks type `kind`: properties the type doesn't have,
    /// nulls where it allows none, wrong JSON types, bad enum members, timestamps and binaries
    /// that don't parse.
    pub fn assert(&self, kind: &str, value: &Value) {
        let mut errors = Vec::new();
        self.check_type(kind, value, kind, &mut errors);
        assert!(
            errors.is_empty(),
            "{kind} violations:\n{}\nin {value:#}",
            errors.join("\n")
        );
    }

    /// Checks each item of a collection response's `value`.
    pub fn assert_collection(&self, kind: &str, body: &Value) {
        let items = body["value"]
            .as_array()
            .unwrap_or_else(|| panic!("no value in {body:#}"));
        for item in items {
            self.assert(kind, item);
        }
    }

    fn find(&self, kind: &str, property: &str) -> Option<&(String, bool)> {
        let mut current = self.types.get(kind);
        while let Some(kind) = current {
            if let Some(found) = kind.properties.get(property) {
                return Some(found);
            }
            current = kind.base.as_deref().and_then(|base| self.types.get(base));
        }
        None
    }

    fn derives(&self, derived: &str, base: &str) -> bool {
        let mut current = Some(derived.to_owned());
        while let Some(kind) = current {
            if kind == base {
                return true;
            }
            current = self.types.get(&kind).and_then(|t| t.base.clone());
        }
        false
    }

    fn check_type(&self, kind: &str, value: &Value, path: &str, errors: &mut Vec<String>) {
        let Some(definition) = self.types.get(kind) else {
            errors.push(format!("{path}: unknown type {kind}"));
            return;
        };
        if definition.is_enum {
            match value.as_str() {
                Some(member) if definition.members.iter().any(|m| m == member) => {}
                _ => errors.push(format!("{path}: {value} isn't a {kind}")),
            }
            return;
        }
        let Some(object) = value.as_object() else {
            errors.push(format!("{path}: expected a {kind} object, got {value}"));
            return;
        };
        let mut kind = kind.to_owned();
        if let Some(named) = object.get("@odata.type").and_then(Value::as_str) {
            let named = named
                .trim_start_matches('#')
                .trim_start_matches("microsoft.graph.");
            if self.derives(named, &kind) {
                kind = named.to_owned();
            } else {
                errors.push(format!("{path}: @odata.type {named} isn't a {kind}"));
            }
        }
        if object.contains_key("@removed") {
            for key in object
                .keys()
                .filter(|key| !matches!(key.as_str(), "id" | "@removed" | "@odata.type"))
            {
                errors.push(format!("{path}.{key}: a removed entry carries only its id"));
            }
            return;
        }
        for (key, item) in object {
            let child = format!("{path}.{key}");
            if key.starts_with('@') {
                if matches!(
                    key.as_str(),
                    "@odata.etag"
                        | "@odata.context"
                        | "@odata.nextLink"
                        | "@odata.deltaLink"
                        | "@odata.mediaEtag"
                ) && !item.is_string()
                {
                    errors.push(format!("{child}: expected a string"));
                }
                continue;
            }
            if key.contains('@') {
                continue;
            }
            match self.find(&kind, key) {
                Some((property_type, nullable)) => {
                    self.check_value(property_type, *nullable, item, &child, errors);
                }
                None => errors.push(format!("{child}: not a property of {kind}")),
            }
        }
    }

    fn check_value(
        &self,
        kind: &str,
        nullable: bool,
        value: &Value,
        path: &str,
        errors: &mut Vec<String>,
    ) {
        if value.is_null() {
            if !nullable {
                errors.push(format!("{path}: null, but {kind} isn't nullable"));
            }
            return;
        }
        if let Some(inner) = kind
            .strip_prefix("Collection(")
            .and_then(|k| k.strip_suffix(')'))
        {
            match value.as_array() {
                Some(items) => {
                    for (index, item) in items.iter().enumerate() {
                        self.check_value(inner, true, item, &format!("{path}[{index}]"), errors);
                    }
                }
                None => errors.push(format!("{path}: expected an array, got {value}")),
            }
            return;
        }
        let ok = match kind {
            "Edm.String" => value.is_string(),
            "Edm.Boolean" => value.is_boolean(),
            "Edm.Int32" | "Edm.Int64" | "Edm.Double" | "Edm.Int16" | "Edm.Byte" => {
                value.is_number()
            }
            "Edm.DateTimeOffset" => value
                .as_str()
                .is_some_and(|t| t.parse::<jiff::Timestamp>().is_ok()),
            "Edm.Date" => value
                .as_str()
                .is_some_and(|t| t.parse::<jiff::civil::Date>().is_ok()),
            "Edm.TimeOfDay" => value
                .as_str()
                .is_some_and(|t| t.parse::<jiff::civil::Time>().is_ok()),
            "Edm.Guid" => value
                .as_str()
                .is_some_and(|t| t.len() == 36 && t.matches('-').count() == 4),
            "Edm.Binary" => value.as_str().is_some_and(|t| {
                use base64::Engine;
                base64::engine::general_purpose::STANDARD.decode(t).is_ok()
                    || base64::engine::general_purpose::URL_SAFE.decode(t).is_ok()
            }),
            "Edm.Stream" => true,
            other => {
                self.check_type(&local(other), value, path, errors);
                true
            }
        };
        if !ok {
            errors.push(format!("{path}: {value} isn't an {kind}"));
        }
    }
}

/// `graph.recipient` → `recipient`.
fn local(name: &str) -> String {
    name.trim_start_matches("graph.")
        .trim_start_matches("microsoft.graph.")
        .to_owned()
}

/// Asserts `body` is Graph's error body with `code`.
pub fn assert_error(reply: &Reply, status: u16, code: &str) {
    assert_eq!(reply.status, status, "{}", reply.json);
    let error = &reply.json["error"];
    assert_eq!(error["code"], code, "{}", reply.json);
    assert!(error["message"].is_string(), "{}", reply.json);
    let inner = &error["innerError"];
    for key in ["date", "request-id", "client-request-id"] {
        assert!(
            inner[key].is_string(),
            "innerError.{key} missing: {}",
            reply.json
        );
    }
}

/// A plain-text email, base64-encoded as Graph's MIME bodies are.
pub fn mime(text: &str) -> Vec<u8> {
    use base64::Engine;
    base64::engine::general_purpose::STANDARD
        .encode(text.replace('\n', "\r\n"))
        .into_bytes()
}

pub fn recipients(addresses: &[&str]) -> Value {
    Value::Array(
        addresses
            .iter()
            .map(|address| json!({ "emailAddress": { "address": address } }))
            .collect(),
    )
}

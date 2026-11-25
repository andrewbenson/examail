//! OData conventions as Graph applies them: system query options, `Prefer` preferences,
//! `@odata.context`, paging links and `$select`.

use axum::extract::FromRequestParts;
use axum::http::HeaderMap;
use axum::http::request::Parts;
use base64::Engine;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use serde::{Serialize, de::DeserializeOwned};
use serde_json::{Map, Value};

use super::error::{GraphError, Result};

/// A request's query: OData's system options (`$select`, `$filter`…) and the rest (custom options
/// such as `changeType` or `includeHiddenFolders`). Option names match in any case, as Graph's do.
#[derive(Debug, Clone, Default)]
pub struct Options {
    pub select: Option<Vec<String>>,
    pub expand: Option<String>,
    pub filter: Option<String>,
    pub orderby: Option<String>,
    pub search: Option<String>,
    pub top: Option<usize>,
    pub skip: usize,
    pub count: bool,
    pub skiptoken: Option<String>,
    pub deltatoken: Option<String>,
    /// Options without a `$`, by lowercased name.
    pub custom: Vec<(String, String)>,
}

impl Options {
    pub fn parse(query: &str) -> Result<Self> {
        let mut options = Self::default();
        for (name, value) in url::form_urlencoded::parse(query.as_bytes()) {
            let Some(system) = name.strip_prefix('$') else {
                options
                    .custom
                    .push((name.to_lowercase(), value.into_owned()));
                continue;
            };
            let value = value.into_owned();
            match system.to_ascii_lowercase().as_str() {
                "select" => {
                    options.select = Some(
                        value
                            .split(',')
                            .map(|field| field.trim().to_owned())
                            .filter(|field| !field.is_empty())
                            .collect(),
                    );
                }
                "expand" => options.expand = Some(value),
                "filter" => options.filter = Some(value),
                "orderby" => options.orderby = Some(value),
                "search" => options.search = Some(value),
                "top" => options.top = Some(number(&name, &value)?),
                "skip" => options.skip = number(&name, &value)?,
                "count" => options.count = value.eq_ignore_ascii_case("true"),
                "skiptoken" => options.skiptoken = Some(value),
                "deltatoken" => options.deltatoken = Some(value),
                _ => {
                    return Err(GraphError::bad_request(format!(
                        "Unrecognized query argument specified: '{name}'."
                    )));
                }
            }
        }
        Ok(options)
    }

    /// A custom option's value.
    pub fn custom(&self, name: &str) -> Option<&str> {
        self.custom
            .iter()
            .rev()
            .find(|(key, _)| key.eq_ignore_ascii_case(name))
            .map(|(_, value)| value.as_str())
    }
}

fn number(name: &str, value: &str) -> Result<usize> {
    value.trim().parse().map_err(|_| {
        GraphError::bad_request(format!(
            "Invalid value '{value}' for query option '{name}'."
        ))
    })
}

impl<S: Send + Sync> FromRequestParts<S> for Options {
    type Rejection = GraphError;

    async fn from_request_parts(parts: &mut Parts, _: &S) -> Result<Self> {
        Self::parse(parts.uri.query().unwrap_or_default())
    }
}

/// The body format a client prefers for `body` and `uniqueBody`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BodyType {
    Text,
    Html,
}

/// What the `Prefer` headers ask for.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Prefer {
    /// `IdType="ImmutableId"`.
    pub immutable_ids: bool,
    /// `outlook.body-content-type="text"` or `"html"`.
    pub body: Option<BodyType>,
    /// `odata.maxpagesize=N`.
    pub max_page: Option<usize>,
    /// `outlook.timezone="Pacific Standard Time"`: the zone events' times are given in.
    pub zone: Option<String>,
}

impl Prefer {
    pub fn from_headers(headers: &HeaderMap) -> Self {
        let mut prefer = Self::default();
        for value in headers.get_all("prefer") {
            for preference in value.to_str().unwrap_or_default().split(',') {
                let (name, value) = preference.split_once('=').unwrap_or((preference, ""));
                let value = value.trim().trim_matches('"');
                match name.trim().to_ascii_lowercase().as_str() {
                    "idtype" => prefer.immutable_ids = value.eq_ignore_ascii_case("ImmutableId"),
                    "outlook.body-content-type" => {
                        prefer.body = match value.to_ascii_lowercase().as_str() {
                            "text" => Some(BodyType::Text),
                            "html" => Some(BodyType::Html),
                            _ => prefer.body,
                        };
                    }
                    "odata.maxpagesize" => prefer.max_page = value.parse().ok().filter(|&n| n > 0),
                    "outlook.timezone" => {
                        prefer.zone = Some(value.to_owned()).filter(|z| !z.is_empty())
                    }
                    _ => {}
                }
            }
        }
        prefer
    }

    /// What a response says it honored, in `Preference-Applied`.
    pub fn applied(&self) -> Option<String> {
        let mut applied = Vec::new();
        if self.immutable_ids {
            applied.push("IdType=\"ImmutableId\"".to_owned());
        }
        match self.body {
            Some(BodyType::Text) => applied.push("outlook.body-content-type=\"text\"".to_owned()),
            Some(BodyType::Html) => applied.push("outlook.body-content-type=\"html\"".to_owned()),
            None => {}
        }
        if let Some(zone) = &self.zone {
            applied.push(format!("outlook.timezone=\"{zone}\""));
        }
        (!applied.is_empty()).then(|| applied.join(", "))
    }
}

impl<S: Send + Sync> FromRequestParts<S> for Prefer {
    type Rejection = std::convert::Infallible;

    async fn from_request_parts(parts: &mut Parts, _: &S) -> Result<Self, Self::Rejection> {
        Ok(Self::from_headers(&parts.headers))
    }
}

/// `@odata.context` for `fragment`, e.g. `users('…')/messages`.
pub fn context(origin: &url::Url, fragment: &str) -> String {
    format!("{origin}v1.0/$metadata#{fragment}")
}

/// An absolute link to `path` (under `/v1.0/`) with `params`, as Graph writes paging links: the
/// `$` of system options left as is.
pub fn link(origin: &url::Url, path: &str, params: &[(String, String)]) -> String {
    let query: Vec<String> = params
        .iter()
        .map(|(name, value)| {
            let value: String = url::form_urlencoded::byte_serialize(value.as_bytes()).collect();
            format!("{name}={}", value.replace('+', "%20"))
        })
        .collect();
    let mut url = format!("{origin}v1.0/{}", path.trim_start_matches('/'));
    if !query.is_empty() {
        url.push('?');
        url.push_str(&query.join("&"));
    }
    url
}

/// The query parameters to carry into a `$skip` page link: every option the request had, with
/// `$skip` moved on.
pub fn next_page(query: &str, skip: usize) -> Vec<(String, String)> {
    let mut params: Vec<(String, String)> = url::form_urlencoded::parse(query.as_bytes())
        .into_owned()
        .filter(|(name, _)| !name.eq_ignore_ascii_case("$skip"))
        .collect();
    params.push(("$skip".to_owned(), skip.to_string()));
    params
}

/// Encodes a JSON pagination or delta state token in URL-safe base64 without padding.
pub fn encode_token<T: Serialize>(value: &T) -> String {
    URL_SAFE_NO_PAD.encode(serde_json::to_vec(value).expect("tokens serialize"))
}

/// Decodes a JSON pagination or delta state token encoded by [`encode_token`].
pub fn decode_token<T: DeserializeOwned>(token: &str) -> Option<T> {
    serde_json::from_slice(&URL_SAFE_NO_PAD.decode(token).ok()?).ok()
}

/// Encodes a plain UTF-8 token in URL-safe base64 without padding.
pub fn encode_text_token(text: impl AsRef<[u8]>) -> String {
    URL_SAFE_NO_PAD.encode(text)
}

/// Decodes a plain UTF-8 token encoded by [`encode_text_token`].
pub fn decode_text_token(token: &str) -> Option<String> {
    String::from_utf8(URL_SAFE_NO_PAD.decode(token).ok()?).ok()
}

/// A collection response.
pub fn collection(
    context: String,
    value: Vec<Value>,
    count: Option<usize>,
    next: Option<String>,
) -> Value {
    let mut body = Map::new();
    body.insert("@odata.context".into(), context.into());
    if let Some(count) = count {
        body.insert("@odata.count".into(), count.into());
    }
    if let Some(next) = next {
        body.insert("@odata.nextLink".into(), next.into());
    }
    body.insert("value".into(), value.into());
    Value::Object(body)
}

/// `object` cut to `select` (and its `id` and annotations), after checking every selected
/// property is one of `known` for `kind`.
pub fn select(
    object: Map<String, Value>,
    select: Option<&[String]>,
    known: &[&str],
    kind: &str,
) -> Result<Map<String, Value>> {
    let Some(select) = select else {
        return Ok(object);
    };
    check_properties(select, known, kind)?;
    Ok(object
        .into_iter()
        .filter(|(name, _)| {
            name == "id"
                || name.starts_with('@')
                || select
                    .iter()
                    .any(|wanted| wanted.eq_ignore_ascii_case(name))
        })
        .collect())
}

/// Rejects names that aren't properties of `kind`.
pub fn check_properties(names: &[String], known: &[&str], kind: &str) -> Result<()> {
    match names
        .iter()
        .find(|name| !known.iter().any(|known| known.eq_ignore_ascii_case(name)))
    {
        Some(unknown) => Err(GraphError::unknown_property(unknown, kind)),
        None => Ok(()),
    }
}

/// Graph's timestamps: UTC, to the second.
pub fn datetime(at: jiff::Timestamp) -> String {
    at.strftime("%Y-%m-%dT%H:%M:%SZ").to_string()
}

/// A `dateTimeTimeZone` in UTC, as Outlook writes them (seven fractional digits).
pub fn date_time_zone(at: jiff::Timestamp) -> Value {
    serde_json::json!({
        "dateTime": at.strftime("%Y-%m-%dT%H:%M:%S.0000000").to_string(),
        "timeZone": "UTC",
    })
}

/// A `dateTimeTimeZone`'s instant.
pub fn parse_date_time_zone(value: &Value) -> Option<jiff::Timestamp> {
    let text = value.get("dateTime")?.as_str()?;
    let zone = value
        .get("timeZone")
        .and_then(Value::as_str)
        .unwrap_or("UTC");
    let civil: jiff::civil::DateTime = text.trim_end_matches('Z').parse().ok()?;
    let tz = gmail::ics::resolve_timezone(zone)?;
    civil.to_zoned(tz).ok().map(|zoned| zoned.timestamp())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn options_match_in_any_case() {
        let options = Options::parse(
            "$Top=5&$orderBy=receivedDateTime%20desc&$select=subject,from&changeType=created",
        )
        .unwrap();
        assert_eq!(options.top, Some(5));
        assert_eq!(options.orderby.as_deref(), Some("receivedDateTime desc"));
        assert_eq!(options.custom("changetype"), Some("created"));
        assert_eq!(options.select.unwrap(), ["subject", "from"]);
        let error = Options::parse("$expnad=attachments").unwrap_err();
        assert_eq!(
            error.message,
            "Unrecognized query argument specified: '$expnad'."
        );
    }

    #[test]
    fn prefer_reads_every_preference() {
        let mut headers = HeaderMap::new();
        headers.append(
            "Prefer",
            "IdType=\"ImmutableId\", odata.maxpagesize=2"
                .parse()
                .unwrap(),
        );
        headers.append(
            "Prefer",
            "outlook.body-content-type=\"text\"".parse().unwrap(),
        );
        let prefer = Prefer::from_headers(&headers);
        assert!(prefer.immutable_ids);
        assert_eq!(prefer.max_page, Some(2));
        assert_eq!(prefer.body, Some(BodyType::Text));
    }

    #[test]
    fn links_keep_dollar_names() {
        let origin = url::Url::parse("http://127.0.0.1:1/").unwrap();
        let link = link(
            &origin,
            "me/messages",
            &next_page("$top=2&$filter=isRead eq false", 2),
        );
        assert_eq!(
            link,
            "http://127.0.0.1:1/v1.0/me/messages?$top=2&$filter=isRead%20eq%20false&$skip=2"
        );
    }

    #[test]
    fn json_and_text_tokens_round_trip_without_padding() {
        let encoded = encode_token(&(42usize, Some(10usize)));
        assert!(!encoded.contains('='));
        assert_eq!(
            decode_token::<(usize, Option<usize>)>(&encoded),
            Some((42, Some(10)))
        );
        assert_eq!(decode_token::<(usize, Option<usize>)>("not-json!"), None);

        let text = encode_text_token("offset 25");
        assert!(!text.contains('='));
        assert_eq!(decode_text_token(&text).as_deref(), Some("offset 25"));
        assert_eq!(decode_text_token("%%%"), None);
    }
}

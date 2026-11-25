//! Microsoft Graph's error body, `{"error": {"code", "message", "innerError"}}`. Handlers render
//! `code` and `message`; [`super::http::finish`] adds `innerError`, which needs the request's IDs.

use axum::http::{HeaderValue, StatusCode, header};
use axum::response::{IntoResponse, Response};
use serde_json::json;

/// A failed Graph request.
#[derive(Debug, Clone, PartialEq)]
pub struct GraphError {
    pub status: u16,
    pub code: String,
    pub message: String,
    /// `Retry-After`, in seconds, for throttling.
    pub retry_after: Option<u64>,
    /// A more specific code, sent as `innerError.code`.
    pub inner_code: Option<&'static str>,
}

pub type Result<T, E = GraphError> = std::result::Result<T, E>;

impl GraphError {
    pub fn new(status: u16, code: impl Into<String>, message: impl Into<String>) -> Self {
        Self {
            status,
            code: code.into(),
            message: message.into(),
            retry_after: None,
            inner_code: None,
        }
    }

    /// 404 for a message, attachment, rule or override that isn't there, as Exchange words it.
    pub fn not_found() -> Self {
        Self::new(
            404,
            "ErrorItemNotFound",
            "The specified object was not found in the store.",
        )
    }

    /// 404 for a folder that isn't there.
    pub fn folder_not_found() -> Self {
        Self::new(
            404,
            "ErrorItemNotFound",
            "The specified folder could not be found in the store.",
        )
    }

    /// 403 for something the signed-in user has no rights to: another user's mailbox, or a
    /// calendar they can only read.
    pub fn access_denied() -> Self {
        Self::new(
            403,
            "ErrorAccessDenied",
            "Access is denied. Check credentials and try again.",
        )
    }

    /// 400 for an ID that isn't one of the mock's.
    pub fn malformed_id() -> Self {
        Self::new(400, "ErrorInvalidIdMalformed", "Id is malformed.")
    }

    pub fn bad_request(message: impl Into<String>) -> Self {
        Self::new(400, "BadRequest", message)
    }

    pub fn invalid_request(message: impl Into<String>) -> Self {
        Self::new(400, "ErrorInvalidRequest", message)
    }

    /// 400 for a `$select`/`$expand`/`$orderby` naming a property the type doesn't have.
    pub fn unknown_property(property: &str, kind: &str) -> Self {
        Self::new(
            400,
            "RequestBroker--ParseUri",
            format!(
                "Could not find a property named '{property}' on type 'microsoft.graph.{kind}'."
            ),
        )
    }

    /// 400 for a request body naming a property the type doesn't have.
    pub fn unknown_body_property(property: &str, kind: &str) -> Self {
        Self::new(
            400,
            "RequestBodyRead",
            format!(
                "The property '{property}' does not exist on type 'microsoft.graph.{kind}'. Make \
                 sure to only use property names that are defined by the type or mark the type \
                 as open type."
            ),
        )
    }

    /// 400 for a body that isn't JSON of the expected shape.
    pub fn unreadable_body(detail: impl std::fmt::Display) -> Self {
        Self::new(
            400,
            "RequestBodyRead",
            format!("Cannot read the request body: {detail}"),
        )
    }

    /// 400 for a `$filter` the endpoint doesn't support.
    pub fn invalid_filter() -> Self {
        Self::new(
            400,
            "ErrorInvalidUrlQueryFilter",
            "The query filter contains one or more invalid nodes.",
        )
    }

    /// 410 for a delta token whose sync state is gone.
    pub fn sync_state_not_found() -> Self {
        Self::new(
            410,
            "SyncStateNotFound",
            "The sync state is no longer available. Restart the sync with a new delta request.",
        )
    }

    /// 429 with a `Retry-After`.
    pub fn throttled(code: &str, message: &str, retry_after: u64) -> Self {
        Self {
            retry_after: Some(retry_after),
            ..Self::new(429, code, message)
        }
    }
}

impl IntoResponse for GraphError {
    fn into_response(self) -> Response {
        let status = StatusCode::from_u16(self.status).unwrap_or(StatusCode::BAD_REQUEST);
        let mut error = json!({ "code": self.code, "message": self.message });
        if let Some(code) = self.inner_code {
            error["innerError"] = json!({ "code": code });
        }
        let mut response = (status, axum::Json(json!({ "error": error }))).into_response();
        if let Some(seconds) = self.retry_after {
            response
                .headers_mut()
                .insert(header::RETRY_AFTER, HeaderValue::from(seconds));
        }
        response
    }
}

impl From<crate::error::Failure> for GraphError {
    /// A mailbox operation refused by Gmail's rules, as Exchange would refuse the same request.
    fn from(failure: crate::error::Failure) -> Self {
        match failure.error.code {
            404 => Self::not_found(),
            409 => Self::new(409, "ErrorFolderExists", failure.error.message),
            code => Self::new(code, "ErrorInvalidRequest", failure.error.message),
        }
    }
}

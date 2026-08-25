//! Error mapping: core taxonomy onto HTTP responses.
//!
//! The taxonomy owns stable public codes; this module only chooses a
//! status and renders the safe message. Storage and external faults
//! are logged with their source chain here - and nowhere else - so
//! handlers stay free of logging boilerplate while internals never
//! reach clients.

use axum::Json;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use hephaestus_core::Error;
use serde::Serialize;

/// A handler error ready to render as an HTTP response.
#[derive(Debug)]
pub struct ApiError(pub Error);

/// Convenience alias for handler return types.
pub type ApiResult<T> = Result<T, ApiError>;

impl From<Error> for ApiError {
    fn from(e: Error) -> Self {
        Self(e)
    }
}

/// Stable response body shape for every failure.
#[derive(Debug, Serialize)]
pub struct ErrorBody {
    /// Machine-readable code; part of the public contract.
    pub code: &'static str,
    /// Safe human-readable explanation.
    pub message: String,
    /// Offending field, when the failure is input validation.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub field: Option<String>,
}

/// HTTP status for a core error. Internal faults collapse into 500;
/// their details live in logs, not responses.
pub fn status_for(err: &Error) -> StatusCode {
    match err {
        Error::Validation { .. } => StatusCode::UNPROCESSABLE_ENTITY,
        Error::NotFound { .. } => StatusCode::NOT_FOUND,
        Error::Conflict { .. } | Error::IllegalTransition { .. } => StatusCode::CONFLICT,
        Error::Unauthenticated => StatusCode::UNAUTHORIZED,
        Error::Forbidden { .. } => StatusCode::FORBIDDEN,
        Error::BudgetExhausted { .. } => StatusCode::TOO_MANY_REQUESTS,
        // Configuration, storage and external faults are server-side.
        Error::Config(_)
        | Error::Storage(_)
        | Error::External { .. }
        | Error::VerificationFailed { .. } => StatusCode::INTERNAL_SERVER_ERROR,
    }
}

/// The safe client-facing message for an error.
fn safe_message(err: &Error) -> String {
    match err {
        Error::Validation { field, message } => format!("{field}: {message}"),
        Error::NotFound { entity } => format!("{entity} not found"),
        Error::Conflict { message } => message.clone(),
        Error::Forbidden { reason } => reason.clone(),
        Error::IllegalTransition { from, reason, .. } => {
            format!("workflow is at {from}; {reason}")
        }
        Error::BudgetExhausted { what } => format!("budget exhausted: {what}"),
        Error::Storage(_) => "internal storage error".into(),
        Error::External { system, .. } => format!("external system error ({system})"),
        Error::Config(_) => "internal configuration error".into(),
        Error::VerificationFailed { layer } => format!("verification failed: {layer}"),
        Error::Unauthenticated => "authentication required".into(),
    }
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        let status = status_for(&self.0);
        if status.is_server_error() {
            log_internal(&self.0);
        }
        let field = match &self.0 {
            Error::Validation { field, .. } => Some(field.clone()),
            _ => None,
        };
        let body = ErrorBody {
            code: self.0.code(),
            message: safe_message(&self.0),
            field,
        };
        (status, Json(body)).into_response()
    }
}

fn log_internal(err: &Error) {
    match err {
        Error::Storage(source) => {
            tracing::error!(code = err.code(), source = ?source, "storage fault");
        }
        Error::External { system, source } => {
            tracing::error!(code = err.code(), system, source = ?source, "external fault");
        }
        other => {
            tracing::error!(code = other.code(), detail = %other, "internal fault");
        }
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

    use super::*;
    use axum::body::to_bytes;

    /// Render an error and decode its body as raw JSON (the wire
    /// format uses a borrowed static code, so clients - like this
    /// test - read it through Value rather than DeserializeOwned).
    fn rendered(err: Error) -> (StatusCode, serde_json::Value) {
        let response = ApiError(err).into_response();
        let status = response.status();
        let bytes = futures_now(response);
        let parsed: std::result::Result<serde_json::Value, _> = serde_json::from_slice(&bytes);
        (status, parsed.expect("json body"))
    }

    fn futures_now(response: Response) -> Vec<u8> {
        // Body collection needs an async context; the test runtime below
        // provides one per call site through block_on.
        tokio::task::block_in_place(|| {
            tokio::runtime::Handle::current().block_on(async move {
                to_bytes(response.into_body(), usize::MAX)
                    .await
                    .expect("body")
                    .to_vec()
            })
        })
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn maps_taxonomy_to_statuses_and_codes() {
        let cases: Vec<(Error, StatusCode, &'static str)> = vec![
            (
                Error::Validation {
                    field: "title".into(),
                    message: "too long".into(),
                },
                StatusCode::UNPROCESSABLE_ENTITY,
                "VALIDATION_FAILED",
            ),
            (
                Error::NotFound { entity: "task" },
                StatusCode::NOT_FOUND,
                "NOT_FOUND",
            ),
            (
                Error::Conflict {
                    message: "run is building".into(),
                },
                StatusCode::CONFLICT,
                "CONFLICT",
            ),
            (
                Error::Unauthenticated,
                StatusCode::UNAUTHORIZED,
                "UNAUTHENTICATED",
            ),
            (
                Error::Forbidden {
                    reason: "read-only role".into(),
                },
                StatusCode::FORBIDDEN,
                "FORBIDDEN",
            ),
            (
                Error::BudgetExhausted { what: "tool-calls" },
                StatusCode::TOO_MANY_REQUESTS,
                "BUDGET_EXHAUSTED",
            ),
            (
                Error::Storage(Box::new(std::io::Error::other("boom"))),
                StatusCode::INTERNAL_SERVER_ERROR,
                "STORAGE_ERROR",
            ),
        ];
        for (err, expected_status, expected_code) in cases {
            let (status, body) = rendered(err);
            assert_eq!(status, expected_status);
            assert_eq!(body["code"], expected_code);
        }
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn storage_details_never_reach_the_client() {
        let secret = "postgres://admin:hunter2@db/x";
        let err = Error::Storage(Box::new(std::io::Error::other(secret.to_string())));
        let (_status, body) = rendered(err);
        let message = body["message"].as_str().expect("message");
        assert!(!message.contains("hunter2"), "leak: {message}");
        assert_eq!(message, "internal storage error");
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn validation_errors_name_the_field() {
        let (_status, body) = rendered(Error::Validation {
            field: "title".into(),
            message: "must not be empty".into(),
        });
        assert_eq!(body["field"], "title");
    }
}

//! JSON API: one module per route family, mirroring the JavaScript edition's
//! `server/index.js` so behaviour stays comparable route by route.
//!
//! Conventions:
//! * every handler returns [`ApiResult`], which serializes to `200 {json}` or
//!   `{status} {"error": "..."}` exactly like the old `fail()` helper;
//! * SQL lives in the handler, in the same shape the old code used, through the
//!   helpers in [`storyforge_core::json_sql`];
//! * path parameters are parsed as `i64`; a non-numeric id answers 404 because
//!   the old code sent `Number('abc')` (NaN) to SQLite and found nothing.

pub mod ai;
pub mod annotations;
pub mod books;
pub mod chapters;
pub mod graph;
pub mod import;
pub mod mcp;
pub mod state;
pub mod world;

use std::sync::Arc;

use axum::Json;
use axum::Router;
use axum::body::Bytes;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use serde_json::{Value, json};

use storyforge_core::SfError;

use crate::server::AppState;

pub type ApiResult = Result<Json<Value>, ApiError>;
pub type AppRouter = Router<Arc<AppState>>;

/// Error rendered as `{"error": "…"}` with the status the old `fail()` used.
#[derive(Debug)]
pub struct ApiError {
    pub status: u16,
    pub message: String,
}

impl ApiError {
    pub fn new(status: u16, message: impl Into<String>) -> Self {
        Self {
            status,
            message: message.into(),
        }
    }

    pub fn invalid(message: impl Into<String>) -> Self {
        Self::new(400, message)
    }

    pub fn not_found(message: impl Into<String>) -> Self {
        Self::new(404, message)
    }

    pub fn conflict(message: impl Into<String>) -> Self {
        Self::new(409, message)
    }
}

impl From<SfError> for ApiError {
    fn from(err: SfError) -> Self {
        let status = err.status();
        // Ship the message itself, not `thiserror`'s `"conflict: …"` display,
        // so the JSON `error` field matches the JavaScript edition byte for byte.
        let message = match &err {
            SfError::NotFound(text)
            | SfError::Invalid(text)
            | SfError::Conflict(text)
            | SfError::Config(text)
            | SfError::Other(text) => text.clone(),
            other => other.to_string(),
        };
        ApiError::new(status, message)
    }
}

impl From<serde_json::Error> for ApiError {
    fn from(err: serde_json::Error) -> Self {
        ApiError::new(500, err.to_string())
    }
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        let status = StatusCode::from_u16(self.status).unwrap_or(StatusCode::INTERNAL_SERVER_ERROR);
        (status, Json(json!({ "error": self.message }))).into_response()
    }
}

/// Parse a path segment the way `Number(params.id)` behaved: anything that is
/// not an integer simply matches no row.
pub fn path_id(raw: &str) -> Result<i64, ApiError> {
    raw.trim()
        .parse::<i64>()
        .map_err(|_| ApiError::not_found("Не найдено"))
}

/// Parse a request body; an empty body is `{}` like `readBody()` produced.
pub fn parse_body(body: &Bytes) -> Result<Value, ApiError> {
    if body.is_empty() {
        return Ok(json!({}));
    }
    serde_json::from_slice(body).map_err(|_| ApiError::invalid("Некорректный JSON в теле запроса"))
}

/// `Number(query.x)` semantics for optional numeric query parameters.
pub fn query_num(value: Option<&str>) -> Option<i64> {
    value.and_then(|raw| raw.trim().parse::<i64>().ok())
}

/// `String(query.x ?? '')`.
pub fn query_str(value: Option<&String>) -> String {
    value.cloned().unwrap_or_default()
}

pub fn router() -> AppRouter {
    Router::new()
        .merge(books::routes())
        .merge(chapters::routes())
        .merge(world::routes())
        .merge(state::routes())
        .merge(graph::routes())
        .merge(annotations::routes())
        .merge(ai::routes())
        .merge(import::routes())
        .merge(mcp::routes())
}

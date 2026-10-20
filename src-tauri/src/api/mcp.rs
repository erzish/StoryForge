//! MCP Streamable HTTP transport (R34–R36), the port of the `/mcp` routes in
//! `server/index.js`.
//!
//! One endpoint, JSON-RPC 2.0 over POST. The server returns no SSE stream and
//! keeps no sessions, so:
//! * `POST /mcp` → `200` with the JSON-RPC reply, or `202` with an empty body
//!   when the message was a notification;
//! * `GET` / `DELETE /mcp` → `405` with the same Russian explanations the
//!   JavaScript edition sent.
//!
//! Only `POST` from a page on `localhost` / `127.0.0.1` is accepted. The
//! `Origin` header is optional: clients such as Claude Code do not send one.

use std::sync::Arc;

use axum::body::Bytes;
use axum::extract::State;
use axum::http::{HeaderMap, StatusCode, header};
use axum::response::{IntoResponse, Response};
use axum::routing::post;
use serde_json::{Value, json};

use storyforge_core::mcp;

use super::{AppRouter, parse_body};
use crate::server::AppState;

pub fn routes() -> AppRouter {
    axum::Router::new().route(
        "/mcp",
        post(post_mcp).get(get_mcp).delete(delete_mcp),
    )
}

/// `{"error": "…"}` with an explicit status, compact like `JSON.stringify`.
fn json_response(status: StatusCode, value: &Value) -> Response {
    let body = serde_json::to_vec(value).unwrap_or_else(|_| b"{}".to_vec());
    (
        status,
        [(header::CONTENT_TYPE, "application/json")],
        body,
    )
        .into_response()
}

/// `originAllowed`: no header is fine, otherwise the header must parse and its
/// host must be loopback.
fn origin_allowed(headers: &HeaderMap) -> bool {
    let Some(value) = headers.get(header::ORIGIN) else {
        return true;
    };
    let Ok(origin) = value.to_str() else {
        return false;
    };
    if origin.is_empty() {
        return true;
    }
    host_of(origin)
        .map(|host| host == "localhost" || host == "127.0.0.1")
        .unwrap_or(false)
}

/// `new URL(origin).hostname`, case-folded the way the URL parser does.
fn host_of(origin: &str) -> Option<String> {
    let uri: axum::http::Uri = origin.parse().ok()?;
    Some(uri.authority()?.host().to_ascii_lowercase())
}

async fn post_mcp(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    if !origin_allowed(&headers) {
        return json_response(
            StatusCode::FORBIDDEN,
            &json!({ "error": "Недопустимый Origin" }),
        );
    }
    if body.is_empty() {
        return StatusCode::ACCEPTED.into_response();
    }
    let message = match parse_body(&body) {
        Ok(message) => message,
        Err(err) => return err.into_response(),
    };
    let result = mcp::dispatch(&state.db, &message);
    if !result.has_reply {
        return StatusCode::ACCEPTED.into_response();
    }
    match result.payload {
        Some(payload) => json_response(StatusCode::OK, &payload),
        None => StatusCode::ACCEPTED.into_response(),
    }
}

async fn get_mcp() -> Response {
    json_response(
        StatusCode::METHOD_NOT_ALLOWED,
        &json!({ "error": "GET не поддерживается — сервер не отдаёт SSE-поток" }),
    )
}

async fn delete_mcp() -> Response {
    json_response(
        StatusCode::METHOD_NOT_ALLOWED,
        &json!({ "error": "DELETE не поддерживается — управление сессиями не реализовано" }),
    )
}

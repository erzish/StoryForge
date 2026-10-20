//! Import and export routes (R32–R33).
//!
//! Port of the `/api/import` and `/api/import-all` handlers in
//! `server/index.js`. The JavaScript edition had **no** export route — export
//! was `npm run export` only — so no HTTP surface is invented here; the
//! equivalent lives in [`storyforge_core::export`] and can be called from a
//! Tauri command or a future route.
//!
//! Import rebuilds the whole book in one transaction (see
//! [`storyforge_core::import`]); a failure answers 500 and leaves the previous
//! book untouched. `import-all` stops at the first failing book, exactly like
//! `importAll`, so books imported before it stay committed.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use axum::Json;
use axum::body::Bytes;
use axum::extract::State;
use axum::routing::post;
use serde_json::{Value, json};

use storyforge_core::import;

use super::{ApiError, ApiResult, AppRouter, parse_body};
use crate::server::AppState;

/// `importBook` answers `{bookId, title, stats, warnings}`.
const MISSING_PATH: &str = "Нужен путь к папке книги";
/// `POST /api/import-all` with an empty library.
const NO_BOOKS: &str = "Книги не найдены — папка с book.md рядом со StoryForge не обнаружена";

pub fn routes() -> AppRouter {
    axum::Router::new()
        .route("/api/import", post(import_one))
        .route("/api/import-all", post(import_everything))
}

async fn import_one(State(state): State<Arc<AppState>>, body: Bytes) -> ApiResult {
    let body = parse_body(&body)?;
    let Some(raw) = body
        .get("path")
        .and_then(Value::as_str)
        .filter(|path| !path.trim().is_empty())
    else {
        return Err(ApiError::invalid(MISSING_PATH));
    };
    let book_dir = resolve_input_path(raw)?;
    let outcome = import::import_book(&state.db, &book_dir)?;
    Ok(Json(outcome.to_json()))
}

async fn import_everything(State(state): State<Arc<AppState>>) -> ApiResult {
    let root = import::default_library_root();
    let outcomes = import::import_all(&state.db, &root)?;
    if outcomes.is_empty() {
        return Err(ApiError::not_found(NO_BOOKS));
    }
    let books: Vec<Value> = outcomes
        .iter()
        .map(|outcome| {
            json!({
                "title": outcome.title,
                "stats": Value::Object(outcome.stats.clone()),
                "warnings": outcome.warnings,
            })
        })
        .collect();
    Ok(Json(json!({ "books": books })))
}

/// `path.resolve(body.path)`: a relative path is taken from the working
/// directory, and `..` is normalised lexically (on Windows by
/// `GetFullPathNameW`), without touching the filesystem or following links.
fn resolve_input_path(raw: &str) -> Result<PathBuf, ApiError> {
    std::path::absolute(Path::new(raw))
        .map_err(|error| ApiError::new(500, error.to_string()))
}

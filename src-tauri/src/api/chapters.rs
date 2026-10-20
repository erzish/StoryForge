//! Chapters: list, prose text with revisions, version history (R21–R26).
//!
//! Every route here is thin: the file and revision semantics live in
//! [`storyforge_core::chapter_text`], which reproduces `lib/chapterText.js`.
//! What this module owns is the shape of the JSON the old handlers returned:
//!
//! * the text route spreads the whole `chapter` row (including `root_path`
//!   from the joined book) and adds `body`, `revision` and, only when the file
//!   is gone while `file_path` is set, `missing: true`;
//! * a save answers `{ok, revision, chars_no_space, words}`, plus
//!   `unchanged: true` when the body was already on disk;
//! * the version list is limited to the newest 100 rows and exposes
//!   `LENGTH(body)` as `chars` (SQLite counts characters, not bytes).

use std::sync::Arc;

use axum::Json;
use axum::body::Bytes;
use axum::extract::{Path, State};
use axum::routing::{get, post};
use serde_json::{Map, Value, json};

use storyforge_core::chapter_text::{self, SaveOptions};
use storyforge_core::json_sql::{self, query, query_one};

use super::{ApiError, ApiResult, AppRouter, parse_body, path_id};
use crate::server::AppState;

pub fn routes() -> AppRouter {
    axum::Router::new()
        .route("/api/books/{id}/chapters", get(list_chapters))
        .route("/api/chapters/{id}/text", get(get_text).put(put_text))
        .route("/api/chapters/{id}/versions", get(list_versions))
        .route("/api/chapters/{id}/versions/{version}", get(get_version))
        .route(
            "/api/chapters/{id}/versions/{version}/restore",
            post(restore_version),
        )
}

/// Chapter list of a book: the row plus arc name/colour and two counters, by
/// `number`. An unknown book answers an empty array, not 404.
async fn list_chapters(State(state): State<Arc<AppState>>, Path(id): Path<String>) -> ApiResult {
    let book_id = path_id(&id)?;
    let rows = state.db.with_conn(|conn| {
        query(
            conn,
            "SELECT ch.*, a.name AS arc_name, a.color AS arc_color,
               (SELECT COUNT(*) FROM event e WHERE e.chapter_id=ch.id) AS event_count,
               (SELECT COUNT(*) FROM annotation an WHERE an.chapter_id=ch.id AND an.status='открыта') AS open_notes
             FROM chapter ch LEFT JOIN arc a ON a.id=ch.arc_id
             WHERE ch.book_id=? ORDER BY ch.number",
            &[json!(book_id)],
        )
    })?;
    Ok(Json(Value::Array(rows)))
}

/// Chapter row plus prose. `missing` appears only when `file_path` is set and
/// the file is absent; a chapter without prose yet has an empty body and no
/// `missing` key.
async fn get_text(State(state): State<Arc<AppState>>, Path(id): Path<String>) -> ApiResult {
    let chapter_id = path_id(&id)?;
    let text = state
        .db
        .with_conn(|conn| chapter_text::read_chapter_text(conn, chapter_id))?;

    let mut object = text.chapter;
    if let Some(map) = object.as_object_mut() {
        map.insert("body".into(), json!(text.body));
        map.insert("revision".into(), json!(text.revision));
        if text.missing {
            map.insert("missing".into(), Value::Bool(true));
        }
    }
    Ok(Json(object))
}

/// Save prose. `body` must be a JSON string (else 400 `Нет текста`), which is
/// checked before the chapter is looked up, exactly like the old handler.
///
/// `expected_revision` is optional: when present and stale the answer is 409,
/// and when the body already matches the file nothing is written at all.
async fn put_text(
    State(state): State<Arc<AppState>>,
    Path(id): Path<String>,
    body: Bytes,
) -> ApiResult {
    let input = parse_body(&body)?;
    let chapter_id = path_id(&id)?;
    let Some(text) = input.get("body").and_then(Value::as_str) else {
        return Err(ApiError::invalid("Нет текста"));
    };
    let options = SaveOptions {
        expected_revision: expected_revision(&input),
        reason: reason_from(&input),
        ..SaveOptions::default()
    };
    let outcome = chapter_text::save_chapter_text(&state.db, chapter_id, text, &options)?;
    Ok(Json(save_response(&outcome)))
}

/// Version list, newest first, without the bodies.
async fn list_versions(State(state): State<Arc<AppState>>, Path(id): Path<String>) -> ApiResult {
    let chapter_id = path_id(&id)?;
    let rows = state.db.with_conn(|conn| {
        query(
            conn,
            "SELECT id, reason, created_at, LENGTH(body) AS chars
             FROM chapter_version WHERE chapter_id=? ORDER BY id DESC LIMIT 100",
            &[json!(chapter_id)],
        )
    })?;
    Ok(Json(Value::Array(rows)))
}

/// One version with its full body.
async fn get_version(
    State(state): State<Arc<AppState>>,
    Path((id, version)): Path<(String, String)>,
) -> ApiResult {
    let chapter_id = path_id(&id)?;
    let version_id = path_id(&version)?;
    let row = state.db.with_conn(|conn| {
        query_one(
            conn,
            "SELECT * FROM chapter_version WHERE chapter_id=? AND id=?",
            &[json!(chapter_id), json!(version_id)],
        )
    })?;
    match row {
        Some(row) => Ok(Json(row)),
        None => Err(ApiError::not_found("Версия не найдена")),
    }
}

/// Restore a version: read its body, then save it as ordinary prose with the
/// reason `восстановление версии` — which itself versions the pre-restore text.
async fn restore_version(
    State(state): State<Arc<AppState>>,
    Path((id, version)): Path<(String, String)>,
    body: Bytes,
) -> ApiResult {
    let input = parse_body(&body)?;
    let chapter_id = path_id(&id)?;
    let version_id = path_id(&version)?;

    let previous = state.db.with_conn(|conn| {
        let row = query_one(
            conn,
            "SELECT body FROM chapter_version WHERE chapter_id=? AND id=?",
            &[json!(chapter_id), json!(version_id)],
        )?;
        Ok::<Option<String>, ApiError>(
            row.and_then(|row| row.get("body").and_then(Value::as_str).map(str::to_owned)),
        )
    })?;
    let Some(text) = previous else {
        return Err(ApiError::not_found("Версия не найдена"));
    };

    let options = SaveOptions {
        expected_revision: expected_revision(&input),
        reason: chapter_text::REASON_RESTORE.to_string(),
        ..SaveOptions::default()
    };
    let outcome = chapter_text::save_chapter_text(&state.db, chapter_id, &text, &options)?;
    Ok(Json(save_response(&outcome)))
}

/// `{ok, revision, chars_no_space, words}` with `unchanged: true` folded in
/// for a save that wrote nothing.
fn save_response(outcome: &chapter_text::SaveOutcome) -> Value {
    let mut map = Map::new();
    map.insert("ok".into(), Value::Bool(true));
    if outcome.unchanged {
        map.insert("unchanged".into(), Value::Bool(true));
    }
    map.insert("revision".into(), json!(outcome.revision));
    map.insert("chars_no_space".into(), json!(outcome.chars_no_space));
    map.insert("words".into(), json!(outcome.words));
    Value::Object(map)
}

/// `expected_revision` is only enforced when it is present and not null; any
/// non-string value can never match the revision and therefore conflicts.
fn expected_revision(input: &Value) -> Option<String> {
    match input.get("expected_revision") {
        None | Some(Value::Null) => None,
        Some(Value::String(text)) => Some(text.clone()),
        Some(other) => Some(other.to_string()),
    }
}

/// `reason || 'ручная правка'`: an empty string falls back to the default.
fn reason_from(input: &Value) -> String {
    match input.get("reason") {
        Some(value) if json_sql::truthy(value) => json_sql::text(value),
        _ => chapter_text::REASON_MANUAL.to_string(),
    }
}

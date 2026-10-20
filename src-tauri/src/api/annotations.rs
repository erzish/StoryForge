//! Annotations (notes) and edit application (R74–R79).
//!
//! Ports the note routes of `server/index.js`:
//!
//! * `GET/POST /api/chapters/:id/annotations`,
//! * `PATCH/DELETE /api/annotations/:id`,
//! * `GET /api/chapters/:id/ai-context`,
//! * `POST /api/chapters/:id/apply-edits`.
//!
//! The JavaScript edition has no book-level note list (the only other
//! `annotation` reads are counters and the AI suggest-edits route), so no book
//! route is registered here.
//!
//! Parity details worth keeping:
//!
//! * note status values are wire strings (`открыта`, `применена`,
//!   `выполнена`); `PATCH` stamps `resolved_at` for anything that is not
//!   exactly `открыта`, and does so before the column whitelist is applied;
//! * `ai-context` reads the prose only when `chapter.file_path` is a non-empty
//!   string. A chapter without `file_path` answers an empty `text` even if the
//!   derived `chapters/chNNN.md` exists — unlike `chapter_text::read_chapter_text`,
//!   which would fall back to that derived name;
//! * a lost anchor answers without `paragraph`, `paragraph_before` and
//!   `paragraph_after`: the JavaScript objects left those entries `undefined`
//!   and `JSON.stringify` dropped them, so inserting `null` would change the
//!   response;
//! * `apply-edits` answers 409 with the exact message the old route used when
//!   `expected_revision` is missing or stale (a missing value can never match);
//! * the prose read, revision gate, note lookup and planning all run under one
//!   `with_conn`, then the save (which takes the database lock itself) runs
//!   outside it, then the applied notes are stamped.

use std::collections::HashMap;
use std::path::Path as FsPath;
use std::sync::Arc;

use axum::Json;
use axum::body::Bytes;
use axum::extract::{Path, State};
use axum::routing::{get, patch, post};
use serde_json::{Map, Value, json};

use storyforge_core::anchor::{self, PlannedEdit};
use storyforge_core::chapter_text::{self, SaveOptions};
use storyforge_core::json_sql::{self, exec, insert, query, query_one};

use super::{ApiError, ApiResult, AppRouter, parse_body, path_id};
use crate::server::AppState;

/// `EDITABLE.annotation` from `lib/crud.js` — the only columns a patch may
/// touch, and the reason arbitrary column names never reach the SQL text.
const EDITABLE: [&str; 8] = [
    "note",
    "status",
    "ai_result",
    "quote",
    "prefix",
    "suffix",
    "start_pos",
    "end_pos",
];

const CHAPTER_NOT_FOUND: &str = "Глава не найдена";
const NO_EDITS: &str = "Нет правок";
const NO_TEXT_YET: &str = "У главы ещё нет текста";
const FILE_NOT_FOUND: &str = "Файл главы не найден";
const REVISION_CHANGED: &str =
    "Глава изменилась после просмотра правок. Получите предпросмотр заново.";
/// Version reason recorded by `apply-edits`.
const REASON_NOTES: &str = "правки по заметкам";

pub fn routes() -> AppRouter {
    axum::Router::new()
        .route(
            "/api/chapters/{id}/annotations",
            get(list_annotations).post(create_annotation),
        )
        .route(
            "/api/annotations/{id}",
            patch(update_annotation).delete(remove_annotation),
        )
        .route("/api/chapters/{id}/ai-context", get(ai_context))
        .route("/api/chapters/{id}/apply-edits", post(apply_edits))
}

/// Notes of a chapter, ordered by their stored anchor. `start_pos` is nullable,
/// and SQLite sorts NULLs first here, exactly like the original query.
async fn list_annotations(State(state): State<Arc<AppState>>, Path(id): Path<String>) -> ApiResult {
    let chapter_id = path_id(&id)?;
    let rows = state.db.with_conn(|conn| {
        query(
            conn,
            "SELECT * FROM annotation WHERE chapter_id=? ORDER BY start_pos",
            &[json!(chapter_id)],
        )
    })?;
    Ok(Json(Value::Array(rows)))
}

/// Create a note and answer the created row.
///
/// `quote` and `note` default to `''`, the three anchor columns to NULL — the
/// `??` chain of the old handler. A card is created as soon as the author
/// selects text, so the note text itself may be empty; the interface deletes
/// empty cards.
async fn create_annotation(
    State(state): State<Arc<AppState>>,
    Path(id): Path<String>,
    body: Bytes,
) -> ApiResult {
    let input = parse_body(&body)?;
    let chapter_id = path_id(&id)?;

    let created = state
        .db
        .with_conn(|conn| -> Result<Option<Value>, ApiError> {
            let new_id = insert(
                conn,
                "INSERT INTO annotation (chapter_id, quote, prefix, suffix, start_pos, end_pos, note)
                 VALUES (?,?,?,?,?,?,?)",
                &[
                    json!(chapter_id),
                    or_default(&input, "quote", json!("")),
                    or_default(&input, "prefix", Value::Null),
                    or_default(&input, "suffix", Value::Null),
                    or_default(&input, "start_pos", Value::Null),
                    or_default(&input, "end_pos", Value::Null),
                    or_default(&input, "note", json!("")),
                ],
            )?;
            Ok(query_one(
                conn,
                "SELECT * FROM annotation WHERE id=?",
                &[json!(new_id)],
            )?)
        })?;

    Ok(Json(created.unwrap_or(Value::Null)))
}

/// Partial update: stamp `resolved_at` when the status leaves `открыта`, then
/// write only the whitelisted columns that arrived, then answer the row.
///
/// Both statements run without a transaction (the original did the same), and
/// the `resolved_at` update is unconditional on the row existing.
async fn update_annotation(
    State(state): State<Arc<AppState>>,
    Path(id): Path<String>,
    body: Bytes,
) -> ApiResult {
    let input = parse_body(&body)?;
    let annotation_id = path_id(&id)?;

    let mut columns: Vec<&str> = Vec::new();
    let mut params: Vec<Value> = Vec::new();
    if let Some(map) = input.as_object() {
        for (key, value) in map {
            if EDITABLE.contains(&key.as_str()) {
                columns.push(key);
                params.push(value.clone());
            }
        }
    }
    params.push(json!(annotation_id));
    // Column names come from the whitelist above, never from the request.
    let assignments = (!columns.is_empty()).then(|| {
        columns
            .iter()
            .map(|column| format!("{column}=?"))
            .collect::<Vec<_>>()
            .join(", ")
    });

    let status = input.get("status");
    let row = state
        .db
        .with_conn(|conn| -> Result<Option<Value>, ApiError> {
            if status.is_some_and(json_sql::truthy) && status != Some(&json!("открыта")) {
                exec(
                    conn,
                    "UPDATE annotation SET resolved_at=datetime('now') WHERE id=?",
                    &[json!(annotation_id)],
                )?;
            }
            if let Some(assignments) = &assignments {
                exec(
                    conn,
                    &format!("UPDATE annotation SET {assignments} WHERE id=?"),
                    &params,
                )?;
            }
            Ok(query_one(
                conn,
                "SELECT * FROM annotation WHERE id=?",
                &[json!(annotation_id)],
            )?)
        })?;

    Ok(Json(row.unwrap_or(Value::Null)))
}

/// Delete one note and answer `{changed: n}`, like `crud.remove`.
async fn remove_annotation(
    State(state): State<Arc<AppState>>,
    Path(id): Path<String>,
) -> ApiResult {
    let annotation_id = path_id(&id)?;
    let changed = state.db.with_conn(|conn| {
        exec(
            conn,
            "DELETE FROM annotation WHERE id=?",
            &[json!(annotation_id)],
        )
    })?;
    Ok(Json(json!({ "changed": changed })))
}

/// Everything the model needs to find and rewrite passages: the chapter, the
/// book, the prose and the open notes located against it.
async fn ai_context(State(state): State<Arc<AppState>>, Path(id): Path<String>) -> ApiResult {
    let chapter_id = path_id(&id)?;
    state.db.with_conn(|conn| -> Result<Json<Value>, ApiError> {
        let Some(chapter) = query_one(
            conn,
            "SELECT ch.*, b.root_path, b.title AS book_title, b.active_style,
                    b.min_chars, b.pov AS book_pov
             FROM chapter ch JOIN book b ON b.id=ch.book_id WHERE ch.id=?",
            &[json!(chapter_id)],
        )?
        else {
            return Err(ApiError::not_found(CHAPTER_NOT_FOUND));
        };

        let text = prose_text(&chapter)?;
        let notes = query(
            conn,
            "SELECT * FROM annotation WHERE chapter_id=? AND status='открыта' ORDER BY start_pos",
            &[json!(chapter_id)],
        )?;
        let notes: Vec<Value> = notes.iter().map(|note| note_context(note, &text)).collect();

        // `ch.pov ?? ch.book_pov`: only NULL/absent falls back, an empty string
        // does not.
        let pov = match chapter.get("pov") {
            None | Some(Value::Null) => json_sql::field(&chapter, "book_pov"),
            Some(value) => value.clone(),
        };

        Ok(Json(json!({
            "chapter": {
                "id": json_sql::field(&chapter, "id"),
                "number": json_sql::field(&chapter, "number"),
                "title": json_sql::field(&chapter, "title"),
                "pov": pov,
                "summary": json_sql::field(&chapter, "summary"),
                "chars_no_space": json_sql::field(&chapter, "chars_no_space"),
                "min_chars": json_sql::field(&chapter, "min_chars"),
            },
            "book": {
                "title": json_sql::field(&chapter, "book_title"),
                "style": json_sql::field(&chapter, "active_style"),
            },
            "text": text,
            "notes": notes,
        })))
    })
}

/// Apply the model's exact replacements, or preview them with `dry_run`.
///
/// Each replacement must name a unique fragment; [`anchor::plan_edits`] owns
/// that validation and [`anchor::apply_planned`] applies the survivors from the
/// end. A real run versions the previous prose (inside
/// [`chapter_text::save_chapter_text`]) and stamps the notes whose edits passed
/// as `применена`.
async fn apply_edits(
    State(state): State<Arc<AppState>>,
    Path(id): Path<String>,
    body: Bytes,
) -> ApiResult {
    let input = parse_body(&body)?;
    let chapter_id = path_id(&id)?;
    let Some(edits) = input
        .get("edits")
        .and_then(Value::as_array)
        .filter(|edits| !edits.is_empty())
    else {
        return Err(ApiError::invalid(NO_EDITS));
    };
    let dry_run = input.get("dry_run").is_some_and(json_sql::truthy);
    let expected = input.get("expected_revision").and_then(Value::as_str);

    // Read and plan under one connection: 404 for an unknown chapter, 400 when
    // the chapter has no prose yet or its file is gone, then the revision gate
    // (skipped for a dry run), then note lookup and planning.
    let prepared = state.db.with_conn(|conn| -> Result<Prepared, ApiError> {
        let text = chapter_text::read_chapter_text(conn, chapter_id)?;
        if !text.has_file_path {
            return Err(ApiError::invalid(NO_TEXT_YET));
        }
        if text.missing {
            return Err(ApiError::invalid(FILE_NOT_FOUND));
        }
        if !dry_run && expected != Some(text.revision.as_str()) {
            return Err(ApiError::conflict(REVISION_CHANGED));
        }

        let notes = query(
            conn,
            "SELECT * FROM annotation WHERE chapter_id=?",
            &[json!(chapter_id)],
        )?;
        let mut notes_by_id = HashMap::with_capacity(notes.len());
        for note in notes {
            if let Some(id) = note.get("id").and_then(json_sql::num) {
                notes_by_id.insert(id, note);
            }
        }

        let outcome = anchor::plan_edits(&text.body, edits, &notes_by_id);
        Ok(Prepared {
            chars_no_space: json_sql::field(&text.chapter, "chars_no_space"),
            body: text.body,
            revision: text.revision,
            planned: outcome.planned,
            rejected: outcome.rejected,
        })
    })?;

    if dry_run {
        return Ok(Json(json!({
            "dry_run": true,
            "revision": prepared.revision,
            "preview": anchor::apply_planned(&prepared.body, &prepared.planned),
            "applied": applied(&prepared.planned),
            "rejected": prepared.rejected,
        })));
    }

    if prepared.planned.is_empty() {
        // Nothing to write: the chapter row is echoed as it was, including a
        // NULL `chars_no_space`.
        return Ok(Json(json!({
            "applied": [],
            "rejected": prepared.rejected,
            "chars_no_space": prepared.chars_no_space,
        })));
    }

    let next = anchor::apply_planned(&prepared.body, &prepared.planned);
    let saved = chapter_text::save_chapter_text(
        &state.db,
        chapter_id,
        &next,
        &SaveOptions {
            expected_revision: Some(prepared.revision),
            reason: REASON_NOTES.to_string(),
            ..SaveOptions::default()
        },
    )?;

    // Notes whose edits went through are done. A planned edit without an
    // `annotation_id` (or with a falsy one) leaves its note untouched.
    state.db.with_conn(|conn| -> Result<(), ApiError> {
        for edit in &prepared.planned {
            let Some(annotation_id) = edit.annotation_id.as_ref() else {
                continue;
            };
            if !json_sql::truthy(annotation_id) {
                continue;
            }
            let Some(annotation_id) = json_sql::num(annotation_id) else {
                continue;
            };
            exec(
                conn,
                "UPDATE annotation SET status='применена', resolved_at=datetime('now'), ai_result=?
                 WHERE id=? AND chapter_id=?",
                &[json!(edit.new), json!(annotation_id), json!(chapter_id)],
            )?;
        }
        Ok(())
    })?;

    Ok(Json(json!({
        "applied": applied(&prepared.planned),
        "rejected": prepared.rejected,
        "chars_no_space": saved.chars_no_space,
        "revision": saved.revision,
    })))
}

/// What a real or previewed run needs after the database lock is released.
struct Prepared {
    body: String,
    revision: String,
    chars_no_space: Value,
    planned: Vec<PlannedEdit>,
    rejected: Vec<Value>,
}

/// The prose of a chapter as `ai-context` reads it.
///
/// The old handler used `join(root_path, file_path)` only when `file_path` was
/// truthy and answered `''` when the file did not exist, so a missing
/// `file_path` never falls back to the derived `chapters/chNNN.md` name.
fn prose_text(chapter: &Value) -> Result<String, ApiError> {
    let Some(file_path) = chapter
        .get("file_path")
        .and_then(Value::as_str)
        .filter(|path| !path.is_empty())
    else {
        return Ok(String::new());
    };
    let root = chapter
        .get("root_path")
        .and_then(Value::as_str)
        .unwrap_or_default();
    let full = FsPath::new(root).join(file_path);
    if !full.exists() {
        return Ok(String::new());
    }
    let bytes = std::fs::read(&full).map_err(|error| ApiError::new(500, error.to_string()))?;
    Ok(String::from_utf8_lossy(&bytes).into_owned())
}

/// One located note in the `ai-context` shape.
///
/// A lost anchor omits the three paragraph fields, because the JavaScript
/// object had them `undefined` and `JSON.stringify` dropped the keys.
fn note_context(note: &Value, text: &str) -> Value {
    let located = anchor::locate_note(text, note);
    let mut out = Map::new();
    out.insert("id".into(), json_sql::field(note, "id"));
    out.insert("note".into(), json_sql::field(note, "note"));
    out.insert("quote".into(), json_sql::field(note, "quote"));
    out.insert("found".into(), Value::Bool(located.found));
    out.insert(
        "matched_by".into(),
        located.how.map_or(Value::Null, |how| json!(how)),
    );
    out.insert("unique".into(), Value::Bool(located.unique));
    out.insert("occurrences".into(), json!(located.occurrences));
    out.insert(
        "paragraph_index".into(),
        match located.paragraph_index {
            Some(index) => json!(index),
            // A fragment that starts in the blank-line gap between paragraphs
            // has no paragraph: JavaScript's `findIndex` reported -1 and
            // `?? null` kept it, so `-1` is the wire value for that case.
            None if located.found => json!(-1),
            None => Value::Null,
        },
    );
    out.insert("paragraph_count".into(), json!(located.paragraph_count));
    if located.found {
        out.insert(
            "paragraph".into(),
            located.paragraph.map_or(Value::Null, Value::String),
        );
        out.insert(
            "paragraph_before".into(),
            located.before.map_or(Value::Null, Value::String),
        );
        out.insert(
            "paragraph_after".into(),
            located.after.map_or(Value::Null, Value::String),
        );
    }
    Value::Object(out)
}

/// `planned.map((p) => ({ annotation_id: p.annotation_id, old: p.old, new: p.new }))`.
///
/// An absent `annotation_id` stays absent (JavaScript `undefined` disappears
/// from the JSON), while an explicit `null` is kept.
fn applied(planned: &[PlannedEdit]) -> Vec<Value> {
    planned
        .iter()
        .map(|edit| {
            let mut map = Map::new();
            if let Some(annotation_id) = &edit.annotation_id {
                map.insert("annotation_id".into(), annotation_id.clone());
            }
            map.insert("old".into(), json!(edit.old));
            map.insert("new".into(), json!(edit.new));
            Value::Object(map)
        })
        .collect()
}

/// `body[key] ?? fallback`: only an absent key or an explicit `null` takes the
/// default; `0`, `''` and `false` are passed through.
fn or_default(input: &Value, key: &str, fallback: Value) -> Value {
    match input.get(key) {
        None | Some(Value::Null) => fallback,
        Some(value) => value.clone(),
    }
}

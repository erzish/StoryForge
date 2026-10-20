//! Temporal state routes: slices, facts, knowledge, dictionaries, diff, search.
//!
//! Ported from `server/index.js` (§2.7–2.9 of the analysis: R39–R50). Query
//! parsing reproduces the JavaScript helpers:
//!
//! ```js
//! const idList  = (s) => String(s ?? '').split(',').map(Number).filter(Boolean);
//! const strList = (s) => String(s ?? '').split(',').map(x => x.trim()).filter(Boolean);
//! ```
//!
//! so `idList` drops `0`/`NaN` and `strList` drops empty entries — both live in
//! [`storyforge_core::state`] and are unit-tested there.
//!
//! `full` is compared against the literal string `'1'`; `events` and
//! `minWeight` are `Number()`-coerced, so `?events=0` is meaningful (no events).

use std::collections::HashMap;
use std::sync::Arc;

use axum::Json;
use axum::body::Bytes;
use axum::extract::{Path, Query, State};
use axum::routing::{get, patch, post};
use serde_json::{Map, Value, json};

use storyforge_core::json_sql::{self, exec, num, query_one, truthy};
use storyforge_core::state as core_state;

use super::{ApiError, ApiResult, AppRouter, parse_body, path_id, query_num};
use crate::server::AppState;

/// `Number(query.x)` when the key is present at all (an empty value is `0`).
fn number_param(query: &HashMap<String, String>, key: &str) -> Option<f64> {
    query.get(key).map(|raw| core_state::js_number(raw))
}

/// `query.x === '1'`.
fn full_param(query: &HashMap<String, String>) -> bool {
    query.get("full").map(|raw| raw == "1").unwrap_or(false)
}

fn id_param(query: &HashMap<String, String>, key: &str) -> Option<i64> {
    query_num(query.get(key).map(String::as_str))
}

/// Request body as an object; anything else behaves like `{}`, which is what
/// property access on a JSON scalar did in JavaScript.
fn body_object(body: &Bytes) -> Result<Value, ApiError> {
    let parsed = parse_body(body)?;
    Ok(if parsed.is_object() {
        parsed
    } else {
        json!({})
    })
}

/// `{...fact, closed}` — the write helper returns `{fact, closed}` because the
/// MCP tools want them apart, while the HTTP route spreads the fact.
fn spread_row(written: &Value, row_key: &str) -> Value {
    let mut row = match written.get(row_key) {
        Some(Value::Object(object)) => object.clone(),
        _ => Map::new(),
    };
    row.insert(
        "closed".to_string(),
        written
            .get("closed")
            .cloned()
            .unwrap_or_else(|| Value::Array(Vec::new())),
    );
    Value::Object(row)
}

pub fn routes() -> AppRouter {
    axum::Router::new()
        .route("/api/books/{id}/state", get(book_state))
        .route("/api/characters/{id}/state", get(character_state))
        .route("/api/books/{id}/facts", get(list_facts).post(create_fact))
        .route("/api/books/{id}/facts/search", get(search_facts))
        .route("/api/facts/{id}", patch(update_fact).delete(delete_fact))
        .route("/api/facts/{id}/close", post(close_fact))
        .route(
            "/api/books/{id}/knowledge",
            get(list_knowledge).post(create_knowledge),
        )
        .route("/api/dictionaries", get(dictionaries))
        .route("/api/books/{id}/diff", get(diff))
}

// ─────────────────────────── slices ───────────────────────────

async fn book_state(
    State(state): State<Arc<AppState>>,
    Path(book): Path<String>,
    Query(query): Query<HashMap<String, String>>,
) -> ApiResult {
    let book_id = path_id(&book)?;
    let options = core_state::WorldStateOpts {
        characters: core_state::parse_id_list(query.get("characters").map(String::as_str)),
        // The route always passes a list, so an absent filter prints as `[]`
        // rather than `'все'` — same as the JavaScript route did.
        kinds: Some(core_state::parse_str_list(query.get("kinds").map(String::as_str))),
        events: number_param(&query, "events"),
        min_weight: number_param(&query, "minWeight"),
        full: full_param(&query),
    };
    let moment = id_param(&query, "moment");
    let chapter = id_param(&query, "chapter");

    let slice = state.db.with_conn(|conn| -> Result<Value, ApiError> {
        let at = core_state::resolve_moment(conn, book_id, moment, chapter)?;
        Ok(core_state::world_state(conn, book_id, &at, &options)?)
    })?;
    Ok(Json(slice))
}

async fn character_state(
    State(state): State<Arc<AppState>>,
    Path(character): Path<String>,
    Query(query): Query<HashMap<String, String>>,
) -> ApiResult {
    let character_id = path_id(&character)?;
    let options = core_state::CharacterStateOpts {
        full: full_param(&query),
        events: number_param(&query, "events"),
    };
    let moment = id_param(&query, "moment");
    let chapter = id_param(&query, "chapter");

    let slice = state.db.with_conn(|conn| -> Result<Value, ApiError> {
        let Some(row) = query_one(
            conn,
            "SELECT book_id FROM character WHERE id=?",
            &[Value::from(character_id)],
        )?
        else {
            return Err(ApiError::not_found("Персонаж не найден"));
        };
        let book_id = row.get("book_id").and_then(num).unwrap_or(0);
        let at = core_state::resolve_moment(conn, book_id, moment, chapter)?;
        match core_state::character_state(conn, character_id, &at, &options)? {
            Some(slice) => Ok(slice),
            None => Err(ApiError::not_found("Персонаж не найден")),
        }
    })?;
    Ok(Json(slice))
}

// ─────────────────────────── facts ───────────────────────────

async fn list_facts(
    State(state): State<Arc<AppState>>,
    Path(book): Path<String>,
    Query(query): Query<HashMap<String, String>>,
) -> ApiResult {
    let book_id = path_id(&book)?;

    let rows = state.db.with_conn(|conn| -> Result<Vec<Value>, ApiError> {
        let mut where_parts: Vec<String> = vec!["f.book_id = ?".to_string()];
        let mut params: Vec<Value> = vec![Value::from(book_id)];

        if let Some(subject_type) = query.get("subject_type").filter(|value| !value.is_empty()) {
            where_parts.push("f.subject_type = ?".to_string());
            params.push(Value::from(subject_type.as_str()));
        }
        if let Some(subject_id) = query.get("subject_id").filter(|value| !value.is_empty()) {
            where_parts.push("f.subject_id = ?".to_string());
            // A non-numeric value became `NaN` and matched nothing; `NULL`
            // reproduces that (SQL comparisons with NULL are never true).
            params.push(core_state::number_value(core_state::js_number(subject_id)));
        }
        if let Some(kind) = query.get("kind").filter(|value| !value.is_empty()) {
            where_parts.push("f.kind = ?".to_string());
            params.push(Value::from(kind.as_str()));
        }

        // `m.seq AS from_seq` is selected after `f.*`, so it shadows the
        // denormalised column and the JSON shows the joined moment's sequence.
        let sql = format!(
            "SELECT f.*, m.label AS from_label, m.seq AS from_seq
             FROM fact f LEFT JOIN moment m ON m.id = f.from_moment
             WHERE {} ORDER BY COALESCE(m.seq, 0), f.id",
            where_parts.join(" AND ")
        );
        Ok(json_sql::query(conn, &sql, &params)?)
    })?;
    Ok(Json(Value::Array(rows)))
}

async fn create_fact(
    State(state): State<Arc<AppState>>,
    Path(book): Path<String>,
    body: Bytes,
) -> ApiResult {
    let book_id = path_id(&book)?;
    let input = body_object(&body)?;
    let created = state.db.with_conn(|conn| -> Result<Value, ApiError> {
        Ok(core_state::add_fact(conn, book_id, &input)?)
    })?;
    Ok(Json(spread_row(&created, "fact")))
}

async fn update_fact(
    State(state): State<Arc<AppState>>,
    Path(fact): Path<String>,
    body: Bytes,
) -> ApiResult {
    let fact_id = path_id(&fact)?;
    let input = body_object(&body)?;
    let row = state.db.with_conn(|conn| -> Result<Value, ApiError> {
        core_state::patch_row(conn, "fact", fact_id, core_state::EDITABLE_FACT, &input)?;
        Ok(query_one(conn, "SELECT * FROM fact WHERE id=?", &[Value::from(fact_id)])?
            .unwrap_or(Value::Null))
    })?;
    Ok(Json(row))
}

async fn delete_fact(
    State(state): State<Arc<AppState>>,
    Path(fact): Path<String>,
) -> ApiResult {
    let fact_id = path_id(&fact)?;
    let changed = state.db.with_conn(|conn| -> Result<usize, ApiError> {
        Ok(exec(
            conn,
            "DELETE FROM fact WHERE id=?",
            &[Value::from(fact_id)],
        )?)
    })?;
    Ok(Json(json!({ "changed": Value::from(changed as i64) })))
}

async fn close_fact(
    State(state): State<Arc<AppState>>,
    Path(fact): Path<String>,
    body: Bytes,
) -> ApiResult {
    let fact_id = path_id(&fact)?;
    let input = body_object(&body)?;

    let row = state.db.with_conn(|conn| -> Result<Value, ApiError> {
        let Some(existing) = query_one(
            conn,
            "SELECT * FROM fact WHERE id=?",
            &[Value::from(fact_id)],
        )?
        else {
            return Err(ApiError::not_found("Факт не найден"));
        };

        // `body.chapter ? Number(body.chapter) : null` — chapter 0 is "no
        // chapter", exactly like the falsy check in the original.
        let chapter = match input.get("chapter") {
            Some(value) if truthy(value) => num(value),
            _ => None,
        };
        // `body.moment ?? momentOfChapter(book_id, chapter)`; an explicit but
        // unusable moment closes the interval with NULL rather than silently
        // anchoring to a chapter.
        let to_moment = match input.get("moment") {
            Some(value) if !value.is_null() => num(value),
            _ => {
                let book_id = existing.get("book_id").and_then(num).unwrap_or(0);
                core_state::moment_of_chapter(conn, book_id, chapter)?
            }
        };

        exec(
            conn,
            "UPDATE fact SET to_moment=?, to_chapter=? WHERE id=?",
            &[
                to_moment.map(Value::from).unwrap_or(Value::Null),
                chapter.map(Value::from).unwrap_or(Value::Null),
                Value::from(fact_id),
            ],
        )?;

        Ok(query_one(conn, "SELECT * FROM fact WHERE id=?", &[Value::from(fact_id)])?
            .unwrap_or(Value::Null))
    })?;
    Ok(Json(row))
}

async fn search_facts(
    State(state): State<Arc<AppState>>,
    Path(book): Path<String>,
    Query(query): Query<HashMap<String, String>>,
) -> ApiResult {
    let book_id = path_id(&book)?;
    let characters = core_state::parse_id_list(query.get("characters").map(String::as_str));
    let kinds = core_state::parse_str_list(query.get("kinds").map(String::as_str));
    let q = query.get("q").map(String::as_str);
    // `query.limit ? Number(query.limit) : 50` — `"0"` is truthy, `""` is not.
    let limit = match query.get("limit") {
        Some(raw) if !raw.is_empty() => core_state::js_number(raw),
        _ => core_state::SEARCH_LIMIT,
    };
    // An `at` bound exists only when a chapter or a moment was asked for.
    let anchored = query.get("chapter").map(|value| !value.is_empty()).unwrap_or(false)
        || query.get("moment").map(|value| !value.is_empty()).unwrap_or(false);
    let moment = id_param(&query, "moment");
    let chapter = id_param(&query, "chapter");

    let rows = state.db.with_conn(|conn| -> Result<Vec<Value>, ApiError> {
        let at = if anchored {
            Some(core_state::seq_of(&core_state::resolve_moment(
                conn, book_id, moment, chapter,
            )?))
        } else {
            None
        };
        Ok(core_state::search_facts(
            conn,
            book_id,
            q,
            at,
            &characters,
            Some(&kinds),
            limit,
        )?)
    })?;
    Ok(Json(Value::Array(rows)))
}

// ─────────────────────────── knowledge ───────────────────────────

async fn list_knowledge(
    State(state): State<Arc<AppState>>,
    Path(book): Path<String>,
) -> ApiResult {
    let book_id = path_id(&book)?;
    let rows = state.db.with_conn(|conn| -> Result<Vec<Value>, ApiError> {
        Ok(json_sql::query(
            conn,
            "SELECT k.*, c.name AS who, s.title AS secret_title,
                    m.label AS from_label, m.seq AS from_seq
             FROM knowledge k
             LEFT JOIN character c ON c.id = k.character_id
             LEFT JOIN secret s ON s.id = k.secret_id
             LEFT JOIN moment m ON m.id = k.from_moment
             WHERE k.book_id=? ORDER BY COALESCE(m.seq,0), c.name",
            &[Value::from(book_id)],
        )?)
    })?;
    Ok(Json(Value::Array(rows)))
}

async fn create_knowledge(
    State(state): State<Arc<AppState>>,
    Path(book): Path<String>,
    body: Bytes,
) -> ApiResult {
    let book_id = path_id(&book)?;
    let input = body_object(&body)?;
    let created = state.db.with_conn(|conn| -> Result<Value, ApiError> {
        Ok(core_state::add_knowledge(conn, book_id, &input)?)
    })?;
    Ok(Json(spread_row(&created, "knowledge")))
}

// ─────────────────────────── dictionaries and diff ───────────────────────────

async fn dictionaries(State(state): State<Arc<AppState>>) -> ApiResult {
    let payload = state.db.with_conn(|conn| -> Result<Value, ApiError> {
        Ok(json!({
            "tree": core_state::kind_tree(conn)?,
            "fact_kinds": json_sql::query(conn, "SELECT * FROM fact_kind ORDER BY ord", &[])?,
            "stances": json_sql::query(conn, "SELECT * FROM knowledge_stance ORDER BY ord", &[])?,
        }))
    })?;
    Ok(Json(payload))
}

async fn diff(
    State(state): State<Arc<AppState>>,
    Path(book): Path<String>,
    Query(query): Query<HashMap<String, String>>,
) -> ApiResult {
    let book_id = path_id(&book)?;
    let from_moment = id_param(&query, "from_moment");
    let from_chapter = id_param(&query, "from");
    let to_moment = id_param(&query, "to_moment");
    let to_chapter = id_param(&query, "to");

    let payload = state.db.with_conn(|conn| -> Result<Value, ApiError> {
        let from = core_state::resolve_moment(conn, book_id, from_moment, from_chapter)?;
        let to = core_state::resolve_moment(conn, book_id, to_moment, to_chapter)?;
        Ok(core_state::diff(conn, book_id, &from, &to)?)
    })?;
    Ok(Json(payload))
}

//! Characters, relationships, events, timeline, promises, secrets, world, moments.
//!
//! Ports R18–R20, R27–R31 and R38 of `server/index.js`. The SQL is kept verbatim —
//! same joins, `ORDER BY`, `COALESCE` and `GROUP_CONCAT` — because the frontend
//! reads the resulting rows, not only the fields it asked for.
//!
//! R43 (`/api/books/{id}/diff`) is not registered here: it is a `lib/state.js`
//! query, `storyforge_core::state` owns `resolve_moment`/`diff`, and
//! [`super::state`] already serves the path. Registering a second handler for the
//! same path would make axum panic at startup.

use std::sync::Arc;

use axum::Json;
use axum::extract::{Path, State};
use axum::routing::get;
use serde_json::{Value, json};

use storyforge_core::json_sql::{query, query_one};

use super::{ApiError, ApiResult, AppRouter, path_id};
use crate::server::AppState;

pub fn routes() -> AppRouter {
    axum::Router::new()
        .route("/api/books/{id}/characters", get(list_characters))
        .route("/api/characters/{id}", get(get_character))
        .route("/api/books/{id}/relationships", get(list_relationships))
        .route("/api/books/{id}/events", get(list_events))
        .route("/api/books/{id}/timeline", get(list_timeline))
        .route("/api/books/{id}/promises", get(list_promises))
        .route("/api/books/{id}/secrets", get(list_secrets))
        .route("/api/books/{id}/world", get(get_world))
        .route("/api/books/{id}/moments", get(list_moments))
}

/// R18 — character list plus per-row state/scene counters.
async fn list_characters(State(state): State<Arc<AppState>>, Path(id): Path<String>) -> ApiResult {
    let book_id = path_id(&id)?;
    let rows = state.db.with_conn(|conn| {
        query(
            conn,
            "SELECT c.*,
               (SELECT COUNT(*) FROM character_state s WHERE s.character_id=c.id) AS state_count,
               (SELECT COUNT(*) FROM event_character ec WHERE ec.character_id=c.id) AS scene_count
             FROM character c WHERE c.book_id=? ORDER BY c.is_main DESC, c.name",
            &[json!(book_id)],
        )
    })?;
    Ok(Json(Value::Array(rows)))
}

/// R19 — dossier: the row plus its states, outgoing and incoming relationships,
/// event appearances and secret knowledge.
async fn get_character(State(state): State<Arc<AppState>>, Path(id): Path<String>) -> ApiResult {
    let character_id = path_id(&id)?;
    state.db.with_conn(|conn| -> Result<Json<Value>, ApiError> {
        let Some(mut character) =
            query_one(conn, "SELECT * FROM character WHERE id=?", &[json!(character_id)])?
        else {
            return Err(ApiError::not_found("Персонаж не найден"));
        };

        let states = query(
            conn,
            "SELECT * FROM character_state WHERE character_id=? ORDER BY COALESCE(chapter_no, -1), ord",
            &[json!(character_id)],
        )?;

        // Outgoing edges keep label-only targets through COALESCE; `to_id` is NULL
        // for them because the LEFT JOIN finds no character row.
        let relationships = query(
            conn,
            "SELECT r.*, COALESCE(t.name, r.to_label) AS to_name, t.id AS to_id
             FROM relationship r LEFT JOIN character t ON t.id=r.to_char
             WHERE r.from_char=?",
            &[json!(character_id)],
        )?;

        // Incoming edges join `to_char`, not the label, so a label-only target
        // never appears here — deliberate, see the R19 notes.
        let incoming = query(
            conn,
            "SELECT r.*, f.name AS from_name, f.id AS from_id
             FROM relationship r JOIN character f ON f.id=r.from_char
             WHERE r.to_char=?",
            &[json!(character_id)],
        )?;

        let events = query(
            conn,
            "SELECT e.id, e.title, e.summary, e.hidden, ec.state_snapshot, ch.number AS chapter_no
             FROM event_character ec
             JOIN event e ON e.id=ec.event_id
             LEFT JOIN chapter ch ON ch.id=e.chapter_id
             WHERE ec.character_id=? ORDER BY ch.number",
            &[json!(character_id)],
        )?;

        let secrets = query(
            conn,
            "SELECT s.id, s.title, k.stance FROM secret_knowledge k
             JOIN secret s ON s.id=k.secret_id WHERE k.character_id=?",
            &[json!(character_id)],
        )?;

        if let Some(map) = character.as_object_mut() {
            map.insert("states".into(), Value::Array(states));
            map.insert("relationships".into(), Value::Array(relationships));
            map.insert("incoming".into(), Value::Array(incoming));
            map.insert("events".into(), Value::Array(events));
            map.insert("secrets".into(), Value::Array(secrets));
        }
        Ok(Json(character))
    })
}

/// R20 — flat directed edge list ordered by source name.
async fn list_relationships(
    State(state): State<Arc<AppState>>,
    Path(id): Path<String>,
) -> ApiResult {
    let book_id = path_id(&id)?;
    let rows = state.db.with_conn(|conn| {
        query(
            conn,
            "SELECT r.id, r.kind, r.current_state, r.history,
                    f.id AS from_id, f.name AS from_name,
                    t.id AS to_id, COALESCE(t.name, r.to_label) AS to_name
             FROM relationship r
             JOIN character f ON f.id=r.from_char
             LEFT JOIN character t ON t.id=r.to_char
             WHERE r.book_id=? ORDER BY f.name",
            &[json!(book_id)],
        )
    })?;
    Ok(Json(Value::Array(rows)))
}

/// R27 — event list with chapter number and comma-joined participant names.
async fn list_events(State(state): State<Arc<AppState>>, Path(id): Path<String>) -> ApiResult {
    let book_id = path_id(&id)?;
    let rows = state.db.with_conn(|conn| {
        query(
            conn,
            "SELECT e.*, ch.number AS chapter_no,
               (SELECT GROUP_CONCAT(c.name, ', ') FROM event_character ec
                JOIN character c ON c.id=ec.character_id WHERE ec.event_id=e.id) AS participants
             FROM event e LEFT JOIN chapter ch ON ch.id=e.chapter_id
             WHERE e.book_id=? ORDER BY ch.number, e.id",
            &[json!(book_id)],
        )
    })?;
    Ok(Json(Value::Array(rows)))
}

/// R28 — raw chronology rows ordered by `ord`.
async fn list_timeline(State(state): State<Arc<AppState>>, Path(id): Path<String>) -> ApiResult {
    let book_id = path_id(&id)?;
    let rows = state.db.with_conn(|conn| {
        query(
            conn,
            "SELECT * FROM timeline_entry WHERE book_id=? ORDER BY ord",
            &[json!(book_id)],
        )
    })?;
    Ok(Json(Value::Array(rows)))
}

/// R29 — "Chekhov guns", ordered by id.
async fn list_promises(State(state): State<Arc<AppState>>, Path(id): Path<String>) -> ApiResult {
    let book_id = path_id(&id)?;
    let rows = state.db.with_conn(|conn| {
        query(
            conn,
            "SELECT * FROM promise WHERE book_id=? ORDER BY id",
            &[json!(book_id)],
        )
    })?;
    Ok(Json(Value::Array(rows)))
}

/// R30 — secrets, each with a `knowledge` array of who holds which stance.
///
/// The JavaScript mutated the row objects after the main query; the array's keys
/// (`who`, `stance`, `character_id`) are the subquery's column order.
async fn list_secrets(State(state): State<Arc<AppState>>, Path(id): Path<String>) -> ApiResult {
    let book_id = path_id(&id)?;
    state.db.with_conn(|conn| -> Result<Json<Value>, ApiError> {
        let mut secrets = query(
            conn,
            "SELECT * FROM secret WHERE book_id=? ORDER BY id",
            &[json!(book_id)],
        )?;
        for secret in &mut secrets {
            let secret_id = secret.get("id").cloned().unwrap_or(Value::Null);
            let knowledge = query(
                conn,
                "SELECT COALESCE(c.name, k.label) AS who, k.stance, k.character_id
                 FROM secret_knowledge k LEFT JOIN character c ON c.id=k.character_id
                 WHERE k.secret_id=?",
                &[secret_id],
            )?;
            if let Some(map) = secret.as_object_mut() {
                map.insert("knowledge".into(), Value::Array(knowledge));
            }
        }
        Ok(Json(Value::Array(secrets)))
    })
}

/// R31 — the world bible: rules, locations, glossary and hidden lines.
///
/// `hidden` is the bare `hidden_line` table; beats and hints are **not** included,
/// exactly like the original.
async fn get_world(State(state): State<Arc<AppState>>, Path(id): Path<String>) -> ApiResult {
    let book_id = path_id(&id)?;
    state.db.with_conn(|conn| -> Result<Json<Value>, ApiError> {
        let rules = query(
            conn,
            "SELECT * FROM world_rule WHERE book_id=? ORDER BY ord",
            &[json!(book_id)],
        )?;
        let locations = query(
            conn,
            "SELECT * FROM location WHERE book_id=?",
            &[json!(book_id)],
        )?;
        let glossary = query(
            conn,
            "SELECT * FROM glossary WHERE book_id=? ORDER BY section, term",
            &[json!(book_id)],
        )?;
        let hidden = query(
            conn,
            "SELECT * FROM hidden_line WHERE book_id=?",
            &[json!(book_id)],
        )?;
        Ok(Json(json!({
            "rules": rules,
            "locations": locations,
            "glossary": glossary,
            "hidden": hidden,
        })))
    })
}

/// R38 — the time axis with per-moment coverage.
///
/// `chapters` is `GROUP_CONCAT(ch.number)` with SQLite's default `,` separator (no
/// space) and no `ORDER BY`, so it lists chapter numbers in table order. A chapter
/// whose `from_moment`/`to_moment` is NULL compares as NULL and is omitted.
async fn list_moments(State(state): State<Arc<AppState>>, Path(id): Path<String>) -> ApiResult {
    let book_id = path_id(&id)?;
    let rows = state.db.with_conn(|conn| {
        query(
            conn,
            "SELECT m.*,
               (SELECT COUNT(*) FROM timeline_entry t WHERE t.moment_id = m.id) AS entries,
               (SELECT GROUP_CONCAT(ch.number) FROM chapter ch
                 WHERE ch.book_id = m.book_id
                   AND (SELECT seq FROM moment WHERE id = ch.from_moment) <= m.seq
                   AND (SELECT seq FROM moment WHERE id = ch.to_moment) >= m.seq) AS chapters
             FROM moment m WHERE m.book_id=? ORDER BY m.seq",
            &[json!(book_id)],
        )
    })?;
    Ok(Json(Value::Array(rows)))
}

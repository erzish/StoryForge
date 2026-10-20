//! Graph canvas: lazy seeding plus node/edge CRUD (R67–R73).
//!
//! Port of the `/api/…/graph` block in `server/index.js` and of
//! `server/lib/graph.js`. The deliberate hazard of the JavaScript edition is
//! kept: `GET /api/books/:id/graph` **writes**. An empty canvas is filled with
//! one node per chapter and per event on the first read, so that opening a
//! freshly imported book shows its structure instead of a blank page (see
//! `docs/analysis/server-api.md` §5.8 and hazard H7 for the `withEntity`
//! table-name interpolation this port narrows to a whitelist).
//!
//! `rusqlite` is not a direct dependency of this crate, so `Connection` cannot
//! appear in a helper signature. The helpers that need a connection are
//! closures whose parameter type is inferred from [`Db::with_conn`], and the
//! whole "seed → optional insert → read" sequence runs inside one locked
//! section. That matches the JavaScript handler, which was synchronous end to
//! end, so no other request can observe a half-seeded canvas.

use std::collections::HashMap;
use std::sync::Arc;

use axum::Json;
use axum::body::Bytes;
use axum::extract::{Path, Query, State};
use axum::routing::{get, patch, post};
use serde_json::{Value, json};

use storyforge_core::json_sql::{exec, insert, num, query, query_one, scalar_i64, scalar_opt_i64};

use super::{ApiError, ApiResult, AppRouter, parse_body, path_id};
use crate::server::AppState;

/// Horizontal distance between chapter columns (`COL_W` in `lib/graph.js`).
const COL_W: f64 = 320.0;
/// Vertical distance between the events of one chapter (`ROW_H`).
const ROW_H: f64 = 130.0;
/// Top margin of the chapter row (`TOP`).
const TOP: f64 = 60.0;
/// Gap between the chapter row and the first event row (`TOP + 150`).
const EVENT_TOP: f64 = 150.0;

/// Entities `DELETE /api/nodes/:id?withEntity=1` is allowed to remove.
///
/// The JavaScript edition interpolated `node.ref_table` — a user-writable
/// column — straight into `DELETE FROM <table>`, which was an identifier
/// injection surface. A table name outside this list is silently ignored: the
/// node is still deleted, the entity is left alone.
const ENTITY_TABLES: [&str; 6] = [
    "chapter",
    "event",
    "character",
    "secret",
    "promise",
    "location",
];

/// `EDITABLE.node` from `server/lib/crud.js`.
const NODE_EDITABLE: [&str; 9] = [
    "x",
    "y",
    "w",
    "h",
    "data",
    "collapsed",
    "type",
    "ref_table",
    "ref_id",
];

/// Edge fields the entity layer treats as editable.
///
/// The JavaScript HTTP layer exposed no `PATCH /api/edges/:id`; the route is
/// implemented here because the port brief asks for it, with the same
/// "update the whitelisted fields, then return the row or `null`" semantics as
/// `PATCH /api/nodes/:id`.
const EDGE_EDITABLE: [&str; 6] = [
    "from_node",
    "from_port",
    "to_node",
    "to_port",
    "kind",
    "label",
];

pub fn routes() -> AppRouter {
    axum::Router::new()
        .route("/api/books/{id}/graph", get(get_graph))
        .route("/api/books/{id}/nodes", post(create_node))
        .route("/api/books/{id}/edges", post(create_edge))
        .route("/api/nodes/move", post(move_nodes))
        .route("/api/nodes/{id}", patch(patch_node).delete(delete_node))
        .route("/api/edges/{id}", patch(patch_edge).delete(delete_edge))
}

/// Result of one canvas read: the graph itself plus, for a create request, the
/// freshly inserted node (raw row, before `entity` and `data` decoration).
struct Canvas {
    graph: Value,
    created: Option<(i64, Value)>,
}

// ─────────────────────────── canvas (GET + node create) ───────────────────────────

async fn get_graph(State(state): State<Arc<AppState>>, Path(id): Path<String>) -> ApiResult {
    let book_id = path_id(&id)?;
    // The JavaScript edition never checked that the book exists: an unknown
    // numeric id answered `200 {nodes: [], edges: []}`. Keep that.
    let canvas = load_canvas(&state, book_id, None)?;
    Ok(Json(canvas.graph))
}

async fn create_node(
    State(state): State<Arc<AppState>>,
    Path(id): Path<String>,
    body: Bytes,
) -> ApiResult {
    let book_id = path_id(&id)?;
    let body = parse_body(&body)?;
    let Canvas { graph, created } = load_canvas(&state, book_id, Some(&body))?;
    // `loadGraph(bookId).nodes.find(n => n.id === node.id) ?? node` — the raw
    // row is the fallback for the impossible case where the read misses it.
    let node = match created {
        Some((node_id, row)) => find_node(&graph, node_id).unwrap_or(row),
        None => Value::Null,
    };
    Ok(Json(node))
}

/// Seed the canvas when the book has none, optionally create one node with its
/// entity, then read the whole canvas back — all under a single connection lock.
fn load_canvas(state: &AppState, book_id: i64, create: Option<&Value>) -> Result<Canvas, ApiError> {
    state.db.with_conn(|conn| {
        // `ensureGraph` from `lib/graph.js`: a no-op as soon as the book has one
        // node, so a second read of a seeded book writes nothing.
        let seed = |conn| -> Result<(), ApiError> {
            let count = scalar_i64(
                conn,
                "SELECT COUNT(*) FROM node WHERE book_id=?",
                &[json!(book_id)],
            )?;
            if count > 0 {
                return Ok(());
            }

            let chapters = query(
                conn,
                "SELECT * FROM chapter WHERE book_id=? ORDER BY number",
                &[json!(book_id)],
            )?;

            let mut chapter_node: HashMap<i64, i64> = HashMap::new();
            let mut chapter_index: HashMap<i64, usize> = HashMap::new();
            for (index, chapter) in chapters.iter().enumerate() {
                let chapter_id = chapter.get("id").and_then(num).unwrap_or_default();
                let node_id = insert(
                    conn,
                    "INSERT INTO node (book_id, type, ref_table, ref_id, x, y) VALUES (?,?,?,?,?,?)",
                    &[
                        json!(book_id),
                        json!("chapter"),
                        json!("chapter"),
                        json!(chapter_id),
                        json!(index as f64 * COL_W),
                        json!(TOP),
                    ],
                )?;
                chapter_node.insert(chapter_id, node_id);
                chapter_index.insert(chapter_id, index);
            }

            // Chapter order is the "flow" of the book.
            for pair in chapters.windows(2) {
                let from = pair[0]
                    .get("id")
                    .and_then(num)
                    .and_then(|id| chapter_node.get(&id).copied());
                let to = pair[1]
                    .get("id")
                    .and_then(num)
                    .and_then(|id| chapter_node.get(&id).copied());
                if let (Some(from), Some(to)) = (from, to) {
                    insert(
                        conn,
                        "INSERT INTO edge (book_id, from_node, to_node, kind) VALUES (?,?,?,?)",
                        &[json!(book_id), json!(from), json!(to), json!("flow")],
                    )?;
                }
            }

            // Events sit under their chapter, stacked in import order. A chapter id
            // that is not part of this book (including `NULL`) maps to column 0.
            let events = query(
                conn,
                "SELECT * FROM event WHERE book_id=? ORDER BY chapter_id, id",
                &[json!(book_id)],
            )?;

            let mut event_node: HashMap<i64, i64> = HashMap::new();
            let mut per_chapter: HashMap<Option<i64>, i64> = HashMap::new();
            for event in &events {
                let event_id = event.get("id").and_then(num).unwrap_or_default();
                let chapter_id = event.get("chapter_id").and_then(num);
                let row = per_chapter.entry(chapter_id).or_insert(0);
                let offset = *row;
                *row += 1;

                let column = chapter_id
                    .and_then(|id| chapter_index.get(&id).copied())
                    .map_or(0.0, |index| index as f64);
                let x = column * COL_W + 20.0;
                let y = TOP + EVENT_TOP + offset as f64 * ROW_H;

                let node_id = insert(
                    conn,
                    "INSERT INTO node (book_id, type, ref_table, ref_id, x, y) VALUES (?,?,?,?,?,?)",
                    &[
                        json!(book_id),
                        json!("event"),
                        json!("event"),
                        json!(event_id),
                        json!(x),
                        json!(y),
                    ],
                )?;
                event_node.insert(event_id, node_id);
            }

            // Causal links between events are the "data" edges. The JavaScript
            // query only proves the *source* event belongs to this book; a target
            // event from another book simply has no node here and is skipped.
            let links = query(
                conn,
                "SELECT el.from_event, el.to_event FROM event_link el
                   JOIN event e ON e.id = el.from_event
                  WHERE e.book_id=?",
                &[json!(book_id)],
            )?;
            for link in links {
                let from = link
                    .get("from_event")
                    .and_then(num)
                    .and_then(|id| event_node.get(&id).copied());
                let to = link
                    .get("to_event")
                    .and_then(num)
                    .and_then(|id| event_node.get(&id).copied());
                if let (Some(from), Some(to)) = (from, to) {
                    insert(
                        conn,
                        "INSERT INTO edge (book_id, from_node, to_node, kind) VALUES (?,?,?,?)",
                        &[json!(book_id), json!(from), json!(to), json!("data")],
                    )?;
                }
            }

            Ok(())
        };

        // `loadGraph` from `lib/graph.js`: every node plus the live entity it
        // points at, and `data` parsed back from its stored JSON text.
        let read = |conn| -> Result<Value, ApiError> {
            let nodes = query(
                conn,
                "SELECT * FROM node WHERE book_id=?",
                &[json!(book_id)],
            )?;

            let chapters: HashMap<i64, Value> = query(
                conn,
                "SELECT * FROM chapter WHERE book_id=?",
                &[json!(book_id)],
            )?
            .into_iter()
            .filter_map(|row| row.get("id").and_then(num).map(|id| (id, row)))
            .collect();

            let events: HashMap<i64, Value> =
                query(conn, "SELECT * FROM event WHERE book_id=?", &[json!(book_id)])?
                    .into_iter()
                    .filter_map(|row| row.get("id").and_then(num).map(|id| (id, row)))
                    .collect();

            let mut participants: HashMap<i64, Vec<Value>> = HashMap::new();
            for row in query(
                conn,
                "SELECT ec.event_id, c.id, c.name FROM event_character ec
                   JOIN character c ON c.id = ec.character_id
                   JOIN event e ON e.id = ec.event_id
                  WHERE e.book_id=?",
                &[json!(book_id)],
            )? {
                let Some(event_id) = row.get("event_id").and_then(num) else {
                    continue;
                };
                participants.entry(event_id).or_default().push(json!({
                    "id": row.get("id").cloned().unwrap_or(Value::Null),
                    "name": row.get("name").cloned().unwrap_or(Value::Null),
                }));
            }

            let mut decorated = Vec::with_capacity(nodes.len());
            for mut node in nodes {
                match node.get("ref_table").and_then(Value::as_str) {
                    Some("chapter") => {
                        let entity = node
                            .get("ref_id")
                            .and_then(num)
                            .and_then(|id| chapters.get(&id).cloned())
                            .unwrap_or(Value::Null);
                        set_key(&mut node, "entity", entity);
                    }
                    Some("event") => {
                        let event_id = node.get("ref_id").and_then(num);
                        let mut entity = event_id
                            .and_then(|id| events.get(&id).cloned())
                            .unwrap_or(Value::Null);
                        if entity.is_object() {
                            let people = event_id
                                .and_then(|id| participants.get(&id).cloned())
                                .unwrap_or_default();
                            set_key(&mut entity, "participants", Value::Array(people));
                        }
                        set_key(&mut node, "entity", entity);
                    }
                    _ => {}
                }

                // Stored `data` is JSON text; a value that does not parse stays a
                // string (the JavaScript edition did the same).
                if let Some(raw) = node.get("data").and_then(Value::as_str) {
                    if !raw.is_empty() {
                        if let Ok(parsed) = serde_json::from_str::<Value>(raw) {
                            set_key(&mut node, "data", parsed);
                        }
                    }
                }

                decorated.push(node);
            }

            let edges = query(
                conn,
                "SELECT * FROM edge WHERE book_id=?",
                &[json!(book_id)],
            )?;

            Ok(json!({ "nodes": decorated, "edges": edges }))
        };

        // `POST /api/books/:id/nodes`: a chapter/event node without `ref_id`
        // creates the entity first, so the canvas never holds a dangling reference.
        let create_node = |conn, body: &Value| -> Result<(i64, Value), ApiError> {
            let node_type = match body.get("type") {
                Some(value) if js_truthy(value) => value.clone(),
                _ => json!("comment"),
            };
            let mut ref_table = body.get("ref_table").cloned().unwrap_or(Value::Null);
            let mut ref_id = body.get("ref_id").cloned().unwrap_or(Value::Null);

            if node_type.as_str() == Some("chapter") && !js_truthy(&ref_id) {
                let next = scalar_opt_i64(
                    conn,
                    "SELECT MAX(number) FROM chapter WHERE book_id=?",
                    &[json!(book_id)],
                )?
                .unwrap_or(0)
                    + 1;
                let chapter_id = insert(
                    conn,
                    "INSERT INTO chapter (book_id, number, title, status) VALUES (?,?,?,?)",
                    &[
                        json!(book_id),
                        json!(next),
                        json!(format!("Глава {next}")),
                        json!("план"),
                    ],
                )?;
                ref_table = json!("chapter");
                ref_id = json!(chapter_id);
            } else if node_type.as_str() == Some("event") && !js_truthy(&ref_id) {
                let chapter_id = body.get("chapter_id").cloned().unwrap_or(Value::Null);
                let event_id = insert(
                    conn,
                    "INSERT INTO event (book_id, chapter_id, title, status) VALUES (?,?,?,?)",
                    &[
                        json!(book_id),
                        chapter_id,
                        json!("Новое событие"),
                        json!("план"),
                    ],
                )?;
                ref_table = json!("event");
                ref_id = json!(event_id);
            }

            let data = match body.get("data") {
                Some(value) if js_truthy(value) => json!(serde_json::to_string(value)?),
                _ => Value::Null,
            };

            let node_id = insert(
                conn,
                "INSERT INTO node (book_id, type, ref_table, ref_id, x, y, data) VALUES (?,?,?,?,?,?,?)",
                &[
                    json!(book_id),
                    node_type,
                    ref_table,
                    ref_id,
                    or_zero(body, "x"),
                    or_zero(body, "y"),
                    data,
                ],
            )?;

            let row = query_one(conn, "SELECT * FROM node WHERE id=?", &[json!(node_id)])?
                .unwrap_or(Value::Null);
            Ok((node_id, row))
        };

        seed(conn)?;
        let created = match create {
            Some(body) => Some(create_node(conn, body)?),
            None => None,
        };
        let graph = read(conn)?;
        Ok(Canvas { graph, created })
    })
}

// ─────────────────────────── nodes: patch, move, delete ───────────────────────────

async fn patch_node(
    State(state): State<Arc<AppState>>,
    Path(id): Path<String>,
    body: Bytes,
) -> ApiResult {
    let node_id = path_id(&id)?;
    let body = parse_body(&body)?;
    let row = state.db.with_conn(|conn| -> Result<Value, ApiError> {
        let plan = patch_plan("node", node_id, &NODE_EDITABLE, &body, coerce_node_field)?;
        if let Some((sql, params)) = plan {
            exec(conn, &sql, &params)?;
        }
        // Missing row answers `null` with 200, like every other PATCH in the
        // JavaScript edition. `data` stays raw text here: this route returned
        // the unparsed row.
        Ok(
            query_one(conn, "SELECT * FROM node WHERE id=?", &[json!(node_id)])?
                .unwrap_or(Value::Null),
        )
    })?;
    Ok(Json(row))
}

/// Bulk move: one `UPDATE` per item, positions persisted verbatim (floats too).
async fn move_nodes(State(state): State<Arc<AppState>>, body: Bytes) -> ApiResult {
    let body = parse_body(&body)?;
    let items = body
        .get("items")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    let result = state.db.with_conn(|conn| -> Result<Value, ApiError> {
        let mut moved = 0i64;
        for item in &items {
            let x = item.get("x").cloned().unwrap_or(Value::Null);
            let y = item.get("y").cloned().unwrap_or(Value::Null);
            let id = item.get("id").cloned().unwrap_or(Value::Null);
            moved += exec(conn, "UPDATE node SET x=?, y=? WHERE id=?", &[x, y, id])? as i64;
        }
        Ok(json!({ "ok": true, "moved": moved }))
    })?;
    Ok(Json(result))
}

async fn delete_node(
    State(state): State<Arc<AppState>>,
    Path(id): Path<String>,
    Query(q): Query<HashMap<String, String>>,
) -> ApiResult {
    let node_id = path_id(&id)?;
    let with_entity = q.get("withEntity").map(String::as_str) == Some("1");
    let result = state.db.with_conn(|conn| -> Result<Value, ApiError> {
        let Some(node) = query_one(conn, "SELECT * FROM node WHERE id=?", &[json!(node_id)])?
        else {
            return Err(ApiError::not_found("Нода не найдена"));
        };

        // Canvas-only delete by default; `withEntity=1` also removes the
        // entity the node points at. The table name comes from stored data, so
        // only the whitelist is honoured (hazard H7 in the analysis).
        if with_entity {
            if let Some(table) = node.get("ref_table").and_then(Value::as_str) {
                let ref_id = node.get("ref_id").cloned().unwrap_or(Value::Null);
                if ENTITY_TABLES.contains(&table) && js_truthy(&ref_id) {
                    exec(conn, &format!("DELETE FROM {table} WHERE id=?"), &[ref_id])?;
                }
            }
        }

        let changed = exec(conn, "DELETE FROM node WHERE id=?", &[json!(node_id)])?;
        Ok(json!({ "changed": changed }))
    })?;
    Ok(Json(result))
}

// ─────────────────────────── edges ───────────────────────────

async fn create_edge(
    State(state): State<Arc<AppState>>,
    Path(id): Path<String>,
    body: Bytes,
) -> ApiResult {
    let book_id = path_id(&id)?;
    let body = parse_body(&body)?;
    let result = state.db.with_conn(|conn| -> Result<Value, ApiError> {
        let Some(from) = js_number(body.get("from_node")) else {
            return Err(ApiError::invalid("Нужны from_node и to_node"));
        };
        let Some(to) = js_number(body.get("to_node")) else {
            return Err(ApiError::invalid("Нужны from_node и to_node"));
        };
        // `!from` in the JavaScript edition: zero and NaN are rejected, and a
        // zero endpoint would be meaningless anyway.
        if from == 0.0 || to == 0.0 {
            return Err(ApiError::invalid("Нужны from_node и to_node"));
        }
        if from == to {
            return Err(ApiError::invalid("Нода не может ссылаться на себя"));
        }

        let from_value = number_param(from);
        let to_value = number_param(to);

        let duplicate = query_one(
            conn,
            "SELECT id FROM edge WHERE book_id=? AND from_node=? AND to_node=?",
            &[json!(book_id), from_value.clone(), to_value.clone()],
        )?;
        if duplicate.is_some() {
            return Err(ApiError::conflict("Такая связь уже есть"));
        }

        let kind = match body.get("kind") {
            Some(value) if js_truthy(value) => value.clone(),
            _ => json!("flow"),
        };
        let label = body.get("label").cloned().unwrap_or(Value::Null);

        let edge_id = insert(
            conn,
            "INSERT INTO edge (book_id, from_node, to_node, kind, label) VALUES (?,?,?,?,?)",
            &[
                json!(book_id),
                from_value.clone(),
                to_value.clone(),
                kind,
                label,
            ],
        )?;
        let edge = query_one(conn, "SELECT * FROM edge WHERE id=?", &[json!(edge_id)])?
            .unwrap_or(Value::Null);

        // An edge between two event nodes also mirrors the story-level causal
        // link. `OR IGNORE` keeps a duplicate causal link harmless.
        let from_node = query_one(
            conn,
            "SELECT * FROM node WHERE id=?",
            std::slice::from_ref(&from_value),
        )?;
        let to_node = query_one(
            conn,
            "SELECT * FROM node WHERE id=?",
            std::slice::from_ref(&to_value),
        )?;
        if let (Some(from_event), Some(to_event)) = (
            from_node.as_ref().and_then(event_ref),
            to_node.as_ref().and_then(event_ref),
        ) {
            exec(
                conn,
                "INSERT OR IGNORE INTO event_link (from_event, to_event) VALUES (?,?)",
                &[from_event, to_event],
            )?;
        }

        Ok(edge)
    })?;
    Ok(Json(result))
}

/// Not present in the JavaScript HTTP layer; see [`EDGE_EDITABLE`].
async fn patch_edge(
    State(state): State<Arc<AppState>>,
    Path(id): Path<String>,
    body: Bytes,
) -> ApiResult {
    let edge_id = path_id(&id)?;
    let body = parse_body(&body)?;
    let row = state.db.with_conn(|conn| -> Result<Value, ApiError> {
        let plan = patch_plan("edge", edge_id, &EDGE_EDITABLE, &body, |_, value| {
            Ok(value.clone())
        })?;
        if let Some((sql, params)) = plan {
            exec(conn, &sql, &params)?;
        }
        Ok(
            query_one(conn, "SELECT * FROM edge WHERE id=?", &[json!(edge_id)])?
                .unwrap_or(Value::Null),
        )
    })?;
    Ok(Json(row))
}

async fn delete_edge(State(state): State<Arc<AppState>>, Path(id): Path<String>) -> ApiResult {
    let edge_id = path_id(&id)?;
    let result = state.db.with_conn(|conn| -> Result<Value, ApiError> {
        // The causal mirror is removed before the edge itself, exactly like
        // the JavaScript route, and only when both endpoints are events.
        if let Some(edge) = query_one(conn, "SELECT * FROM edge WHERE id=?", &[json!(edge_id)])? {
            let from_node_id = edge.get("from_node").cloned().unwrap_or(Value::Null);
            let to_node_id = edge.get("to_node").cloned().unwrap_or(Value::Null);
            let from_node = query_one(
                conn,
                "SELECT * FROM node WHERE id=?",
                std::slice::from_ref(&from_node_id),
            )?;
            let to_node = query_one(
                conn,
                "SELECT * FROM node WHERE id=?",
                std::slice::from_ref(&to_node_id),
            )?;
            if let (Some(from_event), Some(to_event)) = (
                from_node.as_ref().and_then(event_ref),
                to_node.as_ref().and_then(event_ref),
            ) {
                exec(
                    conn,
                    "DELETE FROM event_link WHERE from_event=? AND to_event=?",
                    &[from_event, to_event],
                )?;
            }
        }

        let changed = exec(conn, "DELETE FROM edge WHERE id=?", &[json!(edge_id)])?;
        Ok(json!({ "changed": changed }))
    })?;
    Ok(Json(result))
}

// ─────────────────────────── helpers ───────────────────────────

/// Build the `UPDATE` for the whitelisted body fields that are present.
///
/// The JavaScript `crud.patch` skipped the statement entirely when no allowed
/// field was in the body, and only touched the fields that were sent; both
/// properties matter (`PATCH {"w": null}` clears the column). `None` means
/// "nothing to update".
fn patch_plan(
    table: &str,
    id: i64,
    allowed: &[&str],
    body: &Value,
    coerce: impl Fn(&str, &Value) -> Result<Value, ApiError>,
) -> Result<Option<(String, Vec<Value>)>, ApiError> {
    let Some(object) = body.as_object() else {
        return Ok(None);
    };

    let mut columns = Vec::new();
    let mut params = Vec::new();
    for key in allowed {
        let Some(value) = object.get(*key) else {
            continue;
        };
        columns.push(*key);
        params.push(coerce(key, value)?);
    }
    if columns.is_empty() {
        return Ok(None);
    }

    let assignments: Vec<String> = columns.iter().map(|key| format!("{key}=?")).collect();
    params.push(json!(id));
    Ok(Some((
        format!("UPDATE {table} SET {} WHERE id=?", assignments.join(", ")),
        params,
    )))
}

/// `PATCH /api/nodes/:id` stores objects as JSON text but leaves strings alone.
fn coerce_node_field(key: &str, value: &Value) -> Result<Value, ApiError> {
    if key == "data" && js_truthy(value) && !value.is_string() {
        return Ok(json!(serde_json::to_string(value)?));
    }
    Ok(value.clone())
}

/// `body?.x ?? 0` — an absent or explicit `null` field becomes zero.
fn or_zero(body: &Value, key: &str) -> Value {
    match body.get(key) {
        None | Some(Value::Null) => json!(0),
        Some(value) => value.clone(),
    }
}

/// JavaScript truthiness for the values that reach the graph routes.
///
/// Unlike [`storyforge_core::json_sql::truthy`], empty arrays and objects are
/// truthy here, the way they are in JavaScript.
fn js_truthy(value: &Value) -> bool {
    match value {
        Value::Null => false,
        Value::Bool(flag) => *flag,
        Value::Number(number) => number.as_f64().is_some_and(|n| n != 0.0 && !n.is_nan()),
        Value::String(text) => !text.is_empty(),
        Value::Array(_) | Value::Object(_) => true,
    }
}

/// `Number(value)` for a JSON body field: `None` stands in for `NaN`.
fn js_number(value: Option<&Value>) -> Option<f64> {
    match value? {
        Value::Number(number) => number.as_f64(),
        Value::String(text) => text.trim().parse().ok(),
        Value::Bool(true) => Some(1.0),
        Value::Bool(false) => Some(0.0),
        _ => None,
    }
}

/// Bind a parsed number the way SQLite expects it: integral values as INTEGER,
/// anything else as REAL, so `from_node`/`to_node` stay comparable to node ids.
fn number_param(number: f64) -> Value {
    if number.fract() == 0.0 && number.abs() <= 9_007_199_254_740_992.0 {
        json!(number as i64)
    } else {
        json!(number)
    }
}

/// The `ref_id` of a node that points at an event, if it does.
fn event_ref(node: &Value) -> Option<Value> {
    if node.get("ref_table").and_then(Value::as_str) == Some("event") {
        Some(node.get("ref_id").cloned().unwrap_or(Value::Null))
    } else {
        None
    }
}

/// `loadGraph(bookId).nodes.find(n => n.id === node.id)`.
fn find_node(graph: &Value, node_id: i64) -> Option<Value> {
    graph
        .get("nodes")?
        .as_array()?
        .iter()
        .find(|node| node.get("id").and_then(num) == Some(node_id))
        .cloned()
}

fn set_key(object: &mut Value, key: &str, value: Value) {
    if let Some(map) = object.as_object_mut() {
        map.insert(key.to_string(), value);
    }
}

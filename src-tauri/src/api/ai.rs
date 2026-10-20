//! AI settings, chat, history, activity, style samples, text search and the
//! five AI tools — agent, chapter tasks, fact extraction, fact commit and
//! edit suggestions (R11–R20, R37–R38).
//!
//! The model client itself lives in [`storyforge_core::ai`]: this module only
//! parses the request, reads the database, awaits the completion and writes the
//! `ai_run` row. The database lock is never held across `.await` — inputs are
//! read, the guard is dropped, and only the result is written back.
//!
//! Path ids are `Number(params.id)` in the old routes, i.e. `NaN` when the
//! segment is not numeric. `NaN` matched no row, so [`loose_id`] maps it to `0`
//! (ids start at 1): routes that used to answer `[]`/`{changed:0}` still do,
//! while routes that checked the row themselves answer their own 404 message.
//! Only the history route validated the id up front, so only it answers `400`.
//!
//! `/ai/agent`, `/ai/task`, `/ai/extract`, `/ai/commit-facts` and
//! `/ai/suggest-edits` are ports of `runAgent` (`agent.js`), the four chapter
//! tasks of `server/index.js` and `proposals.js` / `anchor.js`. Their prompts
//! live in [`storyforge_core::ai`]; the agent borrows the MCP tool surface
//! through [`storyforge_core::mcp::dispatch`] instead of a second registry, so
//! the in-app agent and an external MCP client call the same tools.

use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use axum::Json;
use axum::body::Bytes;
use axum::extract::{Path, Query, State};
use axum::routing::{delete, get, post};
use serde_json::{Map, Value, json};

use storyforge_core::SfError;
use storyforge_core::ai as core_ai;
use storyforge_core::anchor;
use storyforge_core::chapter_text;
use storyforge_core::json_sql::{self, exec, insert, query, query_one, truthy};
use storyforge_core::mcp;
use storyforge_core::search as core_search;
use storyforge_core::state as core_state;
use storyforge_core::Db;

use super::{ApiError, ApiResult, AppRouter, parse_body, query_str};
use crate::server::AppState;

impl From<core_ai::AiError> for ApiError {
    fn from(err: core_ai::AiError) -> Self {
        ApiError::new(err.status(), err.message())
    }
}

pub fn routes() -> AppRouter {
    axum::Router::new()
        .route("/api/ai/settings", get(get_settings).put(put_settings))
        .route("/api/ai/test", post(test_connection))
        .route("/api/books/{id}/ai/history", get(get_history))
        .route("/api/books/{id}/ai/chat", post(chat))
        .route("/api/books/{id}/activity", get(list_activity))
        .route(
            "/api/books/{id}/style-examples",
            get(list_style_examples).post(create_style_example),
        )
        .route("/api/style-examples/{id}", delete(delete_style_example))
        .route("/api/books/{id}/text-search", get(text_search))
        .route("/api/books/{id}/ai/agent", post(agent))
        .route("/api/chapters/{id}/ai/task", post(task))
        .route("/api/chapters/{id}/ai/extract", post(extract))
        .route("/api/chapters/{id}/ai/commit-facts", post(commit_facts))
        .route("/api/chapters/{id}/ai/suggest-edits", post(suggest_edits))
}

/// `Number(params.id)` as SQLite saw it: a non-numeric segment becomes `NaN`,
/// which matched no row — `0` matches no row either.
fn loose_id(raw: &str) -> i64 {
    loose_number(raw).unwrap_or(0)
}

fn loose_number(raw: &str) -> Option<i64> {
    let trimmed = raw.trim();
    let value = if trimmed.is_empty() {
        0.0
    } else {
        trimmed.parse::<f64>().ok()?
    };
    safe_int(value)
}

/// `Number.isSafeInteger` for an already-coerced `Number()`.
fn safe_int(number: f64) -> Option<i64> {
    if number.is_finite() && number.fract() == 0.0 && number.abs() <= 9_007_199_254_740_992.0 {
        Some(number as i64)
    } else {
        None
    }
}

/// `Number(value)` for a JSON body field, restricted to safe integers.
fn body_number(value: &Value) -> Option<i64> {
    match value {
        Value::Number(number) => number.as_f64().and_then(safe_int),
        Value::String(raw) => safe_int(core_state::js_number(raw)),
        Value::Bool(flag) => Some(i64::from(*flag)),
        _ => None,
    }
}

/// `String(value || '').trim()` for a body field.
fn trimmed_string(value: Option<&Value>) -> String {
    match value {
        Some(value) if truthy(value) => json_sql::text(value).trim().to_string(),
        _ => String::new(),
    }
}

// ─────────────────────────── settings ───────────────────────────

async fn get_settings() -> ApiResult {
    let config = core_ai::public_ai_config().map_err(|err| ApiError::new(500, err.message()))?;
    Ok(Json(config))
}

/// Every failure here is a `400`, like the old `try { saveAiConfig } catch`.
async fn put_settings(body: Bytes) -> ApiResult {
    let input = parse_body(&body)?;
    match core_ai::save_ai_config(&input) {
        Ok(config) => Ok(Json(config)),
        Err(err) => Err(ApiError::invalid(err.message())),
    }
}

/// `POST /api/ai/test`: one word back, `502` with the provider's message on
/// failure (a missing key included).
async fn test_connection() -> ApiResult {
    match core_ai::test_ai_connection().await {
        Ok(turn) => Ok(Json(json!({
            "ok": true,
            "model": turn.model,
            "answer": turn.text.chars().take(200).collect::<String>(),
        }))),
        Err(err) => Err(ApiError::new(502, err.message())),
    }
}

// ─────────────────────────── history ───────────────────────────

async fn get_history(
    State(state): State<Arc<AppState>>,
    Path(id): Path<String>,
    Query(params): Query<HashMap<String, String>>,
) -> ApiResult {
    let Some(book_id) = loose_number(&id) else {
        return Err(ApiError::invalid("Некорректная книга или глава"));
    };
    let chapter_id = match params.get("chapter_id") {
        None => None,
        Some(raw) if raw.is_empty() => None,
        Some(raw) => match loose_number(raw) {
            Some(chapter_id) => Some(chapter_id),
            None => return Err(ApiError::invalid("Некорректная книга или глава")),
        },
    };
    let history = state
        .db
        .with_conn(|conn| core_ai::load_chat_history(conn, book_id, chapter_id, 200))?;
    Ok(Json(history))
}

// ─────────────────────────── chat ───────────────────────────

async fn chat(State(state): State<Arc<AppState>>, Path(id): Path<String>, body: Bytes) -> ApiResult {
    let input = parse_body(&body)?;
    let book_id = loose_id(&id);

    let message = match input.get("message").and_then(Value::as_str) {
        Some(text) => text.trim().to_string(),
        None => String::new(),
    };
    let length = message.chars().count();
    if length == 0 || length > 12_000 {
        return Err(ApiError::invalid(
            "Сообщение должно содержать от 1 до 12000 символов",
        ));
    }

    let chapter_id = match input.get("chapter_id") {
        None | Some(Value::Null) => None,
        Some(value) => match body_number(value) {
            Some(chapter_id) => Some(chapter_id),
            None => return Err(ApiError::invalid("Некорректная глава")),
        },
    };

    let mut history: Vec<Value> = Vec::new();
    if let Some(Value::Array(items)) = input.get("history") {
        for item in items.iter().skip(items.len().saturating_sub(8)) {
            let role = item.get("role").and_then(Value::as_str).unwrap_or_default();
            let Some(content) = item.get("content").and_then(Value::as_str) else {
                return Err(ApiError::invalid("Некорректная история диалога"));
            };
            if !matches!(role, "user" | "assistant") || content.chars().count() > 120_000 {
                return Err(ApiError::invalid("Некорректная история диалога"));
            }
            // Only the tail is kept, as the old handler mutated `item.content`.
            let tail: String = {
                let chars: Vec<char> = content.chars().collect();
                let start = chars.len().saturating_sub(6_000);
                chars[start..].iter().collect()
            };
            history.push(json!({ "role": role, "content": tail }));
        }
    }

    // Context is read and the lock released before any network call.
    let context = state.db.with_conn(|conn| {
        core_ai::build_ai_context(conn, book_id, chapter_id, "")
    })?;

    let model = core_ai::read_ai_config()
        .map_err(|err| ApiError::new(err.status(), err.message()))?
        .model;

    let run_id = state.db.with_conn(|conn| {
        Ok::<i64, ApiError>(insert(
            conn,
            "INSERT INTO ai_run (book_id, chapter_id, kind, engine, model, prompt)
             VALUES (?,?,?,?,?,?)",
            &[
                json!(book_id),
                chapter_id.map(Value::from).unwrap_or(Value::Null),
                json!("chat"),
                json!("api"),
                json!(model),
                json!(message),
            ],
        )?)
    })?;

    let system = format!(
        "Ты — помощник автора Story Forge. Работай с контекстом книги ниже.\n\
         Отделяй установленный канон от идей и предположений. Не раскрывай скрытые линии в тексте сцены, \
         если план главы или автор этого не требует. Не утверждай, что изменил книгу: этот диалог ничего не сохраняет.\n\n{context}"
    );
    let mut messages: Vec<Value> = Vec::with_capacity(history.len() + 2);
    messages.push(json!({ "role": "system", "content": system }));
    messages.extend(history);
    messages.push(json!({ "role": "user", "content": message }));

    let options = core_ai::CompleteOptions {
        max_tokens: 6000,
        timeout_ms: 180_000,
        ..core_ai::CompleteOptions::default()
    };

    match core_ai::complete_ai(&messages, &options).await {
        Ok(turn) => {
            let answer = turn.text.clone();
            let tokens_in = turn.tokens_in.clone();
            let tokens_out = turn.tokens_out.clone();
            state.db.with_conn(|conn| {
                exec(
                    conn,
                    "UPDATE ai_run SET result=?, status='ok', tokens_in=?, tokens_out=?,
                       finished_at=datetime('now') WHERE id=?",
                    &[json!(answer), tokens_in, tokens_out, json!(run_id)],
                )?;
                Ok::<(), ApiError>(())
            })?;
            Ok(Json(turn.to_json()))
        }
        Err(err) => {
            // The failure is already redacted by the client; record it and
            // answer 502 exactly like the old handler.
            let message = err.message().to_string();
            let _ = state.db.with_conn(|conn| {
                exec(
                    conn,
                    "UPDATE ai_run SET status='error', error=?, finished_at=datetime('now') WHERE id=?",
                    &[json!(message), json!(run_id)],
                )?;
                Ok::<(), ApiError>(())
            });
            Err(ApiError::new(502, message))
        }
    }
}

// ─────────────────────────── activity ───────────────────────────

async fn list_activity(
    State(state): State<Arc<AppState>>,
    Path(id): Path<String>,
    Query(params): Query<HashMap<String, String>>,
) -> ApiResult {
    let book_id = loose_id(&id);
    let limit = match params.get("limit") {
        Some(raw) if !raw.is_empty() => core_state::js_number(raw) as i64,
        _ => 50,
    };
    let rows = state.db.with_conn(|conn| {
        let rows = query(
            conn,
            "SELECT * FROM mcp_activity WHERE book_id=? ORDER BY id DESC LIMIT ?",
            &[json!(book_id), json!(limit)],
        )?;
        let mut out = Vec::with_capacity(rows.len());
        for mut row in rows {
            let detail = match row.get("detail") {
                Some(Value::String(raw)) if !raw.is_empty() => {
                    serde_json::from_str::<Value>(raw)
                        .map_err(|err| ApiError::new(500, err.to_string()))?
                }
                _ => Value::Null,
            };
            if let Some(object) = row.as_object_mut() {
                object.insert("detail".to_string(), detail);
            }
            out.push(row);
        }
        Ok::<Vec<Value>, ApiError>(out)
    })?;
    Ok(Json(Value::Array(rows)))
}

// ─────────────────────────── style samples ───────────────────────────

async fn list_style_examples(
    State(state): State<Arc<AppState>>,
    Path(id): Path<String>,
) -> ApiResult {
    let book_id = loose_id(&id);
    let rows = state.db.with_conn(|conn| {
        query(
            conn,
            "SELECT * FROM style_example WHERE book_id=? ORDER BY id DESC",
            &[json!(book_id)],
        )
    })?;
    Ok(Json(Value::Array(rows)))
}

async fn create_style_example(
    State(state): State<Arc<AppState>>,
    Path(id): Path<String>,
    body: Bytes,
) -> ApiResult {
    let input = parse_body(&body)?;
    let book_id = loose_id(&id);
    let scene_type = trimmed_string(input.get("scene_type"));
    let text = trimmed_string(input.get("body"));

    let created = state.db.with_conn(|conn| {
        if query_one(conn, "SELECT id FROM book WHERE id=?", &[json!(book_id)])?.is_none() {
            return Err(ApiError::not_found("Книга не найдена"));
        }
        if scene_type.is_empty()
            || scene_type.chars().count() > 60
            || text.chars().count() < 40
            || text.chars().count() > 6000
        {
            return Err(ApiError::invalid(
                "Укажите тип сцены и образец от 40 до 6000 символов",
            ));
        }
        let source_chapter = input
            .get("source_chapter_id")
            .filter(|value| truthy(value))
            .and_then(json_sql::num)
            .filter(|value| *value != 0);
        if let Some(chapter_id) = source_chapter {
            if query_one(
                conn,
                "SELECT id FROM chapter WHERE id=? AND book_id=?",
                &[json!(chapter_id), json!(book_id)],
            )?
            .is_none()
            {
                return Err(ApiError::invalid("Глава-источник не найдена"));
            }
        }
        let title = match input.get("title") {
            Some(value) if truthy(value) => json_sql::text(value),
            _ => String::new(),
        };
        let title: String = title.chars().take(120).collect();
        let id = insert(
            conn,
            "INSERT INTO style_example (book_id,scene_type,title,body,source_chapter_id)
             VALUES (?,?,?,?,?)",
            &[
                json!(book_id),
                json!(scene_type),
                json!(title),
                json!(text),
                source_chapter.map(Value::from).unwrap_or(Value::Null),
            ],
        )?;
        let row = query_one(conn, "SELECT * FROM style_example WHERE id=?", &[json!(id)])?
            .unwrap_or(Value::Null);
        Ok::<Value, ApiError>(row)
    })?;
    Ok(Json(created))
}

async fn delete_style_example(
    State(state): State<Arc<AppState>>,
    Path(id): Path<String>,
) -> ApiResult {
    // `Number(params.id)`: a non-numeric id deletes nothing, so `changed` is 0.
    let id = loose_id(&id);
    let changed = state
        .db
        .with_conn(|conn| exec(conn, "DELETE FROM style_example WHERE id=?", &[json!(id)]))?;
    Ok(Json(json!({ "changed": changed })))
}

// ─────────────────────────── text search ───────────────────────────

async fn text_search(
    State(state): State<Arc<AppState>>,
    Path(id): Path<String>,
    Query(params): Query<HashMap<String, String>>,
) -> ApiResult {
    let book_id = loose_id(&id);
    let q = query_str(params.get("q"));
    let limit = params
        .get("limit")
        .map(|raw| core_state::js_number(raw));
    let result = state
        .db
        .with_conn(|conn| core_search::search_book_text(conn, book_id, &q, limit))?;
    Ok(Json(result))
}

// ─────────────────────────── agent ───────────────────────────

/// Tools the model never sees: listing books and importing them are author
/// actions, not model decisions (`HIDDEN_TOOLS` in `agent.js`).
const HIDDEN_TOOLS: [&str; 2] = ["list_books", "run_import"];

/// Tools that change the book. Only these produce an action entry, and only
/// these are offered when the author asked for a change but the model stalled.
const MUTATIONS: [&str; 9] = [
    "add_fact",
    "close_fact",
    "add_knowledge",
    "write_chapter_text",
    "add_promise",
    "update_promise",
    "create_entity",
    "update_entity",
    "delete_entity",
];

/// `runAgent(..., {maxRounds: 12})`.
const MAX_ROUNDS: i64 = 12;
/// `response.tool_calls.indexOf(call) >= 8`.
const MAX_CALLS_PER_ROUND: usize = 8;
/// A tool answer longer than this is cut before it is fed back to the model.
const MAX_TOOL_ANSWER: usize = 30_000;

const NO_ACTION: &str =
    "Агент не выполнил действие. Попробуйте указать конкретную главу или запись и повторить задание.";

/// A failed agent run: the route still answers `200` and reports the message
/// next to whatever the model already did.
struct AgentError {
    message: String,
    actions: Vec<Value>,
    steps: Vec<Value>,
}

impl AgentError {
    fn new(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
            actions: Vec::new(),
            steps: Vec::new(),
        }
    }
}

fn failure(message: &str, actions: &[Value], steps: &[Value]) -> AgentError {
    AgentError {
        message: message.to_string(),
        actions: actions.to_vec(),
        steps: steps.to_vec(),
    }
}

impl From<SfError> for AgentError {
    fn from(err: SfError) -> Self {
        AgentError::new(sf_message(&err))
    }
}

impl From<core_ai::AiError> for AgentError {
    fn from(err: core_ai::AiError) -> Self {
        AgentError::new(err.message())
    }
}

/// The bare Russian sentence of a core error, as the old routes shipped it.
fn sf_message(err: &SfError) -> String {
    match err {
        SfError::NotFound(text)
        | SfError::Invalid(text)
        | SfError::Conflict(text)
        | SfError::Config(text)
        | SfError::Other(text) => text.clone(),
        other => other.to_string(),
    }
}

/// `String(value)` with template-literal semantics — `undefined` and `null`
/// print as their names, scalars as their text.
fn js_scalar(value: Option<&Value>) -> String {
    match value {
        None => "undefined".to_string(),
        Some(Value::Null) => "null".to_string(),
        Some(Value::String(text)) => text.clone(),
        Some(other) => other.to_string(),
    }
}

/// `POST /api/books/:id/ai/agent`: one tool-calling conversation.
///
/// Like the JavaScript route, a failure after validation answers `200` with
/// `{error, actions, steps}` — the run row records it either way. Only the
/// request itself (`400`) and an unknown book (`404`) use an HTTP status.
async fn agent(
    State(state): State<Arc<AppState>>,
    Path(id): Path<String>,
    body: Bytes,
) -> ApiResult {
    let input = parse_body(&body)?;
    let message = input
        .get("message")
        .and_then(Value::as_str)
        .map(|text| text.trim().to_string())
        .unwrap_or_default();
    let chapter_value = input.get("chapter_id").filter(|value| !value.is_null());
    let chapter_id = chapter_value.and_then(body_number);
    let book_id = loose_number(&id);

    if book_id.is_none() || (chapter_value.is_some() && chapter_id.is_none()) {
        return Err(ApiError::invalid("Некорректное задание агенту"));
    }
    if message.is_empty() || message.chars().count() > 12_000 {
        return Err(ApiError::invalid("Некорректное задание агенту"));
    }

    let mut history: Vec<Value> = Vec::new();
    if let Some(Value::Array(items)) = input.get("history") {
        for item in items.iter().skip(items.len().saturating_sub(8)) {
            let role = item.get("role").and_then(Value::as_str).unwrap_or_default();
            let Some(content) = item.get("content").and_then(Value::as_str) else {
                return Err(ApiError::invalid("Некорректная история диалога"));
            };
            if !matches!(role, "user" | "assistant") || content.chars().count() > 120_000 {
                return Err(ApiError::invalid("Некорректная история диалога"));
            }
            let tail: String = {
                let chars: Vec<char> = content.chars().collect();
                let start = chars.len().saturating_sub(6_000);
                chars[start..].iter().collect()
            };
            history.push(json!({ "role": role, "content": tail }));
        }
    }

    let book_id = book_id.expect("checked above");
    if state
        .db
        .with_conn(|conn| query_one(conn, "SELECT id FROM book WHERE id=?", &[json!(book_id)]))?
        .is_none()
    {
        return Err(ApiError::not_found("Книга не найдена"));
    }

    let model = configured_model()?;
    let run_id = start_run(&state, book_id, chapter_id, "agent", &model, &message)?;

    match run_agent(&state.db, book_id, chapter_id, &message, &history).await {
        Ok(result) => {
            let text = result.get("text").cloned().unwrap_or(Value::Null);
            let actions = result
                .get("actions")
                .cloned()
                .unwrap_or_else(|| json!([]));
            let actions_json =
                serde_json::to_string(&actions).unwrap_or_else(|_| "[]".to_string());
            state.db.with_conn(|conn| -> Result<(), ApiError> {
                exec(
                    conn,
                    "UPDATE ai_run SET result=?, actions_json=?, status='ok',
                       finished_at=datetime('now') WHERE id=?",
                    &[text, json!(actions_json), json!(run_id)],
                )?;
                Ok(())
            })?;
            Ok(Json(result))
        }
        Err(err) => {
            let actions_json =
                serde_json::to_string(&err.actions).unwrap_or_else(|_| "[]".to_string());
            let _ = state.db.with_conn(|conn| -> Result<(), ApiError> {
                exec(
                    conn,
                    "UPDATE ai_run SET status='error', error=?, actions_json=?,
                       finished_at=datetime('now') WHERE id=?",
                    &[json!(err.message), json!(actions_json), json!(run_id)],
                )?;
                Ok(())
            });
            Ok(Json(json!({
                "error": err.message,
                "actions": err.actions,
                "steps": err.steps,
            })))
        }
    }
}

/// `runAgent`: up to [`MAX_ROUNDS`] model turns, each allowed
/// [`MAX_CALLS_PER_ROUND`] tool calls, against the MCP tool surface.
///
/// The database lock is only taken inside [`mcp::dispatch`], between awaits, so
/// no guard is ever held across a network call.
async fn run_agent(
    db: &Db,
    book_id: i64,
    chapter_id: Option<i64>,
    message: &str,
    history: &[Value],
) -> Result<Value, AgentError> {
    let target_chapter = infer_chapter_id(db, book_id, chapter_id, message)?;
    let context = db.with_conn(|conn| core_ai::build_ai_context(conn, book_id, target_chapter, ""))?;
    let (definitions, schemas) = tool_definitions(db);
    let wants_action = core_ai::wants_action(message);

    let mut messages: Vec<Value> = Vec::with_capacity(history.len() + 2);
    messages.push(json!({
        "role": "system",
        "content": core_ai::agent_system_prompt(book_id, &context),
    }));
    messages.extend(history.iter().cloned());
    messages.push(json!({ "role": "user", "content": message }));

    let mut actions: Vec<Value> = Vec::new();
    let mut steps: Vec<Value> = Vec::new();

    for round in 0..MAX_ROUNDS {
        // After six fruitless rounds, offer only the tools that can change the
        // book and demand one of them.
        let write_now = wants_action && actions.is_empty() && round >= 6;
        let offered: Vec<Value> = if write_now {
            definitions
                .iter()
                .filter(|definition| {
                    definition
                        .get("function")
                        .and_then(|function| function.get("name"))
                        .and_then(Value::as_str)
                        .is_some_and(|name| MUTATIONS.contains(&name))
                })
                .cloned()
                .collect()
        } else {
            definitions.clone()
        };
        let options = core_ai::CompleteOptions {
            timeout_ms: 180_000,
            max_tokens: 8_000,
            tools: Some(offered),
            tool_choice: if wants_action && (round == 0 || write_now) {
                "required".to_string()
            } else {
                "auto".to_string()
            },
        };
        let turn = match core_ai::complete_ai_turn(&messages, &options).await {
            Ok(turn) => turn,
            Err(err) => return Err(failure(err.message(), &actions, &steps)),
        };

        if turn.tool_calls.is_empty() {
            if wants_action && actions.is_empty() {
                return Err(failure(NO_ACTION, &actions, &steps));
            }
            let text = if turn.text.is_empty() {
                format!("Выполнено действий: {}", actions.len())
            } else {
                turn.text.clone()
            };
            return Ok(json!({
                "text": text,
                "actions": actions,
                "steps": steps,
                "model": turn.model,
                "finish_reason": turn.finish_reason,
            }));
        }

        let content = if turn.text.is_empty() {
            Value::Null
        } else {
            json!(turn.text)
        };
        messages.push(json!({
            "role": "assistant",
            "content": content,
            "tool_calls": turn.tool_calls.clone(),
        }));

        for (index, call) in turn.tool_calls.iter().enumerate() {
            let name_value = call.get("function").and_then(|function| function.get("name"));
            let name = name_value.and_then(Value::as_str).unwrap_or("");
            let step_name = if name.is_empty() { "(неизвестно)" } else { name };
            let raw_arguments = call
                .get("function")
                .and_then(|function| function.get("arguments"))
                .and_then(Value::as_str)
                .unwrap_or("");

            let mut used_args: Option<Value> = None;
            let outcome: Result<Value, String> = if index >= MAX_CALLS_PER_ROUND {
                Err("Слишком много вызовов инструментов за один шаг".to_string())
            } else if !schemas.contains_key(name) {
                Err(format!(
                    "Неизвестный инструмент {}",
                    js_scalar(name_value)
                ))
            } else {
                let schema = &schemas[name];
                match parse_tool_arguments(raw_arguments) {
                    Err(message) => Err(message),
                    Ok(args) => match validate_tool_args(schema, &args)
                        .and_then(|()| assert_tool_scope(db, book_id, name, &args, message))
                    {
                        Err(message) => Err(message),
                        Ok(()) => match call_tool(db, name, &args) {
                            Ok(value) => {
                                used_args = Some(args);
                                Ok(value)
                            }
                            Err(message) => Err(message),
                        },
                    },
                }
            };

            let output = match outcome {
                Ok(value) => {
                    let write = MUTATIONS.contains(&name);
                    steps.push(json!({ "tool": step_name, "ok": true, "write": write }));
                    if write {
                        if let Some(args) = &used_args {
                            actions.push(json!({
                                "tool": name,
                                "label": action_label(db, name, args, &value),
                                "entity_type": action_entity_type(name, args),
                                "id": action_id(args, &value),
                            }));
                        }
                    }
                    value
                }
                Err(message) => {
                    steps.push(json!({ "tool": step_name, "ok": false, "error": message }));
                    json!({ "error": message })
                }
            };

            let mut serialized =
                serde_json::to_string(&output).unwrap_or_else(|_| "null".to_string());
            if serialized.chars().count() > MAX_TOOL_ANSWER {
                serialized = serialized.chars().take(MAX_TOOL_ANSWER).collect::<String>()
                    + "\n[ответ сокращён]";
            }
            let mut tool_message = Map::new();
            tool_message.insert("role".to_string(), json!("tool"));
            if let Some(id) = call.get("id") {
                tool_message.insert("tool_call_id".to_string(), id.clone());
            }
            tool_message.insert("content".to_string(), json!(serialized));
            messages.push(Value::Object(tool_message));
        }
    }

    Err(failure(
        &format!(
            "Агент остановлен после {MAX_ROUNDS} шагов. Выполнено действий: {}.",
            actions.len()
        ),
        &actions,
        &steps,
    ))
}

/// Turn a chapter mentioned in the message (or an explicit id) into a chapter
/// of this book, or `None` when the message names something that is not there.
fn infer_chapter_id(
    db: &Db,
    book_id: i64,
    chapter_id: Option<i64>,
    message: &str,
) -> Result<Option<i64>, AgentError> {
    if let Some(number) = core_ai::chapter_number(message) {
        if number > 0 {
            let named = db.with_conn(|conn| {
                query_one(
                    conn,
                    "SELECT id FROM chapter WHERE book_id=? AND number=?",
                    &[json!(book_id), json!(number)],
                )
            })?;
            return Ok(named.and_then(|row| row.get("id").and_then(json_sql::num)));
        }
    }
    if let Some(chapter_id) = chapter_id {
        let chapter = db.with_conn(|conn| {
            query_one(
                conn,
                "SELECT id FROM chapter WHERE id=? AND book_id=?",
                &[json!(chapter_id), json!(book_id)],
            )
        })?;
        if chapter.is_none() {
            return Err(AgentError::new("Глава не принадлежит книге"));
        }
        return Ok(Some(chapter_id));
    }
    Ok(None)
}

/// `tools/list` through the MCP dispatcher, minus the hidden tools, in both the
/// OpenAI definition shape and a lookup by name.
fn tool_definitions(db: &Db) -> (Vec<Value>, HashMap<String, Value>) {
    let response = mcp::dispatch(
        db,
        &json!({ "jsonrpc": "2.0", "id": 0, "method": "tools/list" }),
    );
    let listed = response
        .payload
        .as_ref()
        .and_then(|payload| payload.get("result"))
        .and_then(|result| result.get("tools"))
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();

    let mut definitions = Vec::with_capacity(listed.len());
    let mut schemas = HashMap::with_capacity(listed.len());
    for tool in listed {
        let Some(name) = tool.get("name").and_then(Value::as_str) else {
            continue;
        };
        if HIDDEN_TOOLS.contains(&name) {
            continue;
        }
        let schema = tool
            .get("inputSchema")
            .cloned()
            .unwrap_or_else(|| json!({ "type": "object" }));
        schemas.insert(name.to_string(), schema.clone());
        definitions.push(json!({
            "type": "function",
            "function": {
                "name": name,
                "description": tool.get("description").cloned().unwrap_or(Value::Null),
                "parameters": schema,
            },
        }));
    }
    (definitions, schemas)
}

/// `JSON.parse(call.function.arguments || '{}')`.
fn parse_tool_arguments(raw: &str) -> Result<Value, String> {
    if raw.is_empty() {
        return Ok(json!({}));
    }
    serde_json::from_str(raw).map_err(|err| err.to_string())
}

/// One `tools/call`, unwrapped from its JSON-RPC envelope.
///
/// The dispatcher answers tool failures as a *successful* call with
/// `isError: true` and a `Ошибка: …` text; the model must see the message, so
/// that text becomes the tool error here too.
fn call_tool(db: &Db, name: &str, args: &Value) -> Result<Value, String> {
    let response = mcp::dispatch(
        db,
        &json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "tools/call",
            "params": { "name": name, "arguments": args },
        }),
    );
    let Some(payload) = response.payload else {
        return Err("Инструмент не ответил".to_string());
    };
    if let Some(error) = payload.get("error") {
        let message = error
            .get("message")
            .and_then(Value::as_str)
            .unwrap_or("Ошибка инструмента");
        return Err(message.to_string());
    }
    let Some(result) = payload.get("result") else {
        return Err("Инструмент не ответил".to_string());
    };
    let text = result
        .get("content")
        .and_then(|content| content.get(0))
        .and_then(|part| part.get("text"))
        .and_then(Value::as_str)
        .unwrap_or("");
    if result.get("isError").is_some_and(truthy) {
        return Err(text.strip_prefix("Ошибка: ").unwrap_or(text).to_string());
    }
    serde_json::from_str(text).map_err(|err| err.to_string())
}

/// `validateArgs`: the schema checks that turn a confused model call into a
/// message the model can act on.
fn validate_tool_args(schema: &Value, args: &Value) -> Result<(), String> {
    let Some(object) = args.as_object() else {
        return Err("Аргументы инструмента должны быть объектом".to_string());
    };
    if let Some(required) = schema.get("required").and_then(Value::as_array) {
        for key in required {
            let key = key.as_str().unwrap_or_default();
            if object.get(key).is_none_or(Value::is_null) {
                return Err(format!("Нет обязательного поля {key}"));
            }
        }
    }
    let properties = schema.get("properties");
    let strict = schema
        .get("additionalProperties")
        .is_some_and(|value| value == &Value::Bool(false));
    for (key, value) in object {
        let Some(field) = properties.and_then(|properties| properties.get(key)) else {
            if strict {
                return Err(format!("Неизвестное поле {key}"));
            }
            continue;
        };
        if value.is_null() {
            continue;
        }
        match field.get("type").and_then(Value::as_str) {
            Some("integer") => {
                if !is_safe_integer(value) {
                    return Err(format!("Поле {key} должно быть целым числом"));
                }
            }
            Some("string") => {
                if !value.is_string() {
                    return Err(format!("Поле {key} должно быть строкой"));
                }
            }
            Some("object") => {
                if !value.is_object() {
                    return Err(format!("Поле {key} должно быть объектом"));
                }
            }
            Some("array") => {
                if !value.is_array() {
                    return Err(format!("Поле {key} должно быть списком"));
                }
            }
            _ => {}
        }
        if let Some(choices) = field.get("enum").and_then(Value::as_array) {
            if !choices.contains(value) {
                return Err(format!("Недопустимое значение {key}"));
            }
        }
    }
    Ok(())
}

/// `Number.isSafeInteger`.
fn is_safe_integer(value: &Value) -> bool {
    match value {
        Value::Number(number) => {
            number.as_i64().is_some()
                || number.as_f64().is_some_and(|number| {
                    number.is_finite()
                        && number.fract() == 0.0
                        && number.abs() <= 9_007_199_254_740_992.0
                })
        }
        _ => false,
    }
}

/// `assertToolScope`: the book-ownership and author-intent gates around a tool.
fn assert_tool_scope(
    db: &Db,
    book_id: i64,
    name: &str,
    args: &Value,
    message: &str,
) -> Result<(), String> {
    if MUTATIONS.contains(&name) && !core_ai::wants_action(message) {
        return Err("Автор не поручал изменять книгу в этом сообщении".to_string());
    }
    if let Some(value) = args.get("book_id") {
        if value != &json!(book_id) {
            return Err("Инструмент запросил другую книгу".to_string());
        }
    }
    if name == "delete_entity" && !core_ai::wants_delete(message) {
        return Err("Удаление разрешено только по прямой просьбе автора".to_string());
    }

    match name {
        "get_character_state" => owned_by_book(db, "character", "character_id", args, book_id)?,
        "get_chapter_text" | "write_chapter_text" => {
            owned_by_book(db, "chapter", "chapter_id", args, book_id)?
        }
        "close_fact" => owned_by_book(db, "fact", "fact_id", args, book_id)?,
        "update_promise" => owned_by_book(db, "promise", "promise_id", args, book_id)?,
        "add_knowledge" => owned_by_book(db, "character", "character_id", args, book_id)?,
        "add_fact" => {
            if args.get("subject_type").and_then(Value::as_str) == Some("character") {
                owned_by_book(db, "character", "subject_id", args, book_id)?;
            }
        }
        _ => {}
    }

    if name == "write_chapter_text" {
        let chapter = match args.get("chapter_id").and_then(json_sql::num) {
            Some(chapter_id) => db
                .with_conn(|conn| chapter_text::read_chapter_text(conn, chapter_id))
                .map_err(|err| sf_message(&err))?,
            None => return Err("Глава не найдена".to_string()),
        };
        if !chapter.body.is_empty() {
            let expected = args.get("expected_revision").and_then(Value::as_str);
            if expected != Some(chapter.revision.as_str()) {
                return Err(
                    "Глава уже содержит текст. Сначала прочитай её через get_chapter_text и \
                     передай текущую revision в expected_revision."
                        .to_string(),
                );
            }
        }
        let body_ok = match args.get("body") {
            Some(Value::String(body)) => {
                !body.trim().is_empty() && body.encode_utf16().count() <= 160_000
            }
            _ => false,
        };
        if !body_ok {
            return Err("Текст главы должен содержать от 1 до 160000 символов".to_string());
        }
    }
    Ok(())
}

/// A `SELECT book_id FROM <table> WHERE id=?` ownership probe; an absent id is
/// not an error, an unknown row is.
fn owned_by_book(
    db: &Db,
    table: &str,
    argument: &str,
    args: &Value,
    book_id: i64,
) -> Result<(), String> {
    let Some(id) = args.get(argument) else {
        return Ok(());
    };
    if id.is_null() {
        return Ok(());
    }
    let owner = db
        .with_conn(|conn| {
            query_one(
                conn,
                &format!("SELECT book_id FROM {table} WHERE id=?"),
                std::slice::from_ref(id),
            )
        })
        .map_err(|err| sf_message(&err))?
        .and_then(|row| row.get("book_id").and_then(json_sql::num));
    if owner != Some(book_id) {
        return Err(format!(
            "{table} #{} не принадлежит выбранной книге",
            js_scalar(Some(id))
        ));
    }
    Ok(())
}

/// The one-line Russian description of a written action.
fn action_label(db: &Db, name: &str, args: &Value, result: &Value) -> String {
    match name {
        "write_chapter_text" => {
            let number = args
                .get("chapter_id")
                .and_then(json_sql::num)
                .and_then(|chapter_id| {
                    db.with_conn(|conn| {
                        query_one(
                            conn,
                            "SELECT number FROM chapter WHERE id=?",
                            &[json!(chapter_id)],
                        )
                    })
                    .ok()
                    .flatten()
                })
                .and_then(|row| row.get("number").cloned());
            let which = match number {
                Some(value) if !value.is_null() => js_scalar(Some(&value)),
                _ => js_scalar(args.get("chapter_id")),
            };
            format!(
                "Глава {which}: записано {} знаков",
                js_scalar(result.get("chars_no_space"))
            )
        }
        "create_entity" => {
            if args.get("entity_type").and_then(Value::as_str) == Some("chapter") {
                format!("Создана глава {}", js_scalar(result.get("number")))
            } else {
                format!(
                    "Создано {} #{}",
                    js_scalar(args.get("entity_type")),
                    js_scalar(result.get("id"))
                )
            }
        }
        "update_entity" => {
            if args.get("entity_type").and_then(Value::as_str) == Some("chapter") {
                format!("Изменена глава {}", js_scalar(result.get("number")))
            } else {
                format!(
                    "Изменено {} #{}",
                    js_scalar(args.get("entity_type")),
                    js_scalar(args.get("id"))
                )
            }
        }
        "delete_entity" => format!(
            "Удалено {} #{}",
            js_scalar(args.get("entity_type")),
            js_scalar(args.get("id"))
        ),
        "add_fact" => format!(
            "Добавлен факт #{}",
            js_scalar(result.get("fact").and_then(|fact| fact.get("id")))
        ),
        "add_knowledge" => format!(
            "Добавлено знание #{}",
            js_scalar(result.get("knowledge").and_then(|item| item.get("id")))
        ),
        "close_fact" => format!("Закрыт факт #{}", js_scalar(args.get("fact_id"))),
        "add_promise" => format!("Добавлено ружьё #{}", js_scalar(result.get("id"))),
        "update_promise" => format!("Обновлено ружьё #{}", js_scalar(args.get("promise_id"))),
        _ => name.to_string(),
    }
}

/// `args.entity_type || (name === 'write_chapter_text' ? 'chapter' : null)`.
fn action_entity_type(name: &str, args: &Value) -> Value {
    match args.get("entity_type") {
        Some(value) if truthy(value) => value.clone(),
        _ => {
            if name == "write_chapter_text" {
                json!("chapter")
            } else {
                Value::Null
            }
        }
    }
}

/// `output?.id || output?.fact?.id || output?.knowledge?.id || args.chapter_id
/// || args.id || null` — the first truthy value wins.
fn action_id(args: &Value, result: &Value) -> Value {
    let candidates = [
        result.get("id"),
        result.get("fact").and_then(|fact| fact.get("id")),
        result.get("knowledge").and_then(|item| item.get("id")),
        args.get("chapter_id"),
        args.get("id"),
    ];
    for candidate in candidates.into_iter().flatten() {
        if truthy(candidate) {
            return candidate.clone();
        }
    }
    Value::Null
}

// ─────────────────────────── chapter tasks ───────────────────────────

/// `POST /api/chapters/:id/ai/task`: plan, draft, audit or style review.
async fn task(
    State(state): State<Arc<AppState>>,
    Path(id): Path<String>,
    body: Bytes,
) -> ApiResult {
    let input = parse_body(&body)?;
    let chapter_id = loose_id(&id);
    let kind = input.get("kind").and_then(Value::as_str).unwrap_or("");
    if core_ai::task_instruction(kind).is_none() {
        return Err(ApiError::invalid("Неизвестная задача ИИ"));
    }

    let chapter = state
        .db
        .with_conn(|conn| chapter_text::read_chapter_text(conn, chapter_id))?;

    let extra: String = match input.get("input") {
        Some(value) if truthy(value) => json_sql::text(value).chars().take(60_000).collect(),
        _ => String::new(),
    };
    if matches!(kind, "audit" | "style_review") && extra.trim().is_empty() {
        return Err(ApiError::invalid("Нет черновика для проверки"));
    }
    let scene_type: String = match input.get("scene_type") {
        Some(value) if truthy(value) => json_sql::text(value),
        _ => String::new(),
    };
    let scene_type: String = scene_type.trim().chars().take(60).collect();

    let book_id = chapter
        .chapter
        .get("book_id")
        .and_then(json_sql::num)
        .unwrap_or_default();
    let context = state.db.with_conn(|conn| {
        core_ai::build_ai_context(conn, book_id, Some(chapter_id), &scene_type)
    })?;

    let user = core_ai::task_user_prompt(kind, &extra, &chapter.chapter);
    let model = configured_model()?;
    let label: String = user.chars().take(10_000).collect();
    let run_id = start_run(&state, book_id, Some(chapter_id), kind, &model, &label)?;

    let options = core_ai::CompleteOptions {
        timeout_ms: if kind == "draft" { 180_000 } else { 120_000 },
        max_tokens: if kind == "draft" { 8_000 } else { 3_000 },
        ..core_ai::CompleteOptions::default()
    };
    let messages = vec![
        json!({ "role": "system", "content": core_ai::task_system_prompt(&context) }),
        json!({ "role": "user", "content": user }),
    ];

    match core_ai::complete_ai(&messages, &options).await {
        Ok(turn) => {
            finish_run(&state, run_id, &turn)?;
            let mut answer = turn.to_json();
            if let Some(object) = answer.as_object_mut() {
                object.insert("revision".to_string(), json!(chapter.revision));
            }
            Ok(Json(answer))
        }
        Err(err) => {
            fail_run(&state, run_id, err.message());
            Err(ApiError::new(502, err.message().to_string()))
        }
    }
}

/// `POST /api/chapters/:id/ai/extract`: propose facts and knowledge from the
/// chapter text, validated against the book before the author sees them.
async fn extract(State(state): State<Arc<AppState>>, Path(id): Path<String>) -> ApiResult {
    let chapter_id = loose_id(&id);
    let chapter = state
        .db
        .with_conn(|conn| chapter_text::read_chapter_text(conn, chapter_id))?;
    if chapter.body.trim().is_empty() {
        return Err(ApiError::invalid("В главе пока нет текста"));
    }
    let book_id = chapter
        .chapter
        .get("book_id")
        .and_then(json_sql::num)
        .unwrap_or_default();
    let characters = state.db.with_conn(|conn| {
        query(
            conn,
            "SELECT id,name FROM character WHERE book_id=?",
            &[json!(book_id)],
        )
    })?;
    let kinds: Vec<String> = state
        .db
        .with_conn(|conn| query(conn, "SELECT code FROM fact_kind", &[]))?
        .iter()
        .filter_map(|row| row.get("code").and_then(Value::as_str))
        .map(str::to_string)
        .collect();

    let prompt = core_ai::extract_prompt(&chapter.body, &characters, &kinds);
    let model = configured_model()?;
    let label = format!(
        "Извлечение фактов из главы {}",
        js_scalar(chapter.chapter.get("number"))
    );
    let run_id = start_run(&state, book_id, Some(chapter_id), "extract_facts", &model, &label)?;

    let options = core_ai::CompleteOptions {
        timeout_ms: 150_000,
        max_tokens: 3_500,
        ..core_ai::CompleteOptions::default()
    };
    let messages = vec![json!({ "role": "user", "content": prompt })];
    let turn = match core_ai::complete_ai(&messages, &options).await {
        Ok(turn) => turn,
        Err(err) => {
            fail_run(&state, run_id, err.message());
            return Err(ApiError::new(502, err.message().to_string()));
        }
    };
    let proposals = match core_ai::parse_proposals(&turn.text) {
        Ok(proposals) => proposals,
        Err(err) => {
            fail_run(&state, run_id, err.message());
            return Err(ApiError::new(err.status(), err.message().to_string()));
        }
    };
    let checked = match validate_proposals(&state, book_id, chapter_id, Some(&proposals)) {
        Ok(checked) => checked,
        Err(err) => {
            fail_run(&state, run_id, &err.message);
            return Err(err);
        }
    };
    finish_run(&state, run_id, &turn)?;

    Ok(Json(json!({
        "accepted": checked.accepted,
        "rejected": checked.rejected,
        "revision": checked.revision,
        "model": turn.model,
    })))
}

/// `POST /api/chapters/:id/ai/commit-facts`: persist the proposals the author
/// accepted, refusing a chapter that changed since extraction.
async fn commit_facts(
    State(state): State<Arc<AppState>>,
    Path(id): Path<String>,
    body: Bytes,
) -> ApiResult {
    let input = parse_body(&body)?;
    let chapter_id = loose_id(&id);
    let chapter = state.db.with_conn(|conn| {
        query_one(
            conn,
            "SELECT book_id FROM chapter WHERE id=?",
            &[json!(chapter_id)],
        )
    })?;
    let Some(chapter) = chapter else {
        return Err(ApiError::not_found("Глава не найдена"));
    };
    let book_id = chapter.get("book_id").and_then(json_sql::num).unwrap_or_default();
    let proposals = input
        .get("proposals")
        .and_then(Value::as_array)
        .map(Vec::as_slice);
    let expected = input.get("expected_revision").cloned().unwrap_or(Value::Null);

    let checked = validate_proposals(&state, book_id, chapter_id, proposals)?;
    if json!(checked.revision) != expected {
        return Err(ApiError::conflict(
            "Текст главы изменился после извлечения фактов. Повторите извлечение.",
        ));
    }
    if !checked.rejected.is_empty() {
        return Err(ApiError::invalid("Часть выбранных фактов не прошла проверку"));
    }

    let number = checked.chapter.get("number").cloned().unwrap_or(Value::Null);
    let moment = match checked.chapter.get("to_moment") {
        Some(value) if truthy(value) => value.clone(),
        _ => match checked.chapter.get("from_moment") {
            Some(value) if truthy(value) => value.clone(),
            _ => Value::Null,
        },
    };
    let accepted = checked.accepted;

    let inserted = state.db.with_tx(|tx| -> Result<i64, ApiError> {
        let mut inserted = 0i64;
        for proposal in &accepted {
            match proposal.get("type").and_then(Value::as_str).unwrap_or("") {
                "fact" => {
                    let subject_id = proposal
                        .get("subject_id")
                        .cloned()
                        .unwrap_or(Value::Null);
                    let prior = query_one(
                        tx,
                        "SELECT id FROM fact WHERE book_id=? AND source_chapter_id=?
                           AND subject_type=? AND subject_id IS ? AND kind=? AND value=? LIMIT 1",
                        &[
                            json!(book_id),
                            json!(chapter_id),
                            field_or_null(proposal, "subject_type"),
                            subject_id.clone(),
                            field_or_null(proposal, "kind"),
                            field_or_null(proposal, "value"),
                        ],
                    )?;
                    if prior.is_some() {
                        continue;
                    }
                    let input_fact = json!({
                        "subject_type": field_or_null(proposal, "subject_type"),
                        "subject_id": subject_id,
                        "subject_name": field_or_null(proposal, "subject_name"),
                        "kind": field_or_null(proposal, "kind"),
                        "key": field_or_null(proposal, "key"),
                        "value": field_or_null(proposal, "value"),
                        "from_moment": moment.clone(),
                        "from_chapter": number.clone(),
                        "hidden": proposal.get("hidden") == Some(&Value::Bool(true)),
                        "source": "ИИ: принято автором",
                        "source_chapter_id": chapter_id,
                        "source_quote": field_or_null(proposal, "source_quote"),
                    });
                    core_state::add_fact(tx, book_id, &input_fact)?;
                    inserted += 1;
                }
                "knowledge" => {
                    // Validation guarantees this is a character of the book.
                    let character_id = js_number_of(proposal.get("character_id")) as i64;
                    let prior = query_one(
                        tx,
                        "SELECT id FROM knowledge WHERE book_id=? AND source_chapter_id=?
                           AND character_id=? AND topic=? AND stance=? LIMIT 1",
                        &[
                            json!(book_id),
                            json!(chapter_id),
                            json!(character_id),
                            field_or_null(proposal, "topic"),
                            field_or_null(proposal, "stance"),
                        ],
                    )?;
                    if prior.is_some() {
                        continue;
                    }
                    let input_knowledge = json!({
                        "character_id": character_id,
                        "topic": field_or_null(proposal, "topic"),
                        "stance": field_or_null(proposal, "stance"),
                        "belief": field_or_null(proposal, "belief"),
                        "from_moment": moment.clone(),
                        "from_chapter": number.clone(),
                        "source": "ИИ: принято автором",
                        "source_chapter_id": chapter_id,
                        "source_quote": field_or_null(proposal, "source_quote"),
                    });
                    core_state::add_knowledge(tx, book_id, &input_knowledge)?;
                    inserted += 1;
                }
                _ => {
                    let prior = query_one(
                        tx,
                        "SELECT id FROM reader_knowledge WHERE book_id=? AND source_chapter_id=?
                           AND topic=? AND stance=? LIMIT 1",
                        &[
                            json!(book_id),
                            json!(chapter_id),
                            field_or_null(proposal, "topic"),
                            field_or_null(proposal, "stance"),
                        ],
                    )?;
                    if prior.is_some() {
                        continue;
                    }
                    insert(
                        tx,
                        "INSERT INTO reader_knowledge
                           (book_id,topic,stance,belief,from_moment,source_chapter_id,source_quote,source)
                         VALUES (?,?,?,?,?,?,?,?)",
                        &[
                            json!(book_id),
                            field_or_null(proposal, "topic"),
                            field_or_null(proposal, "stance"),
                            field_or_null(proposal, "belief"),
                            moment.clone(),
                            json!(chapter_id),
                            field_or_null(proposal, "source_quote"),
                            json!("ИИ: принято автором"),
                        ],
                    )?;
                    inserted += 1;
                }
            }
        }
        Ok(inserted)
    })?;

    Ok(Json(json!({ "inserted": inserted })))
}

/// `POST /api/chapters/:id/ai/suggest-edits`: a dry run that plans exact
/// replacements for the open notes. Nothing is written.
async fn suggest_edits(
    State(state): State<Arc<AppState>>,
    Path(id): Path<String>,
) -> ApiResult {
    let chapter_id = loose_id(&id);
    let chapter = state
        .db
        .with_conn(|conn| chapter_text::read_chapter_text(conn, chapter_id))?;
    let notes = state.db.with_conn(|conn| {
        query(
            conn,
            "SELECT * FROM annotation WHERE chapter_id=? AND status='открыта'",
            &[json!(chapter_id)],
        )
    })?;
    if notes.is_empty() {
        return Err(ApiError::invalid("Нет открытых заметок"));
    }
    let book_id = chapter
        .chapter
        .get("book_id")
        .and_then(json_sql::num)
        .unwrap_or_default();
    let context = state.db.with_conn(|conn| {
        core_ai::build_ai_context(conn, book_id, Some(chapter_id), "")
    })?;

    let prompt = core_ai::suggest_edits_prompt(&chapter.body, &notes);
    let model = configured_model()?;
    let label = format!("Правки по {} заметкам", notes.len());
    let run_id = start_run(
        &state,
        book_id,
        Some(chapter_id),
        "suggest_edits",
        &model,
        &label,
    )?;

    let options = core_ai::CompleteOptions {
        timeout_ms: 150_000,
        max_tokens: 4_500,
        ..core_ai::CompleteOptions::default()
    };
    let messages = vec![
        json!({
            "role": "system",
            "content": core_ai::suggest_edits_system_prompt(&context),
        }),
        json!({ "role": "user", "content": prompt }),
    ];
    let turn = match core_ai::complete_ai(&messages, &options).await {
        Ok(turn) => turn,
        Err(err) => {
            fail_run(&state, run_id, err.message());
            return Err(ApiError::new(502, err.message().to_string()));
        }
    };
    let parsed = match core_ai::parse_fenced_json(
        &turn.text,
        "Модель вернула некорректный JSON правок",
    ) {
        Ok(parsed) => parsed,
        Err(err) => {
            fail_run(&state, run_id, err.message());
            return Err(ApiError::new(err.status(), err.message().to_string()));
        }
    };
    let Some(edits) = parsed.get("edits").and_then(Value::as_array) else {
        let message = "В ответе нет списка правок";
        fail_run(&state, run_id, message);
        return Err(ApiError::new(502, message));
    };

    let mut notes_by_id = HashMap::with_capacity(notes.len());
    for note in &notes {
        if let Some(note_id) = note.get("id").and_then(json_sql::num) {
            notes_by_id.insert(note_id, note.clone());
        }
    }
    let outcome = anchor::plan_edits(&chapter.body, edits, &notes_by_id);
    finish_run(&state, run_id, &turn)?;

    Ok(Json(json!({
        "revision": chapter.revision,
        "preview": anchor::apply_planned(&chapter.body, &outcome.planned),
        "applied": applied_edits(&outcome.planned),
        "rejected": outcome.rejected,
    })))
}

/// `planned.map((p) => ({ annotation_id, old, new }))`: an absent
/// `annotation_id` disappears, an explicit `null` stays.
fn applied_edits(planned: &[anchor::PlannedEdit]) -> Vec<Value> {
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

// ─────────────────────────── proposals ───────────────────────────

/// Outcome of `validateProposals`: what may be committed, what was refused and
/// the revision the check ran against.
struct CheckedProposals {
    accepted: Vec<Value>,
    rejected: Vec<Value>,
    revision: String,
    chapter: Value,
}

/// `validateProposals` from `proposals.js`.
///
/// A quote must occur verbatim in the chapter, kinds and stances must exist,
/// characters must belong to the book, and two conflicting values for one slot
/// in the same batch are refused.
fn validate_proposals(
    state: &AppState,
    book_id: i64,
    chapter_id: i64,
    proposals: Option<&[Value]>,
) -> Result<CheckedProposals, ApiError> {
    state.db.with_conn(|conn| -> Result<CheckedProposals, ApiError> {
        let text = chapter_text::read_chapter_text(conn, chapter_id)?;
        if text.chapter.get("book_id").and_then(json_sql::num) != Some(book_id) {
            return Err(ApiError::not_found("Глава не принадлежит книге"));
        }
        let Some(proposals) = proposals else {
            return Err(ApiError::invalid("Некорректный список фактов"));
        };
        if proposals.len() > 100 {
            return Err(ApiError::invalid("Некорректный список фактов"));
        }

        let kinds: HashSet<String> = query(conn, "SELECT code FROM fact_kind", &[])?
            .iter()
            .filter_map(|row| row.get("code").and_then(Value::as_str))
            .map(str::to_string)
            .collect();
        let slotted = core_state::slotted_kinds(conn)?;
        let characters: HashSet<i64> =
            query(conn, "SELECT id FROM character WHERE book_id=?", &[json!(book_id)])?
                .iter()
                .filter_map(|row| row.get("id").and_then(json_sql::num))
                .collect();

        let mut slots: HashMap<String, String> = HashMap::new();
        let mut accepted = Vec::new();
        let mut rejected = Vec::new();
        for proposal in proposals {
            let proposal = proposal.clone();
            match proposal_reason(
                &proposal,
                &text.body,
                &kinds,
                &slotted,
                &characters,
                &mut slots,
            ) {
                Some(reason) => rejected.push(json!({ "proposal": proposal, "reason": reason })),
                None => accepted.push(proposal),
            }
        }

        Ok(CheckedProposals {
            accepted,
            rejected,
            revision: text.revision,
            chapter: text.chapter,
        })
    })
}

/// The rejection reason of one proposal, or `None` when it is acceptable.
fn proposal_reason(
    proposal: &Value,
    body: &str,
    kinds: &HashSet<String>,
    slotted: &HashSet<String>,
    characters: &HashSet<i64>,
    slots: &mut HashMap<String, String>,
) -> Option<&'static str> {
    let kind = proposal.get("type").and_then(Value::as_str).unwrap_or("");
    if !matches!(kind, "fact" | "knowledge" | "reader") {
        return Some("неизвестный тип");
    }
    let quote_ok = match proposal.get("source_quote").and_then(Value::as_str) {
        Some(quote) => {
            let length = quote.encode_utf16().count();
            (4..=1000).contains(&length) && body.contains(quote)
        }
        None => false,
    };
    if !quote_ok {
        return Some("цитата-источник не найдена дословно в главе");
    }

    match kind {
        "fact" => fact_reason(proposal, kinds, slotted, characters, slots),
        "knowledge" => {
            if !known_character(characters, proposal.get("character_id")) {
                Some("персонаж не из этой книги")
            } else if !stance_known(proposal.get("stance")) {
                Some("неизвестная позиция знания")
            } else if !topic_known(proposal.get("topic")) {
                Some("некорректная тема")
            } else {
                None
            }
        }
        _ => {
            if !stance_known(proposal.get("stance")) {
                Some("неизвестная позиция читателя")
            } else if !topic_known(proposal.get("topic")) {
                Some("некорректная тема")
            } else {
                None
            }
        }
    }
}

fn fact_reason(
    proposal: &Value,
    kinds: &HashSet<String>,
    slotted: &HashSet<String>,
    characters: &HashSet<i64>,
    slots: &mut HashMap<String, String>,
) -> Option<&'static str> {
    let kind = proposal.get("kind").and_then(Value::as_str).unwrap_or("");
    if !kinds.contains(kind) {
        return Some("неизвестный вид факта");
    }
    let value = proposal.get("value").and_then(Value::as_str);
    match value {
        Some(value) if !value.trim().is_empty() && value.encode_utf16().count() <= 500 => {}
        _ => return Some("некорректный факт"),
    }
    let subject_type = proposal
        .get("subject_type")
        .and_then(Value::as_str)
        .unwrap_or("");
    if subject_type == "character" && !known_character(characters, proposal.get("subject_id")) {
        return Some("персонаж не из этой книги");
    }
    if !matches!(
        subject_type,
        "character" | "plot" | "world" | "location" | "item"
    ) {
        return Some("неизвестный субъект");
    }
    if !slotted.contains(kind) {
        return None;
    }

    let slot = serde_json::to_string(&json!([
        subject_type,
        proposal.get("subject_id").cloned().unwrap_or(Value::Null),
        kind,
        proposal.get("key").cloned().unwrap_or(Value::Null),
    ]))
    .unwrap_or_default();
    let value = value.expect("checked above");
    match slots.get(&slot) {
        Some(previous) if previous.as_str() != value => {
            Some("два разных значения одного слота в одном моменте")
        }
        _ => {
            slots.insert(slot, value.to_string());
            None
        }
    }
}

/// `Number(value)` for an identifier field.
fn js_number_of(value: Option<&Value>) -> f64 {
    match value {
        None | Some(Value::Null) => 0.0,
        Some(Value::Bool(flag)) => f64::from(u8::from(*flag)),
        Some(Value::Number(number)) => number.as_f64().unwrap_or(f64::NAN),
        Some(Value::String(raw)) => core_state::js_number(raw),
        Some(_) => f64::NAN,
    }
}

fn known_character(characters: &HashSet<i64>, value: Option<&Value>) -> bool {
    let number = js_number_of(value);
    number.is_finite() && number.fract() == 0.0 && characters.contains(&(number as i64))
}

fn stance_known(value: Option<&Value>) -> bool {
    matches!(
        value.and_then(Value::as_str),
        Some("знает" | "подозревает" | "заблуждается" | "не знает")
    )
}

fn topic_known(value: Option<&Value>) -> bool {
    matches!(value.and_then(Value::as_str), Some(topic)
        if !topic.trim().is_empty() && topic.encode_utf16().count() <= 300)
}

/// `body[key] ?? null`.
fn field_or_null(body: &Value, key: &str) -> Value {
    match body.get(key) {
        None | Some(Value::Null) => Value::Null,
        Some(value) => value.clone(),
    }
}

// ─────────────────────────── shared run plumbing ───────────────────────────

/// The model name recorded on a run row; reading the settings may itself fail.
fn configured_model() -> Result<String, ApiError> {
    match core_ai::read_ai_config() {
        Ok(config) => Ok(config.model),
        Err(err) => Err(ApiError::new(err.status(), err.message())),
    }
}

/// Insert a `running` run row before the model is called.
fn start_run(
    state: &AppState,
    book_id: i64,
    chapter_id: Option<i64>,
    kind: &str,
    model: &str,
    prompt: &str,
) -> Result<i64, ApiError> {
    state.db.with_conn(|conn| -> Result<i64, ApiError> {
        Ok(insert(
            conn,
            "INSERT INTO ai_run (book_id, chapter_id, kind, engine, model, prompt)
             VALUES (?,?,?,?,?,?)",
            &[
                json!(book_id),
                chapter_id.map_or(Value::Null, Value::from),
                json!(kind),
                json!("api"),
                json!(model),
                json!(prompt),
            ],
        )?)
    })
}

/// Mark a run as answered, with its token counts.
fn finish_run(state: &AppState, run_id: i64, turn: &core_ai::AiTurn) -> Result<(), ApiError> {
    let answer = turn.text.clone();
    let tokens_in = turn.tokens_in.clone();
    let tokens_out = turn.tokens_out.clone();
    state.db.with_conn(move |conn| -> Result<(), ApiError> {
        exec(
            conn,
            "UPDATE ai_run SET result=?, status='ok', tokens_in=?, tokens_out=?,
               finished_at=datetime('now') WHERE id=?",
            &[json!(answer), tokens_in, tokens_out, json!(run_id)],
        )?;
        Ok(())
    })
}

/// Record the failure on the run row; the route keeps its own status.
fn fail_run(state: &AppState, run_id: i64, message: &str) {
    let _ = state.db.with_conn(|conn| -> Result<(), ApiError> {
        exec(
            conn,
            "UPDATE ai_run SET status='error', error=?, finished_at=datetime('now') WHERE id=?",
            &[json!(message), json!(run_id)],
        )?;
        Ok(())
    });
}

//! MCP tool surface — the port of `server/lib/mcpCore.js`.
//!
//! Transport-independent: [`dispatch`] handles one JSON-RPC 2.0 message and
//! says whether it needs a reply. The stdio transport (`crates/mcp`) and the
//! embedded HTTP route (`POST /mcp`) both call it, so the two cannot drift
//! apart — exactly the split the JavaScript edition had.
//!
//! Three kinds of data are exposed:
//!  * **tools** — reads and writes against the database;
//!  * **resources** — the book's own markdown files (`storyforge://<slug>/<key>`);
//!  * **prompts** — the `write_chapter` methodology.
//!
//! Tool failures are JSON-RPC *successes* carrying `isError: true`, because
//! that is what the JavaScript edition did: the model should see the message,
//! not a transport error.

use std::path::Path;
use std::sync::OnceLock;

use regex::Regex;
use rusqlite::Connection;
use serde_json::{Map, Value, json};

use crate::chapter_text::{self, SaveOptions};
use crate::db::Db;
use crate::error::SfError;
use crate::json_sql::{self, field, num, query, query_one, text, truthy};
use crate::state::{self, CharacterStateOpts, WorldStateOpts};

pub const SERVER_NAME: &str = "storyforge";
pub const SERVER_VERSION: &str = "1.1.0";
pub const DEFAULT_PROTOCOL_VERSION: &str = "2025-11-25";

/// What one message produced.
///
/// `has_reply == false` means "notification": the HTTP transport answers 202
/// with no body and the stdio transport writes nothing.
#[derive(Debug, Clone)]
pub struct DispatchResult {
    pub has_reply: bool,
    pub payload: Option<Value>,
}

impl DispatchResult {
    fn none() -> Self {
        Self {
            has_reply: false,
            payload: None,
        }
    }

    fn ok(id: Option<&Value>, result: Value) -> Self {
        Self {
            has_reply: id.is_some(),
            payload: Some(json!({
                "jsonrpc": "2.0",
                "id": id.cloned().unwrap_or(Value::Null),
                "result": result,
            })),
        }
    }

    fn error(id: Option<&Value>, code: i64, message: &str) -> Self {
        Self {
            has_reply: id.is_some(),
            payload: Some(json!({
                "jsonrpc": "2.0",
                "id": id.cloned().unwrap_or(Value::Null),
                "error": { "code": code, "message": message },
            })),
        }
    }
}

/// A tool failure. The message is what the model sees after `Ошибка: `.
#[derive(Debug, Clone)]
pub struct ToolError(pub String);

impl std::fmt::Display for ToolError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(&self.0)
    }
}

impl From<SfError> for ToolError {
    fn from(err: SfError) -> Self {
        match &err {
            // The core prefixes its messages with a category; the JavaScript
            // edition surfaced the bare Russian sentence.
            SfError::NotFound(message)
            | SfError::Invalid(message)
            | SfError::Conflict(message)
            | SfError::Config(message)
            | SfError::Other(message) => ToolError(message.clone()),
            other => ToolError(other.to_string()),
        }
    }
}

impl From<serde_json::Error> for ToolError {
    fn from(err: serde_json::Error) -> Self {
        ToolError(err.to_string())
    }
}

type ToolResult = Result<Value, ToolError>;

fn with_conn<T>(
    db: &Db,
    f: impl FnOnce(&Connection) -> Result<T, ToolError>,
) -> Result<T, ToolError> {
    db.with_conn(f)
}

// ─────────────────────────── small JS helpers ───────────────────────────

/// Template interpolation for `undefined`, `null` and the scalar values that
/// reach a Russian message (`${a.kind}`, `${params?.name}`).
fn js_display(value: Option<&Value>) -> String {
    match value {
        None => "undefined".to_string(),
        Some(Value::Null) => "null".to_string(),
        Some(Value::Bool(flag)) => flag.to_string(),
        Some(Value::Number(number)) => number.to_string(),
        Some(Value::String(text)) => text.clone(),
        Some(Value::Array(items)) => items
            .iter()
            .map(|item| js_display(Some(item)))
            .collect::<Vec<_>>()
            .join(","),
        Some(Value::Object(_)) => "[object Object]".to_string(),
    }
}

/// `String(value || '')` for the resource URI.
fn string_or_empty(value: Option<&Value>) -> String {
    match value {
        Some(value) if truthy(value) => js_display(Some(value)),
        _ => String::new(),
    }
}

fn req_i64(args: &Value, key: &str) -> Result<i64, ToolError> {
    num(&field(args, key)).ok_or_else(|| ToolError(format!("Некорректное поле «{key}»")))
}

fn opt_i64(args: &Value, key: &str) -> Option<i64> {
    num(&field(args, key))
}

/// `[1, 2]` → ids; anything else means "no filter".
fn id_list(value: &Value) -> Option<Vec<f64>> {
    value
        .as_array()
        .map(|items| items.iter().filter_map(num).map(|id| id as f64).collect())
}

fn str_list(value: &Value) -> Option<Vec<String>> {
    value.as_array().map(|items| {
        items
            .iter()
            .filter_map(Value::as_str)
            .map(str::to_string)
            .collect()
    })
}

fn number_or(value: &Value, default: i64) -> f64 {
    num(value).unwrap_or(default) as f64
}

// ─────────────────────────── tool identity ───────────────────────────

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ToolId {
    ListBooks,
    ListCharacters,
    GetState,
    GetCharacterState,
    SearchFacts,
    DiffState,
    GetDictionaries,
    AddFact,
    CloseFact,
    AddKnowledge,
    GetChapterText,
    WriteChapterText,
    ListPromises,
    AddPromise,
    UpdatePromise,
    RunImport,
    ListEntities,
    GetEntity,
    GetEntitySchema,
    CreateEntity,
    UpdateEntity,
    DeleteEntity,
}

struct ToolDef {
    id: ToolId,
    name: &'static str,
    description: &'static str,
    schema: Value,
}

fn no_args_schema() -> Value {
    json!({ "type": "object", "additionalProperties": false })
}

fn tools() -> Vec<ToolDef> {
    let entity_type = json!({ "type": "string", "enum": ENTITY_TYPES });
    let book_id = json!({ "type": "integer" });
    let chapter_no = json!({ "type": "integer" });

    vec![
        ToolDef {
            id: ToolId::ListBooks,
            name: "list_books",
            description: "Список книг в базе с id, названием и краткими счётчиками. Начинай с этого, чтобы узнать book_id.",
            schema: no_args_schema(),
        },
        ToolDef {
            id: ToolId::ListCharacters,
            name: "list_characters",
            description: "Персонажи книги с их id — нужны для character_id/subject_id в других вызовах.",
            schema: json!({
                "type": "object", "additionalProperties": false,
                "properties": { "book_id": book_id },
                "required": ["book_id"],
            }),
        },
        ToolDef {
            id: ToolId::GetState,
            name: "get_state",
            description: "Срез мира на момент истории: кто где, что у кого есть, кто что знает и в чём заблуждается, \
отношения, незакрытые обязательства, ружья в ожидании. Это замена перечитыванию прошлых глав — \
вызывай перед тем, как писать или планировать сцену.",
            schema: json!({
                "type": "object", "additionalProperties": false,
                "properties": {
                    "book_id": book_id,
                    "chapter": { "type": "integer", "description": "Срез на конец этой главы (обычно N-1 перед новой главой)" },
                    "characters": { "type": "array", "items": { "type": "integer" }, "description": "Ограничить участниками сцены" },
                    "kinds": { "type": "array", "items": { "type": "string" }, "description": "Ограничить видами фактов, напр. [\"внешность\",\"эмоция\"]" },
                    "events": { "type": "integer", "description": "Сколько последних событий вернуть" },
                    "min_weight": { "type": "integer", "description": "0 фон · 1 обычный · 2 ключевой" },
                },
                "required": ["book_id"],
            }),
        },
        ToolDef {
            id: ToolId::GetCharacterState,
            name: "get_character_state",
            description: "Полное досье персонажа на момент: факты по разделам, осведомлённость (знает/подозревает/заблуждается), отношения.",
            schema: json!({
                "type": "object", "additionalProperties": false,
                "properties": { "character_id": { "type": "integer" }, "chapter": chapter_no },
                "required": ["character_id"],
            }),
        },
        ToolDef {
            id: ToolId::SearchFacts,
            name: "search_facts",
            description: "Полнотекстовый поиск по фактам — найти конкретное, не выгружая весь срез.",
            schema: json!({
                "type": "object", "additionalProperties": false,
                "properties": {
                    "book_id": book_id,
                    "q": { "type": "string" },
                    "characters": { "type": "array", "items": { "type": "integer" } },
                    "kinds": { "type": "array", "items": { "type": "string" } },
                    "limit": { "type": "integer" },
                },
                "required": ["book_id", "q"],
            }),
        },
        ToolDef {
            id: ToolId::DiffState,
            name: "diff_state",
            description: "Что изменилось между двумя главами: появившиеся факты, закрывшиеся, новое знание.",
            schema: json!({
                "type": "object", "additionalProperties": false,
                "properties": { "book_id": book_id, "from_chapter": chapter_no, "to_chapter": chapter_no },
                "required": ["book_id", "from_chapter", "to_chapter"],
            }),
        },
        ToolDef {
            id: ToolId::GetDictionaries,
            name: "get_dictionaries",
            description: "Дерево видов фактов (kind) и допустимые позиции знания (stance). Сверяйся перед add_fact/add_knowledge — незнакомый вид молча уедет в «событие».",
            schema: no_args_schema(),
        },
        ToolDef {
            id: ToolId::AddFact,
            name: "add_fact",
            description: "Записать один факт о персонаже, предмете или месте — не абзац, одно утверждение. \
key задаёт слот (напр. kind=\"одежда\", key=\"верх\") — новый факт в тот же слот сам закрывает предыдущий. \
weight: 0 фон, 1 обычный, 2 ключевой.",
            schema: json!({
                "type": "object", "additionalProperties": false,
                "properties": {
                    "book_id": book_id,
                    "subject_type": { "type": "string", "enum": ["character", "location", "item"], "default": "character" },
                    "subject_id": { "type": "integer" },
                    "subject_name": { "type": "string", "description": "Если субъект без id — свободное имя" },
                    "kind": { "type": "string", "description": "Вид из get_dictionaries, напр. \"статус\", \"оружие\", \"эмоция\"" },
                    "key": { "type": "string", "description": "Имя слота, если у субъекта таких может быть несколько" },
                    "value": { "type": "string" },
                    "weight": { "type": "integer", "default": 1 },
                    "from_chapter": chapter_no,
                    "note": { "type": "string" },
                },
                "required": ["book_id", "kind", "value"],
            }),
        },
        ToolDef {
            id: ToolId::CloseFact,
            name: "close_fact",
            description: "Закрыть факт — он перестал быть верным с указанной главы (персонаж потерял предмет, статус сменился без замены и т.п.).",
            schema: json!({
                "type": "object", "additionalProperties": false,
                "properties": { "fact_id": { "type": "integer" }, "chapter": chapter_no },
                "required": ["fact_id", "chapter"],
            }),
        },
        ToolDef {
            id: ToolId::AddKnowledge,
            name: "add_knowledge",
            description: "Записать, что персонаж узнал, заподозрил или во что заблуждается. \
stance: \"знает\" · \"подозревает\" · \"заблуждается\" · \"не знает\". \
Заблуждение — не то же самое, что незнание: указывай belief, во что именно верит персонаж.",
            schema: json!({
                "type": "object", "additionalProperties": false,
                "properties": {
                    "book_id": book_id,
                    "character_id": { "type": "integer" },
                    "holder_label": { "type": "string", "description": "Если знание коллективное, а не у одного персонажа, напр. \"весь клуб\"" },
                    "topic": { "type": "string" },
                    "stance": { "type": "string", "enum": ["знает", "подозревает", "заблуждается", "не знает"] },
                    "belief": { "type": "string", "description": "Во что персонаж верит, если stance=\"заблуждается\"" },
                    "from_chapter": chapter_no,
                    "note": { "type": "string" },
                },
                "required": ["book_id", "topic", "stance"],
            }),
        },
        ToolDef {
            id: ToolId::GetChapterText,
            name: "get_chapter_text",
            description: "Прочитать текст главы (markdown) и её метаданные по id или по номеру.",
            schema: json!({
                "type": "object", "additionalProperties": false,
                "properties": { "chapter_id": { "type": "integer" }, "book_id": book_id, "number": chapter_no },
            }),
        },
        ToolDef {
            id: ToolId::WriteChapterText,
            name: "write_chapter_text",
            description: "Сохранить текст главы файлом. Предыдущая версия автоматически уходит в chapter_version — \
откат всегда возможен. Проставляет статус «написано» и пересчитывает знаки/слова. \
ПЕРЕД первым вызовом в сессии — если ещё не читал промпт write_chapter (prompts/get) и ресурсы \
стиля/голосов/правил мира (resources/read), сделай это сначала: текст без них рискует разойтись \
с голосом персонажей, ритмом стиля и уже установленными фактами.",
            schema: json!({
                "type": "object", "additionalProperties": false,
                "properties": {
                    "chapter_id": { "type": "integer" },
                    "body": { "type": "string" },
                    "reason": { "type": "string", "default": "правка ИИ через MCP" },
                    "expected_revision": { "type": "string", "description": "Ревизия из GET текста главы для защиты от перезаписи чужих правок" },
                },
                "required": ["chapter_id", "body"],
            }),
        },
        ToolDef {
            id: ToolId::ListPromises,
            name: "list_promises",
            description: "Ружья (обещания читателю): что заявлено, где должно выстрелить, статус.",
            schema: json!({
                "type": "object", "additionalProperties": false,
                "properties": { "book_id": book_id },
                "required": ["book_id"],
            }),
        },
        ToolDef {
            id: ToolId::AddPromise,
            name: "add_promise",
            description: "Завести новое ружьё.",
            schema: json!({
                "type": "object", "additionalProperties": false,
                "properties": {
                    "book_id": book_id,
                    "title": { "type": "string" },
                    "planted_ch": chapter_no,
                    "expected_payoff": { "type": "string" },
                },
                "required": ["book_id", "title"],
            }),
        },
        ToolDef {
            id: ToolId::UpdatePromise,
            name: "update_promise",
            description: "Пометить ружьё выстрелившим или обновить его статус.",
            schema: json!({
                "type": "object", "additionalProperties": false,
                "properties": {
                    "promise_id": { "type": "integer" },
                    "status": { "type": "string", "enum": ["ожидает", "выстрелило", "отменено"] },
                    "fired_ch": chapter_no,
                },
                "required": ["promise_id"],
            }),
        },
        ToolDef {
            id: ToolId::RunImport,
            name: "run_import",
            description: "Импортировать книги из markdown в базу — то же, что кнопка «Импорт» в приложении. Вызывай после ручных правок markdown-файлов.",
            schema: no_args_schema(),
        },
        ToolDef {
            id: ToolId::ListEntities,
            name: "list_entities",
            description: "Список записей книги. Типы: chapter, character, event, promise, relationship, relationship_state, moment, location, secret, hidden_line, timeline_entry, node, edge, character_state, hidden_beat, annotation. Используй для поиска id.",
            schema: json!({
                "type": "object", "additionalProperties": false,
                "properties": { "book_id": book_id, "entity_type": entity_type },
                "required": ["book_id", "entity_type"],
            }),
        },
        ToolDef {
            id: ToolId::GetEntity,
            name: "get_entity",
            description: "Прочитать одну запись по типу и id.",
            schema: json!({
                "type": "object", "additionalProperties": false,
                "properties": { "book_id": book_id, "entity_type": entity_type, "id": { "type": "integer" } },
                "required": ["book_id", "entity_type", "id"],
            }),
        },
        ToolDef {
            id: ToolId::GetEntitySchema,
            name: "get_entity_schema",
            description: "Узнать обязательные и разрешённые поля перед созданием или изменением записи.",
            schema: json!({
                "type": "object", "additionalProperties": false,
                "properties": { "entity_type": entity_type },
                "required": ["entity_type"],
            }),
        },
        ToolDef {
            id: ToolId::CreateEntity,
            name: "create_entity",
            description: "Создать запись книги. Для главы номер можно опустить: приложение выберет следующий. Для персонажа нужна name. Для других типов сначала вызови get_entity_schema.",
            schema: json!({
                "type": "object", "additionalProperties": false,
                "properties": {
                    "book_id": book_id,
                    "entity_type": entity_type,
                    "data": { "type": "object", "description": "Поля новой записи" },
                },
                "required": ["book_id", "entity_type", "data"],
            }),
        },
        ToolDef {
            id: ToolId::UpdateEntity,
            name: "update_entity",
            description: "Изменить поля существующей записи. Сначала найди её id и прочитай get_entity_schema. Текст прозы главы меняй через write_chapter_text.",
            schema: json!({
                "type": "object", "additionalProperties": false,
                "properties": {
                    "book_id": book_id,
                    "entity_type": entity_type,
                    "id": { "type": "integer" },
                    "changes": { "type": "object" },
                },
                "required": ["book_id", "entity_type", "id", "changes"],
            }),
        },
        ToolDef {
            id: ToolId::DeleteEntity,
            name: "delete_entity",
            description: "Удалить запись книги только по явной просьбе автора. Удаление главы может оставить её markdown-файл на диске; используй осторожно.",
            schema: json!({
                "type": "object", "additionalProperties": false,
                "properties": { "book_id": book_id, "entity_type": entity_type, "id": { "type": "integer" } },
                "required": ["book_id", "entity_type", "id"],
            }),
        },
    ]
}

// ─────────────────────────── tools ───────────────────────────

fn run_tool(db: &Db, id: ToolId, args: &Value) -> ToolResult {
    match id {
        ToolId::ListBooks => list_books(db),
        ToolId::ListCharacters => list_characters(db, args),
        ToolId::GetState => get_state(db, args),
        ToolId::GetCharacterState => get_character_state(db, args),
        ToolId::SearchFacts => search_facts(db, args),
        ToolId::DiffState => diff_state(db, args),
        ToolId::GetDictionaries => get_dictionaries(db),
        ToolId::AddFact => add_fact(db, args),
        ToolId::CloseFact => close_fact(db, args),
        ToolId::AddKnowledge => add_knowledge(db, args),
        ToolId::GetChapterText => get_chapter_text(db, args),
        ToolId::WriteChapterText => write_chapter_text(db, args),
        ToolId::ListPromises => list_promises(db, args),
        ToolId::AddPromise => add_promise(db, args),
        ToolId::UpdatePromise => update_promise(db, args),
        ToolId::RunImport => run_import(db),
        ToolId::ListEntities => list_entities(db, args),
        ToolId::GetEntity => get_entity(db, args),
        ToolId::GetEntitySchema => get_entity_schema(args),
        ToolId::CreateEntity => create_entity(db, args),
        ToolId::UpdateEntity => update_entity(db, args),
        ToolId::DeleteEntity => delete_entity(db, args),
    }
}

fn list_books(db: &Db) -> ToolResult {
    with_conn(db, |conn| {
        Ok(json!(query(
            conn,
            "SELECT id, slug, title, root_path, active_style FROM book ORDER BY title",
            &[],
        )?))
    })
}

fn list_characters(db: &Db, args: &Value) -> ToolResult {
    let book_id = req_i64(args, "book_id")?;
    with_conn(db, |conn| {
        Ok(json!(query(
            conn,
            "SELECT id, name, is_main, status FROM character WHERE book_id=? ORDER BY is_main DESC, name",
            &[json!(book_id)],
        )?))
    })
}

fn get_state(db: &Db, args: &Value) -> ToolResult {
    let book_id = req_i64(args, "book_id")?;
    let chapter = opt_i64(args, "chapter");
    let options = WorldStateOpts {
        characters: id_list(&field(args, "characters")).unwrap_or_default(),
        kinds: str_list(&field(args, "kinds")),
        events: num(&field(args, "events")).map(|value| value as f64),
        min_weight: num(&field(args, "min_weight")).map(|value| value as f64),
        full: false,
    };
    with_conn(db, |conn| {
        let at = state::resolve_moment(conn, book_id, None, chapter)?;
        Ok(state::world_state(conn, book_id, &at, &options)?)
    })
}

fn get_character_state(db: &Db, args: &Value) -> ToolResult {
    let character_id = req_i64(args, "character_id")?;
    let chapter = opt_i64(args, "chapter");
    with_conn(db, |conn| {
        let Some(row) = query_one(
            conn,
            "SELECT book_id FROM character WHERE id=?",
            &[json!(character_id)],
        )?
        else {
            return Err(ToolError("Персонаж не найден".to_string()));
        };
        let book_id = row.get("book_id").and_then(num).unwrap_or(0);
        let at = state::resolve_moment(conn, book_id, None, chapter)?;
        Ok(state::character_state(conn, character_id, &at, &CharacterStateOpts::default())?
            .unwrap_or(Value::Null))
    })
}

fn search_facts(db: &Db, args: &Value) -> ToolResult {
    let book_id = req_i64(args, "book_id")?;
    let query_text = match field(args, "q") {
        Value::String(value) => Some(value),
        _ => None,
    };
    let characters = id_list(&field(args, "characters")).unwrap_or_default();
    let kinds = str_list(&field(args, "kinds"));
    let limit = number_or(&field(args, "limit"), 50);
    with_conn(db, |conn| {
        Ok(json!(state::search_facts(
            conn,
            book_id,
            query_text.as_deref(),
            None,
            &characters,
            kinds.as_deref(),
            limit,
        )?))
    })
}

fn diff_state(db: &Db, args: &Value) -> ToolResult {
    let book_id = req_i64(args, "book_id")?;
    let from_chapter = req_i64(args, "from_chapter")?;
    let to_chapter = req_i64(args, "to_chapter")?;
    with_conn(db, |conn| {
        let from = state::resolve_moment(conn, book_id, None, Some(from_chapter))?;
        let to = state::resolve_moment(conn, book_id, None, Some(to_chapter))?;
        Ok(state::diff(conn, book_id, &from, &to)?)
    })
}

fn get_dictionaries(db: &Db) -> ToolResult {
    with_conn(db, |conn| {
        Ok(json!({
            "fact_kinds": state::kind_tree(conn)?,
            "stances": query(conn, "SELECT * FROM knowledge_stance ORDER BY ord", &[])?,
        }))
    })
}

fn add_fact(db: &Db, args: &Value) -> ToolResult {
    let book_id = req_i64(args, "book_id")?;
    let mut input = args.clone();
    if let Some(object) = input.as_object_mut() {
        object.insert("source".to_string(), json!("ИИ (MCP)"));
    }
    with_conn(db, |conn| Ok(state::add_fact(conn, book_id, &input)?))
}

fn close_fact(db: &Db, args: &Value) -> ToolResult {
    let fact_id = req_i64(args, "fact_id")?;
    let chapter = req_i64(args, "chapter")?;
    with_conn(db, |conn| {
        let Some(fact) = query_one(conn, "SELECT * FROM fact WHERE id=?", &[json!(fact_id)])? else {
            return Err(ToolError("Факт не найден".to_string()));
        };
        let book_id = fact.get("book_id").and_then(num).unwrap_or(0);
        let moment = state::moment_of_chapter(conn, book_id, Some(chapter))?;
        json_sql::exec(
            conn,
            "UPDATE fact SET to_moment=?, to_chapter=? WHERE id=?",
            &[
                moment.map_or(Value::Null, |value| json!(value)),
                json!(chapter),
                json!(fact_id),
            ],
        )?;
        Ok(query_one(conn, "SELECT * FROM fact WHERE id=?", &[json!(fact_id)])?
            .unwrap_or(Value::Null))
    })
}

fn add_knowledge(db: &Db, args: &Value) -> ToolResult {
    let book_id = req_i64(args, "book_id")?;
    let mut input = args.clone();
    if let Some(object) = input.as_object_mut() {
        object.insert("source".to_string(), json!("ИИ (MCP)"));
    }
    with_conn(db, |conn| Ok(state::add_knowledge(conn, book_id, &input)?))
}

fn get_chapter_text(db: &Db, args: &Value) -> ToolResult {
    let chapter_id = field(args, "chapter_id");
    let id = if chapter_id.is_null() {
        match (opt_i64(args, "book_id"), opt_i64(args, "number")) {
            (Some(book_id), Some(number)) => with_conn(db, |conn| {
                Ok(query_one(
                    conn,
                    "SELECT id FROM chapter WHERE book_id=? AND number=?",
                    &[json!(book_id), json!(number)],
                )?
                .and_then(|row| row.get("id").and_then(num)))
            })?,
            _ => None,
        }
    } else {
        num(&chapter_id)
    };
    let Some(id) = id else {
        return Err(ToolError(
            "Не нашёл главу — укажи chapter_id или book_id+number".to_string(),
        ));
    };
    let file = with_conn(db, |conn| Ok(chapter_text::chapter_file(conn, id)?))?;
    let body = if file.full.exists() {
        std::fs::read(&file.full)
            .map(|bytes| String::from_utf8_lossy(&bytes).into_owned())
            .unwrap_or_default()
    } else {
        String::new()
    };
    Ok(json!({
        "id": file.chapter.get("id").cloned().unwrap_or(Value::Null),
        "number": file.chapter.get("number").cloned().unwrap_or(Value::Null),
        "title": file.chapter.get("title").cloned().unwrap_or(Value::Null),
        "status": file.chapter.get("status").cloned().unwrap_or(Value::Null),
        "chars_no_space": file.chapter.get("chars_no_space").cloned().unwrap_or(Value::Null),
        "revision": chapter_text::revision_of(&body),
        "body": body,
    }))
}

fn write_chapter_text(db: &Db, args: &Value) -> ToolResult {
    let chapter_id = req_i64(args, "chapter_id")?;
    let body = match field(args, "body") {
        Value::String(body) => body,
        _ => return Err(ToolError("Нет текста".to_string())),
    };
    let reason = match field(args, "reason") {
        value if truthy(&value) => text(&value),
        _ => "правка ИИ через MCP".to_string(),
    };
    let options = SaveOptions {
        expected_revision: expected_revision(args),
        reason,
        ai_run_id: None,
    };
    let outcome = chapter_text::save_chapter_text(db, chapter_id, &body, &options)?;
    let chars = outcome.chars_no_space;
    let book_min = with_conn(db, |conn| {
        Ok(query_one(
            conn,
            "SELECT min_chars FROM book WHERE id=(SELECT book_id FROM chapter WHERE id=?)",
            &[json!(chapter_id)],
        )?
        .and_then(|row| row.get("min_chars").and_then(num)))
    })?;
    let note = if (chars as i64) < book_min.unwrap_or(10_000) {
        json!("меньше минимума площадки")
    } else {
        Value::Null
    };
    let mut map = Map::new();
    map.insert("ok".to_string(), Value::Bool(true));
    if outcome.unchanged {
        map.insert("unchanged".to_string(), Value::Bool(true));
    }
    map.insert("revision".to_string(), json!(outcome.revision));
    map.insert("chars_no_space".to_string(), json!(chars));
    map.insert("words".to_string(), json!(outcome.words));
    map.insert("min_chars_note".to_string(), note);
    Ok(Value::Object(map))
}

/// `expected_revision` is only enforced when present and not null; a
/// non-string can never match the current revision.
fn expected_revision(args: &Value) -> Option<String> {
    match args.get("expected_revision") {
        None | Some(Value::Null) => None,
        Some(Value::String(value)) => Some(value.clone()),
        Some(other) => Some(other.to_string()),
    }
}

/// `importAll(join(APP_ROOT, '..'))`, mapped to `{book_id, title, stats, warnings}`.
fn run_import(db: &Db) -> ToolResult {
    let outcomes = crate::import::import_all(db, &crate::import::default_library_root())?;
    Ok(Value::Array(
        outcomes
            .iter()
            .map(|outcome| {
                json!({
                    "book_id": outcome.book_id,
                    "title": outcome.title,
                    "stats": Value::Object(outcome.stats.clone()),
                    "warnings": outcome.warnings,
                })
            })
            .collect(),
    ))
}

fn list_promises(db: &Db, args: &Value) -> ToolResult {
    let book_id = req_i64(args, "book_id")?;
    with_conn(db, |conn| {
        Ok(json!(query(
            conn,
            "SELECT * FROM promise WHERE book_id=? ORDER BY id",
            &[json!(book_id)],
        )?))
    })
}

fn add_promise(db: &Db, args: &Value) -> ToolResult {
    let book_id = req_i64(args, "book_id")?;
    let data = json!({
        "book_id": book_id,
        "title": field(args, "title"),
        "planted_ch": opt_i64(args, "planted_ch").map_or(Value::Null, |value| json!(value)),
        "expected_payoff": field(args, "expected_payoff"),
        "status": "ожидает",
    });
    with_conn(db, |conn| insert_row(conn, "promise", &data))
}

fn update_promise(db: &Db, args: &Value) -> ToolResult {
    let promise_id = req_i64(args, "promise_id")?;
    let mut body = args.clone();
    if let Some(object) = body.as_object_mut() {
        object.remove("promise_id");
    }
    with_conn(db, |conn| {
        state::patch_row(conn, "promise", promise_id, EDIT_PROMISE, &body)?;
        Ok(query_one(conn, "SELECT * FROM promise WHERE id=?", &[json!(promise_id)])?
            .unwrap_or(Value::Null))
    })
}

// ─────────────────────────── entity CRUD ───────────────────────────

const ENTITY_TYPES: [&str; 16] = [
    "chapter",
    "character",
    "event",
    "promise",
    "relationship",
    "moment",
    "location",
    "secret",
    "hidden_line",
    "timeline_entry",
    "node",
    "edge",
    "character_state",
    "relationship_state",
    "hidden_beat",
    "annotation",
];

const EDIT_CHARACTER: &[&str] = &[
    "name",
    "aliases",
    "is_main",
    "status",
    "age",
    "gender",
    "group_name",
    "summary",
    "character_history",
    "voice",
    "voice_rules",
    "atmosphere_desc",
    "atmosphere_level",
    "appearance",
    "goals",
    "beliefs",
    "knows",
    "notes",
    "arc_from",
    "arc_to",
    "arc_stage",
];
const EDIT_CHAPTER: &[&str] = &["number", "title", "status", "arc_id", "pov", "summary", "plan"];
const EDIT_EVENT: &[&str] = &[
    "chapter_id",
    "chapter_end_id",
    "title",
    "summary",
    "status",
    "hidden",
    "weight",
    "arc_id",
    "story_date",
    "x",
    "y",
];
const EDIT_NODE: &[&str] = &[
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
const EDIT_ANNOTATION: &[&str] = &[
    "note",
    "status",
    "ai_result",
    "quote",
    "prefix",
    "suffix",
    "start_pos",
    "end_pos",
];
const EDIT_PROMISE: &[&str] = &["title", "planted_ch", "expected_payoff", "status", "fired_ch"];
const EDIT_CHARACTER_STATE: &[&str] = &["chapter_no", "kind", "text", "hidden", "ord"];
const EDIT_MOMENT: &[&str] = &["seq", "label", "world", "story_date", "note"];

const CHAPTER_CREATE: &[&str] = &["number", "title", "pov", "summary", "plan"];
const CHARACTER_STATE_CREATE: &[&str] = &[
    "character_id",
    "chapter_no",
    "kind",
    "text",
    "hidden",
    "ord",
];
const ANNOTATION_CREATE: &[&str] = &[
    "chapter_id",
    "note",
    "status",
    "ai_result",
    "quote",
    "prefix",
    "suffix",
    "start_pos",
    "end_pos",
];
const RELATIONSHIP_FIELDS: &[&str] = &[
    "from_char",
    "kind",
    "current_state",
    "history",
    "is_mutual",
    "to_char",
    "to_label",
];
const RELATIONSHIP_STATE_FIELDS: &[&str] = &[
    "chapter_no",
    "to_chapter",
    "from_moment",
    "to_moment",
    "state",
    "note",
    "event_id",
];
const RELATIONSHIP_STATE_CREATE: &[&str] = &[
    "relationship_id",
    "chapter_no",
    "to_chapter",
    "from_moment",
    "to_moment",
    "state",
    "note",
    "event_id",
];
const LOCATION_FIELDS: &[&str] = &[
    "name",
    "kind",
    "where_desc",
    "description",
    "who_here",
    "events_here",
];
const SECRET_FIELDS: &[&str] = &["ext_no", "title", "truth", "reader_knows", "reveal_plan"];
const HIDDEN_LINE_FIELDS: &[&str] = &[
    "code",
    "name",
    "truth",
    "who_knows",
    "reader_learns",
    "status",
];
const TIMELINE_FIELDS: &[&str] = &[
    "chapter_no",
    "story_date",
    "description",
    "who",
    "hidden",
    "ord",
    "moment_id",
];
const EDGE_FIELDS: &[&str] = &[
    "from_node",
    "from_port",
    "to_node",
    "to_port",
    "kind",
    "label",
];
const HIDDEN_BEAT_FIELDS: &[&str] = &["chapter_no", "offscreen", "visible_effect", "planned"];
const HIDDEN_BEAT_CREATE: &[&str] = &[
    "hidden_line_id",
    "chapter_no",
    "offscreen",
    "visible_effect",
    "planned",
];

struct EntityConfig {
    fields: &'static [&'static str],
    /// `None` means "same as `fields`".
    create: Option<&'static [&'static str]>,
    required: &'static [&'static str],
    /// `(parent table, foreign-key column)` for rows that belong to a parent
    /// record instead of carrying `book_id` themselves.
    parent: Option<(&'static str, &'static str)>,
}

impl EntityConfig {
    fn create_fields(&self) -> &'static [&'static str] {
        self.create.unwrap_or(self.fields)
    }
}

fn entity_config(kind: &str) -> Option<EntityConfig> {
    let config = match kind {
        "chapter" => EntityConfig {
            fields: EDIT_CHAPTER,
            create: Some(CHAPTER_CREATE),
            required: &[],
            parent: None,
        },
        "character" => EntityConfig {
            fields: EDIT_CHARACTER,
            create: None,
            required: &["name"],
            parent: None,
        },
        "event" => EntityConfig {
            fields: EDIT_EVENT,
            create: None,
            required: &["title"],
            parent: None,
        },
        "promise" => EntityConfig {
            fields: EDIT_PROMISE,
            create: None,
            required: &["title"],
            parent: None,
        },
        "relationship" => EntityConfig {
            fields: RELATIONSHIP_FIELDS,
            create: None,
            required: &["from_char", "current_state"],
            parent: None,
        },
        "moment" => EntityConfig {
            fields: EDIT_MOMENT,
            create: None,
            required: &["seq", "label"],
            parent: None,
        },
        "location" => EntityConfig {
            fields: LOCATION_FIELDS,
            create: None,
            required: &["name"],
            parent: None,
        },
        "secret" => EntityConfig {
            fields: SECRET_FIELDS,
            create: None,
            required: &["title"],
            parent: None,
        },
        "hidden_line" => EntityConfig {
            fields: HIDDEN_LINE_FIELDS,
            create: None,
            required: &["name"],
            parent: None,
        },
        "timeline_entry" => EntityConfig {
            fields: TIMELINE_FIELDS,
            create: None,
            required: &["description"],
            parent: None,
        },
        "node" => EntityConfig {
            fields: EDIT_NODE,
            create: None,
            required: &["type"],
            parent: None,
        },
        "edge" => EntityConfig {
            fields: EDGE_FIELDS,
            create: None,
            required: &["from_node", "to_node"],
            parent: None,
        },
        "character_state" => EntityConfig {
            fields: EDIT_CHARACTER_STATE,
            create: Some(CHARACTER_STATE_CREATE),
            required: &["character_id", "text"],
            parent: Some(("character", "character_id")),
        },
        "relationship_state" => EntityConfig {
            fields: RELATIONSHIP_STATE_FIELDS,
            create: Some(RELATIONSHIP_STATE_CREATE),
            required: &["relationship_id", "state"],
            parent: Some(("relationship", "relationship_id")),
        },
        "hidden_beat" => EntityConfig {
            fields: HIDDEN_BEAT_FIELDS,
            create: Some(HIDDEN_BEAT_CREATE),
            required: &["hidden_line_id", "chapter_no"],
            parent: Some(("hidden_line", "hidden_line_id")),
        },
        "annotation" => EntityConfig {
            fields: EDIT_ANNOTATION,
            create: Some(ANNOTATION_CREATE),
            required: &["chapter_id", "note"],
            parent: Some(("chapter", "chapter_id")),
        },
        _ => return None,
    };
    Some(config)
}

fn foreign_table(key: &str) -> Option<&'static str> {
    Some(match key {
        "chapter_id" => "chapter",
        "character_id" => "character",
        "from_char" => "character",
        "to_char" => "character",
        "from_node" => "node",
        "to_node" => "node",
        "moment_id" => "moment",
        "from_moment" => "moment",
        "to_moment" => "moment",
        "hidden_line_id" => "hidden_line",
        "relationship_id" => "relationship",
        "arc_id" => "arc",
        "event_id" => "event",
        "to_char_id" => "character",
        _ => return None,
    })
}

/// `entityBook`: the row plus the book it belongs to, directly or through its
/// parent record.
fn entity_book(conn: &Connection, kind: &str, id: i64) -> Result<(Value, Option<i64>), ToolError> {
    let config = entity_config(kind).ok_or_else(|| ToolError("Неизвестный тип записи".into()))?;
    let row = query_one(conn, &format!("SELECT * FROM {kind} WHERE id=?"), &[json!(id)])?
        .ok_or_else(|| ToolError("Запись не найдена".into()))?;
    let Some((parent_table, parent_key)) = config.parent else {
        let book_id = row.get("book_id").and_then(num);
        return Ok((row, book_id));
    };
    let book_id = match row.get(parent_key).and_then(num) {
        Some(parent_id) => query_one(
            conn,
            &format!("SELECT book_id FROM {parent_table} WHERE id=?"),
            &[json!(parent_id)],
        )?
        .and_then(|parent| parent.get("book_id").and_then(num)),
        None => None,
    };
    Ok((row, book_id))
}

/// `assertBook`: a foreign key may only point inside the same book.
fn assert_book(conn: &Connection, book_id: i64, kind: &str, value: &Value) -> Result<(), ToolError> {
    let Some(id) = num(value) else {
        return Ok(());
    };
    let found = if kind == "arc" {
        query_one(conn, "SELECT book_id FROM arc WHERE id=?", &[json!(id)])?
            .and_then(|row| row.get("book_id").and_then(num))
    } else {
        entity_book(conn, kind, id)?.1
    };
    if found != Some(book_id) {
        return Err(ToolError(format!(
            "Связанная запись {kind} #{} не принадлежит выбранной книге",
            js_display(Some(value))
        )));
    }
    Ok(())
}

fn clean_data(
    conn: &Connection,
    book_id: i64,
    kind: &str,
    data: &Value,
    creation: bool,
) -> Result<Map<String, Value>, ToolError> {
    let config = entity_config(kind).ok_or_else(|| ToolError("Неизвестный тип записи".into()))?;
    let Some(input) = data.as_object() else {
        return Err(ToolError("Нужны поля записи".into()));
    };
    let allowed = if creation {
        config.create_fields()
    } else {
        config.fields
    };
    let mut out = Map::new();
    for (key, value) in input {
        if !allowed.contains(&key.as_str()) {
            return Err(ToolError(format!("Поле {key} нельзя изменять у {kind}")));
        }
        let scalar = matches!(
            value,
            Value::Null | Value::String(_) | Value::Number(_) | Value::Bool(_)
        );
        if !scalar {
            return Err(ToolError(format!("Некорректное значение поля {key}")));
        }
        if let Value::String(text) = value
            && text.encode_utf16().count() > 120_000
        {
            return Err(ToolError(format!("Слишком длинное поле {key}")));
        }
        if let Some(foreign) = foreign_table(key) {
            assert_book(conn, book_id, foreign, value)?;
        }
        out.insert(
            key.clone(),
            match value {
                Value::Bool(flag) => json!(i64::from(*flag)),
                other => other.clone(),
            },
        );
    }
    if creation {
        for key in config.required {
            let empty = match out.get(*key) {
                None | Some(Value::Null) => true,
                Some(Value::String(value)) => value.is_empty(),
                _ => false,
            };
            if empty {
                return Err(ToolError(format!("Для {kind} нужно поле {key}")));
            }
        }
    }
    if out.is_empty() && !(creation && kind == "chapter") {
        return Err(ToolError("Нет полей для изменения".into()));
    }
    Ok(out)
}

fn insert_row(conn: &Connection, table: &str, data: &Value) -> ToolResult {
    let Some(object) = data.as_object() else {
        return Err(ToolError("Нужны поля записи".into()));
    };
    let columns: Vec<&str> = object.keys().map(String::as_str).collect();
    let placeholders = vec!["?"; columns.len()].join(", ");
    let sql = format!(
        "INSERT INTO {table} ({}) VALUES ({placeholders})",
        columns.join(", ")
    );
    let params: Vec<Value> = object.values().cloned().collect();
    let id = json_sql::insert(conn, &sql, &params)?;
    Ok(query_one(conn, &format!("SELECT * FROM {table} WHERE id=?"), &[json!(id)])?
        .unwrap_or(Value::Null))
}

fn scope_book(conn: &Connection, book_id: i64) -> Result<(), ToolError> {
    if query_one(conn, "SELECT id FROM book WHERE id=?", &[json!(book_id)])?.is_none() {
        return Err(ToolError("Книга не найдена".into()));
    }
    Ok(())
}

fn entity_type_arg(args: &Value) -> String {
    js_display(args.get("entity_type"))
}

fn list_entities(db: &Db, args: &Value) -> ToolResult {
    let book_id = req_i64(args, "book_id")?;
    let kind = entity_type_arg(args);
    with_conn(db, |conn| {
        scope_book(conn, book_id)?;
        let config =
            entity_config(&kind).ok_or_else(|| ToolError("Неизвестный тип записи".into()))?;
        let rows = match config.parent {
            None => query(
                conn,
                &format!("SELECT * FROM {kind} WHERE book_id=? ORDER BY id LIMIT 100"),
                &[json!(book_id)],
            )?,
            Some((parent_table, parent_key)) => query(
                conn,
                &format!(
                    "SELECT child.* FROM {kind} child
                     JOIN {parent_table} parent ON parent.id=child.{parent_key}
                     WHERE parent.book_id=? ORDER BY child.id LIMIT 100"
                ),
                &[json!(book_id)],
            )?,
        };
        Ok(json!(rows))
    })
}

fn get_entity(db: &Db, args: &Value) -> ToolResult {
    let book_id = req_i64(args, "book_id")?;
    let kind = entity_type_arg(args);
    let id = req_i64(args, "id")?;
    with_conn(db, |conn| {
        let (row, found) = entity_book(conn, &kind, id)?;
        if found != Some(book_id) {
            return Err(ToolError("Запись не принадлежит выбранной книге".into()));
        }
        Ok(row)
    })
}

fn get_entity_schema(args: &Value) -> ToolResult {
    let kind = entity_type_arg(args);
    let config = entity_config(&kind).ok_or_else(|| ToolError("Неизвестный тип записи".into()))?;
    Ok(json!({
        "type": kind,
        "required": config.required,
        "create_fields": config.create_fields(),
        "update_fields": config.fields,
        "parent": config.parent.map_or(Value::Null, |(table, key)| json!([table, key])),
    }))
}

fn create_entity(db: &Db, args: &Value) -> ToolResult {
    let book_id = req_i64(args, "book_id")?;
    let kind = entity_type_arg(args);
    let data = field(args, "data");
    with_conn(db, |conn| {
        scope_book(conn, book_id)?;
        let config =
            entity_config(&kind).ok_or_else(|| ToolError("Неизвестный тип записи".into()))?;
        let mut fields = clean_data(conn, book_id, &kind, &data, true)?;

        if kind == "chapter" {
            if fields.get("number").is_none_or(Value::is_null) {
                let max = query_one(
                    conn,
                    "SELECT MAX(number) n FROM chapter WHERE book_id=?",
                    &[json!(book_id)],
                )?
                .and_then(|row| row.get("n").and_then(num))
                .unwrap_or(0);
                fields.insert("number".to_string(), json!(max + 1));
            }
            if fields.get("title").is_none_or(Value::is_null) {
                let number = fields.get("number").and_then(num).unwrap_or(0);
                fields.insert("title".to_string(), json!(format!("Глава {number}")));
            }
        }

        let mut payload = Map::new();
        if config.parent.is_none() {
            payload.insert("book_id".to_string(), json!(book_id));
        }
        for (key, value) in fields {
            payload.insert(key, value);
        }
        insert_row(conn, &kind, &Value::Object(payload))
    })
}

fn update_entity(db: &Db, args: &Value) -> ToolResult {
    let book_id = req_i64(args, "book_id")?;
    let kind = entity_type_arg(args);
    let id = req_i64(args, "id")?;
    let changes = field(args, "changes");
    with_conn(db, |conn| {
        scope_book(conn, book_id)?;
        let (_row, found) = entity_book(conn, &kind, id)?;
        if found != Some(book_id) {
            return Err(ToolError("Запись не принадлежит выбранной книге".into()));
        }
        let config =
            entity_config(&kind).ok_or_else(|| ToolError("Неизвестный тип записи".into()))?;
        let fields = clean_data(conn, book_id, &kind, &changes, false)?;
        state::patch_row(conn, &kind, id, config.fields, &Value::Object(fields))?;
        Ok(query_one(conn, &format!("SELECT * FROM {kind} WHERE id=?"), &[json!(id)])?
            .unwrap_or(Value::Null))
    })
}

fn delete_entity(db: &Db, args: &Value) -> ToolResult {
    let book_id = req_i64(args, "book_id")?;
    let kind = entity_type_arg(args);
    let id = req_i64(args, "id")?;
    with_conn(db, |conn| {
        let (_row, found) = entity_book(conn, &kind, id)?;
        if found != Some(book_id) {
            return Err(ToolError("Запись не принадлежит выбранной книге".into()));
        }
        if kind == "chapter" {
            return Err(ToolError(
                "Главу удаляйте через редактор после сохранения копии; инструмент агента не удаляет главы"
                    .into(),
            ));
        }
        json_sql::exec(conn, &format!("DELETE FROM {kind} WHERE id=?"), &[json!(id)])?;
        Ok(json!({ "deleted": true, "entity_type": kind, "id": id }))
    })
}

// ─────────────────────────── activity log ───────────────────────────

struct ActivityEntry {
    book_id: Option<i64>,
    summary: String,
    detail: Value,
}

fn has_activity(id: ToolId) -> bool {
    matches!(
        id,
        ToolId::AddFact
            | ToolId::CloseFact
            | ToolId::AddKnowledge
            | ToolId::WriteChapterText
            | ToolId::AddPromise
            | ToolId::UpdatePromise
            | ToolId::RunImport
            | ToolId::CreateEntity
            | ToolId::UpdateEntity
            | ToolId::DeleteEntity
    )
}

fn label_subject(conn: &Connection, args: &Value) -> Result<String, ToolError> {
    if let Some(name) = args.get("subject_name")
        && truthy(name)
    {
        return Ok(text(name));
    }
    if args.get("subject_type").map(text).as_deref() == Some("character")
        && let Some(subject_id) = num(&field(args, "subject_id"))
    {
        if let Some(row) = query_one(
            conn,
            "SELECT name FROM character WHERE id=?",
            &[json!(subject_id)],
        )? {
            return Ok(row.get("name").map(text).unwrap_or_default());
        }
    }
    let subject_id = field(args, "subject_id");
    if truthy(&subject_id) {
        return Ok(format!("#{}", js_display(Some(&subject_id))));
    }
    Ok("(без субъекта)".to_string())
}

fn label_holder(conn: &Connection, args: &Value) -> Result<String, ToolError> {
    if let Some(label) = args.get("holder_label")
        && truthy(label)
    {
        return Ok(text(label));
    }
    if let Some(character_id) = num(&field(args, "character_id")) {
        if let Some(row) = query_one(
            conn,
            "SELECT name FROM character WHERE id=?",
            &[json!(character_id)],
        )? {
            return Ok(row.get("name").map(text).unwrap_or_default());
        }
    }
    Ok("(без держателя)".to_string())
}

/// `… key ? '/' + key : ''` for the slot part of a fact summary.
fn slot_suffix(value: Option<&Value>) -> String {
    match value {
        Some(value) if truthy(value) => format!("/{}", js_display(Some(value))),
        _ => String::new(),
    }
}

fn tool_activity(
    db: &Db,
    id: ToolId,
    args: &Value,
    result: &Value,
) -> Result<Vec<ActivityEntry>, ToolError> {
    let book_id = num(&field(args, "book_id"));
    let entries = match id {
        ToolId::AddFact => {
            let label = with_conn(db, |conn| label_subject(conn, args))?;
            vec![ActivityEntry {
                book_id,
                summary: format!(
                    "Факт: {label} — [{}{}] {}",
                    js_display(args.get("kind")),
                    slot_suffix(args.get("key")),
                    js_display(args.get("value"))
                ),
                detail: args.clone(),
            }]
        }
        ToolId::CloseFact => vec![ActivityEntry {
            book_id: result.get("book_id").and_then(num),
            summary: format!(
                "Факт закрыт (с гл.{}): [{}{}] {}",
                js_display(args.get("chapter")),
                js_display(result.get("kind")),
                slot_suffix(result.get("key")),
                js_display(result.get("value"))
            ),
            detail: args.clone(),
        }],
        ToolId::AddKnowledge => {
            let holder = with_conn(db, |conn| label_holder(conn, args))?;
            let belief = match args.get("belief") {
                Some(value) if truthy(value) => format!(" ({})", js_display(Some(value))),
                _ => String::new(),
            };
            vec![ActivityEntry {
                book_id,
                summary: format!(
                    "Знание: {holder} — {}{belief}: {}",
                    js_display(args.get("stance")),
                    js_display(args.get("topic"))
                ),
                detail: args.clone(),
            }]
        }
        ToolId::WriteChapterText => {
            let chapter_id = num(&field(args, "chapter_id"));
            let chapter = with_conn(db, |conn| {
                let Some(chapter_id) = chapter_id else {
                    return Ok(None);
                };
                Ok(query_one(
                    conn,
                    "SELECT number, title, book_id FROM chapter WHERE id=?",
                    &[json!(chapter_id)],
                )?)
            })?;
            let number = chapter
                .as_ref()
                .and_then(|row| row.get("number"))
                .map(|value| js_display(Some(value)))
                .unwrap_or_else(|| "?".to_string());
            let title = chapter
                .as_ref()
                .and_then(|row| row.get("title"))
                .filter(|value| truthy(value))
                .map(|value| format!(" «{}»", js_display(Some(value))))
                .unwrap_or_default();
            let note = match result.get("min_chars_note") {
                Some(value) if truthy(value) => format!(" ({})", js_display(Some(value))),
                _ => String::new(),
            };
            vec![ActivityEntry {
                book_id: chapter
                    .as_ref()
                    .and_then(|row| row.get("book_id").and_then(num)),
                summary: format!(
                    "Глава {number}{title} сохранена — {} зн.{note}",
                    js_display(result.get("chars_no_space"))
                ),
                detail: json!({
                    "chapter_id": field(args, "chapter_id"),
                    "words": result.get("words").cloned().unwrap_or(Value::Null),
                }),
            }]
        }
        ToolId::AddPromise => vec![ActivityEntry {
            book_id,
            summary: format!(
                "Новое ружьё: {}{}",
                js_display(result.get("title")),
                match args.get("planted_ch") {
                    Some(value) if truthy(value) => format!(" (гл.{})", js_display(Some(value))),
                    _ => String::new(),
                }
            ),
            detail: result.clone(),
        }],
        ToolId::UpdatePromise => vec![ActivityEntry {
            book_id: result.get("book_id").and_then(num),
            summary: format!(
                "Ружьё «{}» → {}{}",
                js_display(result.get("title")),
                js_display(result.get("status")),
                match result.get("fired_ch") {
                    Some(value) if truthy(value) => format!(" (гл.{})", js_display(Some(value))),
                    _ => String::new(),
                }
            ),
            detail: args.clone(),
        }],
        ToolId::RunImport => result
            .as_array()
            .map(|books| {
                books
                    .iter()
                    .map(|book| ActivityEntry {
                        book_id: book.get("book_id").and_then(num),
                        summary: format!(
                            "Импорт из markdown: {} (гл.: {}{})",
                            js_display(book.get("title")),
                            book.get("stats")
                                .and_then(|stats| stats.get("chapters"))
                                .map(|chapters| js_display(Some(chapters)))
                                .unwrap_or_else(|| "?".to_string()),
                            match book
                                .get("warnings")
                                .and_then(Value::as_array)
                                .map(Vec::len)
                                .unwrap_or(0)
                            {
                                0 => String::new(),
                                count => format!(", ⚠ {count}"),
                            }
                        ),
                        detail: json!({
                            "stats": book.get("stats").cloned().unwrap_or(Value::Null),
                            "warnings": book.get("warnings").cloned().unwrap_or(Value::Null),
                        }),
                    })
                    .collect()
            })
            .unwrap_or_default(),
        ToolId::CreateEntity => vec![ActivityEntry {
            book_id,
            summary: format!(
                "Создана запись {} #{}",
                js_display(args.get("entity_type")),
                js_display(result.get("id"))
            ),
            detail: json!({
                "entity_type": field(args, "entity_type"),
                "id": result.get("id").cloned().unwrap_or(Value::Null),
            }),
        }],
        ToolId::UpdateEntity => vec![ActivityEntry {
            book_id,
            summary: format!(
                "Изменена запись {} #{}",
                js_display(args.get("entity_type")),
                js_display(args.get("id"))
            ),
            detail: json!({
                "entity_type": field(args, "entity_type"),
                "id": field(args, "id"),
                "fields": args
                    .get("changes")
                    .and_then(Value::as_object)
                    .map(|changes| changes.keys().cloned().collect::<Vec<_>>())
                    .unwrap_or_default(),
            }),
        }],
        ToolId::DeleteEntity => vec![ActivityEntry {
            book_id,
            summary: format!(
                "Удалена запись {} #{}",
                js_display(args.get("entity_type")),
                js_display(args.get("id"))
            ),
            detail: json!({
                "entity_type": field(args, "entity_type"),
                "id": field(args, "id"),
            }),
        }],
        _ => Vec::new(),
    };
    Ok(entries)
}

fn log_activity(db: &Db, tool: &str, entries: &[ActivityEntry]) -> Result<(), ToolError> {
    for entry in entries {
        let detail = serde_json::to_string(&entry.detail)?;
        with_conn(db, |conn| {
            json_sql::exec(
                conn,
                "INSERT INTO mcp_activity (book_id, tool, summary, detail) VALUES (?,?,?,?)",
                &[
                    entry.book_id.map_or(Value::Null, |id| json!(id)),
                    json!(tool),
                    json!(entry.summary),
                    json!(detail),
                ],
            )?;
            Ok(())
        })?;
    }
    Ok(())
}

fn log_tool_activity(db: &Db, tool: ToolId, args: &Value, result: &Value) -> Result<(), ToolError> {
    let name = tools()
        .into_iter()
        .find(|def| def.id == tool)
        .map(|def| def.name)
        .unwrap_or_default();
    let entries = tool_activity(db, tool, args, result)?;
    log_activity(db, name, &entries)
}

// ─────────────────────────── resources ───────────────────────────

/// The fixed file set `write-chapter` reads on Step 1.
const DOC_FILES: [(&str, &str); 9] = [
    (
        "book",
        "Паспорт книги — жанр, тон, POV, жёсткий минимум знаков, запреты",
    ),
    (
        "style",
        "Профиль стиля — метрики, приёмы-маркеры (тропы стиля), запреты, калибровочный образец",
    ),
    (
        "voices",
        "Живые голоса персонажей — манера речи, форумные привычки",
    ),
    (
        "world_rules",
        "Правила мира — 🔒 неизменяемое, нарушать нельзя",
    ),
    (
        "glossary",
        "Глоссарий — единственно верные написания имён и терминов",
    ),
    (
        "direction",
        "Якорные точки сюжета — куда движется история",
    ),
    (
        "hidden",
        "Скрытые линии — что готовится за кадром, расписание намёков",
    ),
    (
        "arcs",
        "Арки персонажей — на какой они сейчас стадии",
    ),
    (
        "promises",
        "Ружья текстом — то же, что list_promises, но с пояснениями",
    ),
];

fn doc_rel(key: &str, book: &Value) -> Option<String> {
    match key {
        "book" => Some("book.md".to_string()),
        "style" => book
            .get("active_style")
            .and_then(Value::as_str)
            .filter(|style| !style.is_empty())
            .map(|style| format!("styles/{style}.md")),
        "voices" => Some("bible/voices.md".to_string()),
        "world_rules" => Some("bible/world_rules.md".to_string()),
        "glossary" => Some("bible/glossary.md".to_string()),
        "direction" => Some("plot/direction.md".to_string()),
        "hidden" => Some("plot/hidden.md".to_string()),
        "arcs" => Some("plot/arcs.md".to_string()),
        "promises" => Some("plot/promises.md".to_string()),
        _ => None,
    }
}

fn list_resources(db: &Db) -> Result<Vec<Value>, ToolError> {
    with_conn(db, |conn| {
        let books = query(
            conn,
            "SELECT id, slug, title, root_path, active_style FROM book",
            &[],
        )?;
        let mut resources = Vec::new();
        for book in &books {
            let root = book.get("root_path").and_then(Value::as_str).unwrap_or_default();
            let slug = book.get("slug").and_then(Value::as_str).unwrap_or_default();
            let title = book.get("title").and_then(Value::as_str).unwrap_or_default();
            for (key, doc_title) in DOC_FILES {
                let Some(rel) = doc_rel(key, book) else {
                    continue;
                };
                if !Path::new(root).join(&rel).exists() {
                    continue;
                }
                resources.push(json!({
                    "uri": format!("storyforge://{slug}/{key}"),
                    "name": format!("{title} — {doc_title}"),
                    "description": doc_title,
                    "mimeType": "text/markdown",
                }));
            }
        }
        Ok(resources)
    })
}

fn resource_uri_regex() -> &'static Regex {
    static REGEX: OnceLock<Regex> = OnceLock::new();
    REGEX.get_or_init(|| Regex::new(r"^storyforge://([^/]+)/([^/]+)$").expect("valid regex"))
}

fn read_resource(db: &Db, uri: &str) -> Result<Value, ToolError> {
    let captures = resource_uri_regex().captures(uri).ok_or_else(|| {
        ToolError("Некорректный URI ресурса, ожидается storyforge://<slug>/<ключ>".to_string())
    })?;
    let slug = captures.get(1).map(|m| m.as_str()).unwrap_or_default().to_string();
    let key = captures.get(2).map(|m| m.as_str()).unwrap_or_default().to_string();

    with_conn(db, |conn| {
        let book = query_one(conn, "SELECT * FROM book WHERE slug=?", &[json!(slug)])?
            .ok_or_else(|| ToolError(format!("Книга не найдена: {slug}")))?;
        if !DOC_FILES.iter().any(|(doc_key, _)| *doc_key == key) {
            return Err(ToolError(format!("Неизвестный ресурс: {key}")));
        }
        let rel = doc_rel(&key, &book).ok_or_else(|| ToolError(format!("Файл не найден: {key}")))?;
        let root = book.get("root_path").and_then(Value::as_str).unwrap_or_default();
        let full = Path::new(root).join(&rel);
        if !full.exists() {
            return Err(ToolError(format!("Файл не найден: {rel}")));
        }
        let bytes =
            std::fs::read(&full).map_err(|_| ToolError(format!("Файл не найден: {rel}")))?;
        Ok(json!({
            "uri": uri,
            "mimeType": "text/markdown",
            "text": String::from_utf8_lossy(&bytes).into_owned(),
        }))
    })
}

// ─────────────────────────── prompts ───────────────────────────

/// Verbatim from `mcpCore.js` — the whole methodology, step by step.
const WRITE_CHAPTER_PROMPT: &str = r#"Ты — соавтор книги в Story Forge. Источник истины — база и файлы книги, а не память и не старые главы.

Шаг 1 — контекст:
1. list_books → book_id. list_characters → id участников сцены.
2. get_state({book_id, chapter: N-1, characters}) — состояние мира прямо перед новой главой: кто где, что знает, в чём заблуждается, что при себе, какие ружья ждут. Не выдумывай то, что этому противоречит.
3. resources/list, затем resources/read для storyforge://<slug>/book (жанр, тон, POV, минимум знаков, запреты), .../style (метрики предложений, приёмы-маркеры — держись их в ритме и приёмах, не только в сюжете), .../voices (голос каждого персонажа), .../world_rules (🔒 — нарушать нельзя), .../glossary (только эти написания имён), .../direction и .../hidden (куда движется сюжет, готов ли очередной намёк).

Шаг 2 — план: 2–5 пунктов, что происходит, как двигает к ближайшему якорю, какие ружья/намёки в игре. Покажи автору и дождись подтверждения, если явно не сказано «пиши сразу».

Шаг 3 — текст: следуй профилю стиля не только в фабуле — в длине и ритме предложений, приёмах, запретах. Никто не знает и не говорит того, чего ему не положено знать по get_state. Атмосферный персонаж красит тон сцены по своей карточке, пока он в ней участвует.

Шаг 4 — проверка: перечитай как редактор. Имена — по глоссарию. Статусы не нарушены (мёртвые не действуют). Никто не проговорился раньше срока. Правила мира (🔒) не нарушены. Атмосфера и голос соответствуют карточкам.

Шаг 5 — обновление базы: write_chapter_text сохраняет текст (версия — автоматически). add_fact на каждое изменение состояния — один факт, одно утверждение, key — если у субъекта таких слотов несколько. add_knowledge — когда кто-то узнал/заподозрил/начал заблуждаться (заблуждение ≠ незнание, указывай belief). update_promise на выстрелившие ружья, add_promise на новые.

Не дописывай факты абзацами в markdown-карточки персонажей — это генерируемые файлы (npm run export их перезапишет). Факты и знания идут в базу через add_fact/add_knowledge, не в текст файлов."#;

const WRITE_CHAPTER_DESCRIPTION: &str = "Методология написания главы с контролем целостности — контекст → план → текст → проверка → \
обновление базы. Тот же процесс, что скилл write-chapter, но зашитый в сам сервер: работает у \
любого MCP-клиента, а не только там, где скилл установлен. Вызови перед тем, как писать прозу.";

fn prompts() -> Vec<Value> {
    vec![json!({
        "name": "write_chapter",
        "description": WRITE_CHAPTER_DESCRIPTION,
        "arguments": [],
    })]
}

fn get_prompt(name: Option<&Value>) -> Result<Value, ToolError> {
    if name.and_then(Value::as_str) != Some("write_chapter") {
        return Err(ToolError(format!(
            "Неизвестный промпт: {}",
            js_display(name)
        )));
    }
    Ok(json!({
        "description": "Полный процесс написания главы с проверкой целостности",
        "messages": [{
            "role": "user",
            "content": { "type": "text", "text": WRITE_CHAPTER_PROMPT },
        }],
    }))
}

// ─────────────────────────── dispatch ───────────────────────────

const INSTRUCTIONS: &str = "Инструменты — прямой доступ к базе книги (срез мира, факты, знания, текст глав, ружья). \
Ресурсы (resources/list) — файлы книги: стиль, голоса, правила мира, глоссарий, направление сюжета. \
Перед тем как писать прозу, вызови промпт write_chapter (prompts/get) — это методичка, которая \
проведёт через контекст, план, текст, проверку и запись в базу так же, как это делает скилл \
write-chapter, но без зависимости от того, установлен ли этот скилл у клиента.";

/// Handle one JSON-RPC 2.0 message.
///
/// Notifications (no `id`) produce no reply: the caller answers 202 over HTTP
/// or writes nothing over stdio.
pub fn dispatch(db: &Db, message: &Value) -> DispatchResult {
    let id = message.get("id");
    let Some(method) = message.get("method") else {
        return DispatchResult::none();
    };
    let params = message.get("params").cloned().unwrap_or(Value::Null);

    if let Some(name) = method.as_str() {
        match name {
            "initialize" => {
                let protocol_version = match params.get("protocolVersion") {
                    Some(value) if truthy(value) => value.clone(),
                    _ => json!(DEFAULT_PROTOCOL_VERSION),
                };
                return DispatchResult::ok(
                    id,
                    json!({
                        "protocolVersion": protocol_version,
                        "capabilities": { "tools": {}, "resources": {}, "prompts": {} },
                        "serverInfo": { "name": SERVER_NAME, "version": SERVER_VERSION },
                        "instructions": INSTRUCTIONS,
                    }),
                );
            }
            "notifications/initialized" | "notifications/cancelled" => return DispatchResult::none(),
            "ping" => return DispatchResult::ok(id, json!({})),
            "tools/list" => {
                let list: Vec<Value> = tools()
                    .into_iter()
                    .map(|tool| {
                        json!({
                            "name": tool.name,
                            "description": tool.description,
                            "inputSchema": tool.schema,
                        })
                    })
                    .collect();
                return DispatchResult::ok(id, json!({ "tools": list }));
            }
            "tools/call" => {
                let name_value = params.get("name");
                let found = name_value
                    .and_then(Value::as_str)
                    .and_then(|wanted| tools().into_iter().find(|tool| tool.name == wanted));
                let Some(tool) = found else {
                    return DispatchResult::error(
                        id,
                        -32602,
                        &format!("Неизвестный инструмент: {}", js_display(name_value)),
                    );
                };
                let args = match params.get("arguments") {
                    None | Some(Value::Null) => json!({}),
                    Some(value) => value.clone(),
                };
                return match run_tool(db, tool.id, &args) {
                    Ok(result) => {
                        if has_activity(tool.id)
                            && let Err(err) = log_tool_activity(db, tool.id, &args, &result)
                        {
                            tracing::error!(error = %err, "activity log");
                        }
                        let text = serde_json::to_string_pretty(&result)
                            .unwrap_or_else(|_| "null".to_string());
                        DispatchResult::ok(
                            id,
                            json!({ "content": [{ "type": "text", "text": text }] }),
                        )
                    }
                    Err(err) => {
                        let text = format!("Ошибка: {}", err.0);
                        DispatchResult::ok(
                            id,
                            json!({ "content": [{ "type": "text", "text": text }], "isError": true }),
                        )
                    }
                };
            }
            "resources/list" => {
                return match list_resources(db) {
                    Ok(resources) => DispatchResult::ok(id, json!({ "resources": resources })),
                    Err(err) => DispatchResult::error(id, -32603, &err.0),
                };
            }
            "resources/read" => {
                let uri = string_or_empty(params.get("uri"));
                return match read_resource(db, &uri) {
                    Ok(content) => DispatchResult::ok(id, json!({ "contents": [content] })),
                    Err(err) => DispatchResult::error(id, -32602, &err.0),
                };
            }
            "prompts/list" => return DispatchResult::ok(id, json!({ "prompts": prompts() })),
            "prompts/get" => {
                return match get_prompt(params.get("name")) {
                    Ok(prompt) => DispatchResult::ok(id, prompt),
                    Err(err) => DispatchResult::error(id, -32602, &err.0),
                };
            }
            _ => {}
        }
    }

    if id.is_some() {
        return DispatchResult::error(
            id,
            -32601,
            &format!("Неизвестный метод: {}", js_display(Some(method))),
        );
    }
    DispatchResult::none()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scratch_db(tag: &str) -> Db {
        let dir = std::env::temp_dir().join(format!("storyforge-mcp-{tag}"));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("temp dir");
        Db::open(dir.join("story.db")).expect("scratch db")
    }

    fn call(db: &Db, method: &str, params: Value) -> Value {
        let reply = dispatch(
            db,
            &json!({ "jsonrpc": "2.0", "id": 7, "method": method, "params": params }),
        );
        assert!(reply.has_reply, "{method} должен отвечать");
        reply.payload.expect("payload")
    }

    fn call_tool(db: &Db, name: &str, args: Value) -> String {
        let payload = call(db, "tools/call", json!({ "name": name, "arguments": args }));
        assert!(payload.get("error").is_none(), "{name}: {}", payload["error"]);
        payload["result"]["content"][0]["text"]
            .as_str()
            .unwrap_or_default()
            .to_string()
    }

    fn tool_json(db: &Db, name: &str, args: Value) -> Value {
        serde_json::from_str(&call_tool(db, name, args)).expect("tool json")
    }

    fn seed(db: &Db, tag: &str) -> (i64, i64, i64) {
        let root = std::env::temp_dir().join(format!("storyforge-mcp-{tag}/Book"));
        std::fs::create_dir_all(&root).expect("book root");
        db.with_conn(|conn| -> Result<(i64, i64, i64), SfError> {
            let book = json_sql::insert(
                conn,
                "INSERT INTO book (slug, title, root_path) VALUES (?,?,?)",
                &[json!("kniga"), json!("Книга"), json!(root.to_string_lossy())],
            )?;
            let character = json_sql::insert(
                conn,
                "INSERT INTO character (book_id, name) VALUES (?,?)",
                &[json!(book), json!("Герой")],
            )?;
            let chapter = json_sql::insert(
                conn,
                "INSERT INTO chapter (book_id, number, title) VALUES (?,?,?)",
                &[json!(book), json!(1), json!("Начало")],
            )?;
            Ok((book, character, chapter))
        })
        .expect("seed")
    }

    #[test]
    fn jsonrpc_envelope_matches_the_contract() {
        let db = scratch_db("envelope");

        let ping = call(&db, "ping", json!({}));
        assert_eq!(ping["jsonrpc"], "2.0");
        assert_eq!(ping["id"], 7);
        assert_eq!(ping["result"], json!({}));

        // Notifications produce no reply at all.
        let notification = dispatch(
            &db,
            &json!({ "jsonrpc": "2.0", "method": "initialize", "params": {} }),
        );
        assert!(!notification.has_reply);
        let initialized = dispatch(
            &db,
            &json!({ "jsonrpc": "2.0", "method": "notifications/initialized" }),
        );
        assert!(!initialized.has_reply);

        let init = call(&db, "initialize", json!({ "protocolVersion": "2024-11-05" }));
        assert_eq!(init["result"]["protocolVersion"], "2024-11-05");
        assert_eq!(
            init["result"]["serverInfo"],
            json!({ "name": SERVER_NAME, "version": SERVER_VERSION })
        );
        assert_eq!(
            init["result"]["capabilities"],
            json!({ "tools": {}, "resources": {}, "prompts": {} })
        );
        let default_init = call(&db, "initialize", json!({}));
        assert_eq!(
            default_init["result"]["protocolVersion"],
            DEFAULT_PROTOCOL_VERSION
        );

        let unknown = call(&db, "nope", json!({}));
        assert_eq!(unknown["error"]["code"], -32601);
        assert_eq!(unknown["error"]["message"], "Неизвестный метод: nope");

        let missing_prompt = call(&db, "prompts/get", json!({ "name": "нет" }));
        assert_eq!(missing_prompt["error"]["code"], -32602);
        assert_eq!(
            missing_prompt["error"]["message"],
            "Неизвестный промпт: нет"
        );

        let write = call(&db, "prompts/get", json!({ "name": "write_chapter" }));
        assert_eq!(write["result"]["messages"][0]["role"], "user");
        assert_eq!(write["result"]["messages"][0]["content"]["type"], "text");
        assert_eq!(
            write["result"]["messages"][0]["content"]["text"],
            WRITE_CHAPTER_PROMPT
        );
    }

    #[test]
    fn tool_surface_is_complete() {
        let db = scratch_db("tools");
        let list = call(&db, "tools/list", json!({}));
        let tools = list["result"]["tools"].as_array().expect("tools");
        assert_eq!(tools.len(), 22);
        let names: Vec<&str> = tools
            .iter()
            .filter_map(|tool| tool["name"].as_str())
            .collect();
        for expected in [
            "list_books",
            "list_characters",
            "get_state",
            "get_character_state",
            "search_facts",
            "diff_state",
            "get_dictionaries",
            "add_fact",
            "close_fact",
            "add_knowledge",
            "get_chapter_text",
            "write_chapter_text",
            "list_promises",
            "add_promise",
            "update_promise",
            "run_import",
            "list_entities",
            "get_entity",
            "get_entity_schema",
            "create_entity",
            "update_entity",
            "delete_entity",
        ] {
            assert!(names.contains(&expected), "нет инструмента {expected}");
        }
        for tool in tools {
            assert!(
                tool["description"]
                    .as_str()
                    .is_some_and(|text| !text.is_empty())
            );
            assert_eq!(tool["inputSchema"]["type"], "object");
            assert_eq!(tool["inputSchema"]["additionalProperties"], false);
        }

        let unknown = call(&db, "tools/call", json!({ "name": "nope", "arguments": {} }));
        assert_eq!(unknown["error"]["code"], -32602);
        assert_eq!(unknown["error"]["message"], "Неизвестный инструмент: nope");

        let schema = tool_json(
            &db,
            "get_entity_schema",
            json!({ "entity_type": "character_state" }),
        );
        assert_eq!(schema["required"], json!(["character_id", "text"]));
        assert_eq!(schema["parent"], json!(["character", "character_id"]));
        assert_eq!(schema["update_fields"][0], "chapter_no");

        let chapter = tool_json(&db, "get_entity_schema", json!({ "entity_type": "chapter" }));
        assert_eq!(chapter["required"], json!([]));
        assert_eq!(
            chapter["create_fields"],
            json!(["number", "title", "pov", "summary", "plan"])
        );
        assert_eq!(chapter["parent"], Value::Null);
    }

    #[test]
    fn read_tools_run_against_a_real_database() {
        let db = scratch_db("read");
        let (book, character, chapter) = seed(&db, "read");

        let books = tool_json(&db, "list_books", json!({}));
        assert_eq!(books.as_array().map(Vec::len), Some(1));
        assert_eq!(books[0]["slug"], "kniga");

        let characters = tool_json(&db, "list_characters", json!({ "book_id": book }));
        assert_eq!(characters[0]["name"], "Герой");
        assert_eq!(characters[0]["id"], character);

        let state = tool_json(&db, "get_state", json!({ "book_id": book, "chapter": 0 }));
        assert_eq!(state["characters"][0]["name"], "Герой");
        assert!(state["at"].is_object());

        let slice = tool_json(
            &db,
            "get_character_state",
            json!({ "character_id": character }),
        );
        assert_eq!(slice["character"]["name"], "Герой");

        let dictionaries = tool_json(&db, "get_dictionaries", json!({}));
        assert_eq!(dictionaries["stances"].as_array().map(Vec::len), Some(4));
        assert!(
            dictionaries["fact_kinds"]
                .as_array()
                .is_some_and(|kinds| !kinds.is_empty())
        );

        let found = tool_json(&db, "search_facts", json!({ "book_id": book, "q": "ничего" }));
        assert_eq!(found, json!([]));

        let delta = tool_json(
            &db,
            "diff_state",
            json!({ "book_id": book, "from_chapter": 0, "to_chapter": 1 }),
        );
        assert!(delta["from"].is_object());
        assert!(delta["appeared"].is_array());

        assert_eq!(tool_json(&db, "list_promises", json!({ "book_id": book })), json!([]));

        let text = tool_json(&db, "get_chapter_text", json!({ "chapter_id": chapter }));
        assert_eq!(text["number"], 1);
        assert_eq!(text["body"], "");
        let by_number = tool_json(
            &db,
            "get_chapter_text",
            json!({ "book_id": book, "number": 1 }),
        );
        assert_eq!(by_number["id"], chapter);

        assert_eq!(
            call_tool(&db, "get_chapter_text", json!({ "chapter_id": 999 })),
            "Ошибка: Глава не найдена"
        );

        let resources = call(&db, "resources/list", json!({}));
        assert_eq!(resources["result"]["resources"], json!([]));

        let bad_uri = call(&db, "resources/read", json!({ "uri": "nope" }));
        assert_eq!(bad_uri["error"]["code"], -32602);
        assert_eq!(
            bad_uri["error"]["message"],
            "Некорректный URI ресурса, ожидается storyforge://<slug>/<ключ>"
        );
        let unknown_book = call(&db, "resources/read", json!({ "uri": "storyforge://nope/book" }));
        assert_eq!(unknown_book["error"]["message"], "Книга не найдена: nope");
        let unknown_key = call(&db, "resources/read", json!({ "uri": "storyforge://kniga/nope" }));
        assert_eq!(unknown_key["error"]["message"], "Неизвестный ресурс: nope");
        let no_file = call(&db, "resources/read", json!({ "uri": "storyforge://kniga/book" }));
        assert_eq!(no_file["error"]["message"], "Файл не найден: book.md");

        // A resource appears as soon as the file exists on disk.
        let book_root = std::env::temp_dir().join("storyforge-mcp-read/Book");
        std::fs::write(book_root.join("book.md"), "# Паспорт\n").expect("book.md");
        let listed = call(&db, "resources/list", json!({}));
        let resources = listed["result"]["resources"].as_array().expect("resources");
        assert_eq!(resources.len(), 1);
        assert_eq!(resources[0]["uri"], "storyforge://kniga/book");
        assert_eq!(resources[0]["mimeType"], "text/markdown");
        assert_eq!(resources[0]["name"], "Книга — Паспорт книги — жанр, тон, POV, жёсткий минимум знаков, запреты");
        let read = call(&db, "resources/read", json!({ "uri": "storyforge://kniga/book" }));
        assert_eq!(read["result"]["contents"][0]["text"], "# Паспорт\n");
    }

    #[test]
    fn write_tools_land_in_the_database_and_the_activity_log() {
        let db = scratch_db("write");
        let (book, character, chapter) = seed(&db, "write");

        let fact = tool_json(
            &db,
            "add_fact",
            json!({
                "book_id": book, "subject_type": "character", "subject_id": character,
                "kind": "статус", "value": "жив", "from_chapter": 1,
            }),
        );
        let fact_id = fact["fact"]["id"].as_i64().expect("fact id");
        assert_eq!(fact["fact"]["value"], "жив");
        assert_eq!(fact["fact"]["source"], "ИИ (MCP)");
        assert_eq!(fact["closed"], json!([]));

        let closed = tool_json(&db, "close_fact", json!({ "fact_id": fact_id, "chapter": 1 }));
        assert_eq!(closed["id"], fact_id);
        assert_eq!(closed["to_chapter"], 1);

        let knowledge = tool_json(
            &db,
            "add_knowledge",
            json!({
                "book_id": book, "character_id": character, "topic": "тайна",
                "stance": "знает", "from_chapter": 1,
            }),
        );
        assert_eq!(knowledge["knowledge"]["stance"], "знает");
        assert_eq!(knowledge["knowledge"]["source"], "ИИ (MCP)");

        let promise = tool_json(
            &db,
            "add_promise",
            json!({ "book_id": book, "title": "ружьё", "planted_ch": 1 }),
        );
        assert_eq!(promise["status"], "ожидает");
        assert_eq!(promise["planted_ch"], 1);
        let promise_id = promise["id"].as_i64().expect("promise id");
        let fired = tool_json(
            &db,
            "update_promise",
            json!({ "promise_id": promise_id, "status": "выстрелило", "fired_ch": 1 }),
        );
        assert_eq!(fired["status"], "выстрелило");
        assert_eq!(fired["fired_ch"], 1);

        let saved = tool_json(
            &db,
            "write_chapter_text",
            json!({ "chapter_id": chapter, "body": "Привет, мир." }),
        );
        assert_eq!(saved["ok"], true);
        assert_eq!(saved["chars_no_space"], 11);
        assert_eq!(saved["min_chars_note"], "меньше минимума площадки");
        let path = std::env::temp_dir().join("storyforge-mcp-write/Book/chapters/ch001.md");
        assert_eq!(
            std::fs::read_to_string(&path).expect("chapter file"),
            "Привет, мир."
        );
        let (status, revision) = db
            .with_conn(|conn| -> Result<(String, String), SfError> {
                let row = query_one(conn, "SELECT status FROM chapter WHERE id=?", &[json!(chapter)])?
                    .expect("chapter row");
                Ok((text(&row["status"]), text(&saved["revision"])))
            })
            .expect("chapter row");
        assert_eq!(status, "написано");
        assert_eq!(revision.len(), 64);

        let created = tool_json(
            &db,
            "create_entity",
            json!({ "book_id": book, "entity_type": "character", "data": { "name": "Второй" } }),
        );
        let created_id = created["id"].as_i64().expect("created id");
        assert_eq!(created["name"], "Второй");
        let updated = tool_json(
            &db,
            "update_entity",
            json!({
                "book_id": book, "entity_type": "character", "id": created_id,
                "changes": { "status": "жив" },
            }),
        );
        assert_eq!(updated["status"], "жив");
        let fresh_chapter = tool_json(
            &db,
            "create_entity",
            json!({ "book_id": book, "entity_type": "chapter", "data": {} }),
        );
        assert_eq!(fresh_chapter["number"], 2);
        assert_eq!(fresh_chapter["title"], "Глава 2");
        assert_eq!(fresh_chapter["status"], "план");

        let deleted = tool_json(
            &db,
            "delete_entity",
            json!({ "book_id": book, "entity_type": "character", "id": created_id }),
        );
        assert_eq!(deleted, json!({ "deleted": true, "entity_type": "character", "id": created_id }));

        let activity = db
            .with_conn(|conn| -> Result<Vec<Value>, SfError> {
                Ok(query(conn, "SELECT tool, summary, detail FROM mcp_activity ORDER BY id", &[])?)
            })
            .expect("activity");
        let names: Vec<&str> = activity
            .iter()
            .filter_map(|row| row["tool"].as_str())
            .collect();
        for expected in [
            "add_fact",
            "close_fact",
            "add_knowledge",
            "add_promise",
            "update_promise",
            "write_chapter_text",
            "create_entity",
            "update_entity",
            "delete_entity",
        ] {
            assert!(names.contains(&expected), "нет записи активности {expected}");
        }
        assert!(activity.iter().any(|row| row["summary"]
            .as_str()
            .is_some_and(|summary| summary == "Факт: Герой — [статус] жив")));
        assert!(activity.iter().any(|row| row["summary"]
            .as_str()
            .is_some_and(|summary| summary == "Глава 1 «Начало» сохранена — 11 зн. (меньше минимума площадки)")));
        assert!(activity.iter().any(|row| row["detail"]
            .as_str()
            .is_some_and(|detail| detail.contains("\"subject_type\""))));
    }

    #[test]
    fn entity_tools_validate_book_and_fields() {
        let db = scratch_db("validate");
        let (book, _character, chapter) = seed(&db, "validate");
        let (other, other_chapter) = db
            .with_conn(|conn| -> Result<(i64, i64), SfError> {
                let other = json_sql::insert(
                    conn,
                    "INSERT INTO book (slug, title, root_path) VALUES (?,?,?)",
                    &[
                        json!("drugaya"),
                        json!("Другая"),
                        json!(std::env::temp_dir().to_string_lossy()),
                    ],
                )?;
                let other_chapter = json_sql::insert(
                    conn,
                    "INSERT INTO chapter (book_id, number, title) VALUES (?,?,?)",
                    &[json!(other), json!(1), json!("Чужая")],
                )?;
                Ok((other, other_chapter))
            })
            .expect("other book");

        assert_eq!(
            call_tool(
                &db,
                "create_entity",
                json!({ "book_id": 999, "entity_type": "character", "data": { "name": "X" } })
            ),
            "Ошибка: Книга не найдена"
        );
        assert_eq!(
            call_tool(
                &db,
                "create_entity",
                json!({ "book_id": book, "entity_type": "character", "data": { "nope": 1 } })
            ),
            "Ошибка: Поле nope нельзя изменять у character"
        );
        assert_eq!(
            call_tool(
                &db,
                "create_entity",
                json!({ "book_id": book, "entity_type": "character", "data": {} })
            ),
            "Ошибка: Для character нужно поле name"
        );
        assert_eq!(
            call_tool(
                &db,
                "create_entity",
                json!({ "book_id": book, "entity_type": "nope", "data": {} })
            ),
            "Ошибка: Неизвестный тип записи"
        );
        assert_eq!(
            call_tool(
                &db,
                "create_entity",
                json!({
                    "book_id": book, "entity_type": "event",
                    "data": { "title": "Событие", "chapter_id": other_chapter },
                })
            ),
            format!("Ошибка: Связанная запись chapter #{other_chapter} не принадлежит выбранной книге")
        );
        assert_eq!(
            call_tool(&db, "get_entity", json!({ "book_id": book, "entity_type": "character", "id": 999 })),
            "Ошибка: Запись не найдена"
        );
        assert_eq!(
            call_tool(
                &db,
                "delete_entity",
                json!({ "book_id": book, "entity_type": "chapter", "id": chapter })
            ),
            "Ошибка: Главу удаляйте через редактор после сохранения копии; инструмент агента не удаляет главы"
        );


        let _ = other;
    }
}
//! Export the database back to markdown snapshots.
//!
//! Port of `server/tools/export.js`, limited to the three generated files:
//!
//! * `bible/characters/<name>.md` — one card per character,
//! * `state/current.md` — the "now" slice,
//! * `timeline/events.md` — the ordered timeline.
//!
//! Everything else in a book folder is authored markdown and is never written
//! or removed here. Files are overwritten in place; directories are created as
//! needed. Every generated file carries [`GENERATED_HEADER`] so a reader can
//! tell a snapshot from a source.
//!
//! The JavaScript edition exposed this as `npm run export` only — there is no
//! HTTP route for it in `server/index.js` — so this module is the CLI-free
//! replacement: call [`export_book`] or [`export_all`] from the shell, a Tauri
//! command or a future `POST /api/books/{id}/export`.
//!
//! # Rendering rules kept from `export.js`
//!
//! * The slice is resolved once per book (`resolveMoment` with no options) and
//!   every character card is rendered "as of" that moment.
//! * `characterState(..., {full:true, events:500})` feeds the dossier, the
//!   awareness block and the event list; `worldState(..., {events:0})` feeds
//!   the "now" slice.
//! * List values are stored as newline-separated rows; rows that already start
//!   with `- ` are not prefixed twice.
//! * An empty value is omitted, but an explicitly empty string is still
//!   written where `export.js` wrote it (`state ?? ''`).
//! * Object keys and array order come from the core slice helpers, which keep
//!   the JavaScript array order.

use std::fs;
use std::path::{Path, PathBuf};

use rusqlite::Connection;
use serde_json::Value;

use crate::db::Db;
use crate::error::{Result, SfError};
use crate::json_sql::{num, query, query_one};
use crate::state::{
    CharacterStateOpts, WorldStateOpts, character_state, resolve_moment, world_state,
};

/// Exact marker written into every generated file.
pub const GENERATED_HEADER: &str =
    "<!-- Сгенерировано из базы: npm run export. Правки вносите в приложении. -->";

/// Files written for one book.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExportOutcome {
    pub book_id: i64,
    pub title: String,
    pub files: usize,
}

/// Export one book by id.
pub fn export_book(db: &Db, book_id: i64) -> Result<ExportOutcome> {
    db.with_conn(|conn| export_on(conn, book_id))
}

/// Export every book, skipping (with a warning) a book whose folder is gone.
pub fn export_all(db: &Db) -> Result<Vec<ExportOutcome>> {
    db.with_conn(|conn| {
        let books = query(conn, "SELECT * FROM book ORDER BY id", &[])?;
        let mut outcomes = Vec::new();
        for book in &books {
            let book_id = book.get("id").and_then(num).unwrap_or(0);
            match export_on(conn, book_id) {
                Ok(outcome) => outcomes.push(outcome),
                // `export.js` logged "✗ папка книги не найдена" and continued.
                Err(SfError::NotFound(message)) => {
                    tracing::warn!(book_id, %message, "export skipped");
                }
                Err(error) => return Err(error),
            }
        }
        Ok(outcomes)
    })
}

fn export_on(conn: &Connection, book_id: i64) -> Result<ExportOutcome> {
    let Some(book) = query_one(conn, "SELECT * FROM book WHERE id=?", &[serde_json::json!(book_id)])?
    else {
        return Err(SfError::NotFound("Книга не найдена".into()));
    };
    let title = book
        .get("title")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string();
    let root = book
        .get("root_path")
        .and_then(Value::as_str)
        .unwrap_or_default();
    if root.is_empty() || !Path::new(root).exists() {
        return Err(SfError::NotFound(format!("папка книги не найдена: {root}")));
    }
    let root = PathBuf::from(root);

    let at = resolve_moment(conn, book_id, None, None)?;
    let chapters = query(
        conn,
        "SELECT * FROM chapter WHERE book_id=? ORDER BY number",
        &[serde_json::json!(book_id)],
    )?;

    let mut files = 0usize;

    // ─────────── Character cards ───────────
    for character in query(
        conn,
        "SELECT * FROM character WHERE book_id=? ORDER BY name",
        &[serde_json::json!(book_id)],
    )? {
        let Some(character_id) = character.get("id").and_then(num) else {
            continue;
        };
        let Some(state) = character_state(
            conn,
            character_id,
            &at,
            &CharacterStateOpts {
                full: true,
                events: Some(500.0),
            },
        )?
        else {
            continue;
        };
        let name = character
            .get("name")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string();
        let card = render_character(&character, &state, &at);
        write_file(
            &root
                .join("bible")
                .join("characters")
                .join(format!("{name}.md")),
            &card,
        )?;
        files += 1;
    }

    // ─────────── "Now" slice ───────────
    let world = world_state(
        conn,
        book_id,
        &at,
        &WorldStateOpts {
            events: Some(0.0),
            ..WorldStateOpts::default()
        },
    )?;
    write_file(&root.join("state").join("current.md"), &render_current(&world, &chapters, &at))?;
    files += 1;

    // ─────────── Timeline ───────────
    let entries = query(
        conn,
        "SELECT t.*, m.label FROM timeline_entry t
         LEFT JOIN moment m ON m.id = t.moment_id
         WHERE t.book_id=? ORDER BY t.ord",
        &[serde_json::json!(book_id)],
    )?;
    write_file(&root.join("timeline").join("events.md"), &render_timeline(&entries))?;
    files += 1;

    Ok(ExportOutcome {
        book_id,
        title,
        files,
    })
}

// ─────────────────────────── rendering ───────────────────────────

fn render_character(character: &Value, state: &Value, at: &Value) -> String {
    let name = character
        .get("name")
        .map(display)
        .unwrap_or_default();
    let mut lines: Vec<String> = Vec::new();

    lines.push(format!("# {name}"));
    lines.push(String::new());
    lines.push(GENERATED_HEADER.to_string());
    lines.push(String::new());

    lines.push("---".to_string());
    let alias = character.get("aliases").filter(|value| truthy(value));
    lines.push(format!(
        "имя: {}",
        alias.map(display).unwrap_or_else(|| name.clone())
    ));
    for (key, label) in [
        ("status", "статус"),
        ("age", "возраст"),
        ("gender", "пол"),
        ("group_name", "группа"),
    ] {
        if let Some(value) = character.get(key).filter(|value| truthy(value)) {
            lines.push(format!("{label}: {}", display(value)));
        }
    }
    lines.push("---".to_string());
    lines.push(String::new());

    let summary = character.get("summary").filter(|value| truthy(value));
    let history = character
        .get("character_history")
        .filter(|value| truthy(value));
    if summary.is_some() || history.is_some() {
        lines.push("## Характер".to_string());
        lines.push(String::new());
        if let Some(summary) = summary {
            lines.push(format!("**Текущий:** {}", display(summary)));
            lines.push(String::new());
        }
        if let Some(history) = history {
            lines.push(format!("**История изменений:** {}", display(history)));
            lines.push(String::new());
        }
    }

    let atmosphere_desc = character.get("atmosphere_desc");
    let atmosphere_level = character.get("atmosphere_level");
    if atmosphere_desc.is_some_and(truthy) || atmosphere_level.is_some_and(truthy) {
        lines.push("## Атмосфера".to_string());
        lines.push(String::new());
        lines.push(format!(
            "- **Описание:** {}",
            nullish(atmosphere_desc, "нет")
        ));
        lines.push(format!(
            "- **Влияние:** {}/5",
            nullish(atmosphere_level, "0")
        ));
        lines.push(String::new());
    }

    let voice = character.get("voice").filter(|value| truthy(value));
    let voice_rules = character.get("voice_rules").filter(|value| truthy(value));
    if voice.is_some() || voice_rules.is_some() {
        lines.push("## Голос (речь)".to_string());
        lines.push(String::new());
        if let Some(voice) = voice {
            lines.extend(as_bullets(&display(voice)));
        }
        if let Some(rules) = voice_rules {
            lines.extend(as_bullets(&display(rules)));
        }
        lines.push(String::new());
    }

    for (heading, value) in [
        ("Внешность", character.get("appearance")),
        ("Цели", character.get("goals")),
        ("Взгляды и убеждения", character.get("beliefs")),
        ("Знает / Не знает", character.get("knows")),
        ("Прочее", character.get("notes")),
    ] {
        if let Some(value) = value.filter(|value| truthy(value)) {
            lines.push(format!("## {heading}"));
            lines.push(String::new());
            lines.push(display(value));
            lines.push(String::new());
        }
    }

    // Dossier from facts, laid out by dictionary branch.
    let branches = state
        .get("branches")
        .and_then(Value::as_array)
        .map(Vec::as_slice)
        .unwrap_or(&[]);
    if !branches.is_empty() {
        lines.push("## Досье".to_string());
        lines.push(String::new());
        lines.push(format!("_на момент: {}_", nullish(at.get("label"), "")));
        lines.push(String::new());

        for branch in branches {
            if branch.get("code").and_then(Value::as_str) == Some("сюжет") {
                continue;
            }
            lines.push(format!("**{}**", branch.get("title").map(display).unwrap_or_default()));
            lines.push(String::new());
            let kinds = branch
                .get("kinds")
                .and_then(Value::as_array)
                .map(Vec::as_slice)
                .unwrap_or(&[]);
            for kind in kinds {
                let kind_title = kind.get("title").map(display).unwrap_or_default();
                let items = kind
                    .get("items")
                    .and_then(Value::as_array)
                    .map(Vec::as_slice)
                    .unwrap_or(&[]);
                for fact in items {
                    let slot = fact
                        .get("key")
                        .filter(|value| truthy(value))
                        .map(|value| format!(" · {}", display(value)))
                        .unwrap_or_default();
                    let when = if fact.get("from_chapter").is_some_and(truthy) {
                        let mut text = format!(" _(гл.{}", display(fact.get("from_chapter").unwrap()));
                        if fact.get("to_chapter").is_some_and(truthy) {
                            text.push('–');
                            text.push_str(&display(fact.get("to_chapter").unwrap()));
                        }
                        text.push_str(")_");
                        text
                    } else {
                        String::new()
                    };
                    lines.push(format!(
                        "- {kind_title}{slot}: {}{when}",
                        display(fact.get("value").unwrap_or(&Value::Null))
                    ));
                }
            }
            lines.push(String::new());
        }
    }

    // Awareness.
    let awareness = state.get("awareness");
    let stance_rows: Vec<(&str, &Value)> = [
        ("знает", "knows"),
        ("подозревает", "suspects"),
        ("заблуждается", "misbeliefs"),
        ("не знает", "unaware"),
    ]
    .into_iter()
    .filter_map(|(label, key)| {
        let items = awareness?.get(key)?.as_array()?;
        if items.is_empty() {
            None
        } else {
            Some((label, awareness?.get(key)?))
        }
    })
    .collect();
    if !stance_rows.is_empty() {
        lines.push("## Осведомлённость".to_string());
        lines.push(String::new());
        for (label, items) in stance_rows {
            for item in items.as_array().map(Vec::as_slice).unwrap_or(&[]) {
                let subject = item
                    .get("secret_title")
                    .filter(|value| !value.is_null())
                    .or_else(|| item.get("topic").filter(|value| !value.is_null()))
                    .map(display)
                    .unwrap_or_else(|| "—".to_string());
                let belief = item
                    .get("belief")
                    .filter(|value| truthy(value))
                    .map(|value| format!(" — верит: {}", display(value)))
                    .unwrap_or_default();
                let when = item
                    .get("from_chapter")
                    .filter(|value| truthy(value))
                    .map(|value| format!(" _(с гл.{})_", display(value)))
                    .unwrap_or_default();
                lines.push(format!("- **{label}**: {subject}{belief}{when}"));
            }
        }
        lines.push(String::new());
    }

    // Relationships.
    let relationships: Vec<&Value> = state
        .get("relationships")
        .and_then(Value::as_array)
        .map(Vec::as_slice)
        .unwrap_or(&[])
        .iter()
        .filter(|row| row.get("from_id").and_then(num) == Some(character_id_of(character)))
        .collect();
    if !relationships.is_empty() {
        lines.push("## Отношения".to_string());
        lines.push(String::new());
        lines.push("| С кем | Текущее |".to_string());
        lines.push("|---|---|".to_string());
        for relationship in relationships {
            lines.push(format!(
                "| {} | {} |",
                relationship.get("to_name").map(display).unwrap_or_default(),
                nullish(relationship.get("state"), "")
            ));
        }
        lines.push(String::new());
    }

    // Events by chapter.
    let events: Vec<Value> = state
        .get("facts")
        .and_then(|facts| facts.get("событие"))
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    if !events.is_empty() {
        let mut events = events;
        events.sort_by_key(|fact| fact.get("from_chapter").and_then(num).unwrap_or(0));
        lines.push("## Ключевые события".to_string());
        lines.push(String::new());
        for event in events {
            let chapter = if event.get("from_chapter").is_some_and(truthy) {
                format!("гл.{}: ", display(event.get("from_chapter").unwrap()))
            } else {
                String::new()
            };
            lines.push(format!(
                "- {chapter}{}",
                display(event.get("value").unwrap_or(&Value::Null))
            ));
        }
        lines.push(String::new());
    }

    lines.join("\n")
}

fn render_current(world: &Value, chapters: &[Value], at: &Value) -> String {
    let mut lines: Vec<String> = Vec::new();
    lines.push("# Срез \"СЕЙЧАС\"".to_string());
    lines.push(String::new());
    lines.push(GENERATED_HEADER.to_string());
    lines.push(String::new());

    lines.push(format!("**Момент:** {}", nullish(at.get("label"), "")));
    let last_written = chapters
        .iter()
        .filter(|chapter| chapter.get("status").and_then(Value::as_str) == Some("написано"))
        .next_back();
    if let Some(chapter) = last_written {
        lines.push(format!(
            "**Последняя глава:** {}. {}",
            display(chapter.get("number").unwrap_or(&Value::Null)),
            nullish(chapter.get("title"), "")
        ));
    }
    lines.push(String::new());

    lines.push("## Где кто находится".to_string());
    lines.push(String::new());
    lines.push("| Персонаж | Где | Состояние |".to_string());
    lines.push("|---|---|---|".to_string());
    for character in world
        .get("characters")
        .and_then(Value::as_array)
        .map(Vec::as_slice)
        .unwrap_or(&[])
    {
        let facts = character
            .get("facts")
            .and_then(Value::as_array)
            .map(Vec::as_slice)
            .unwrap_or(&[]);
        let place = facts
            .iter()
            .find(|fact| fact.get("kind").and_then(Value::as_str) == Some("место"))
            .map(|fact| display(fact.get("value").unwrap_or(&Value::Null)))
            .unwrap_or_else(|| "—".to_string());
        let condition = facts
            .iter()
            .find(|fact| {
                matches!(
                    fact.get("kind").and_then(Value::as_str),
                    Some("статус") | Some("эмоция")
                )
            })
            .map(|fact| display(fact.get("value").unwrap_or(&Value::Null)))
            .unwrap_or_default();
        lines.push(format!(
            "| {} | {place} | {condition} |",
            character.get("name").map(display).unwrap_or_default()
        ));
    }
    lines.push(String::new());

    let plot = world
        .get("plot")
        .and_then(Value::as_array)
        .map(Vec::as_slice)
        .unwrap_or(&[]);
    if !plot.is_empty() {
        lines.push("## Незакрытое".to_string());
        lines.push(String::new());
        for fact in plot {
            lines.push(format!(
                "- {}",
                display(fact.get("value").unwrap_or(&Value::Null))
            ));
        }
        lines.push(String::new());
    }

    let promises = world
        .get("open_promises")
        .and_then(Value::as_array)
        .map(Vec::as_slice)
        .unwrap_or(&[]);
    if !promises.is_empty() {
        lines.push("## Ружья в ожидании".to_string());
        lines.push(String::new());
        for promise in promises {
            let planted = if promise.get("planted_ch").is_some_and(truthy) {
                format!(
                    " _(посеяно гл.{})_",
                    display(promise.get("planted_ch").unwrap())
                )
            } else {
                String::new()
            };
            lines.push(format!(
                "- {}{planted} — {}",
                promise.get("title").map(display).unwrap_or_default(),
                display(promise.get("status").unwrap_or(&Value::Null))
            ));
        }
        lines.push(String::new());
    }

    lines.join("\n")
}

fn render_timeline(entries: &[Value]) -> String {
    let mut lines: Vec<String> = Vec::new();
    lines.push("# Хронология событий".to_string());
    lines.push(String::new());
    lines.push(GENERATED_HEADER.to_string());
    lines.push(String::new());
    lines.push("| Когда | Глава | Событие | Кто затронут |".to_string());
    lines.push("|---|---|---|---|".to_string());

    for entry in entries {
        // `label ?? story_date ?? ''`, then `chapter_no ?? ''`, `who ?? ''`.
        let when = entry
            .get("label")
            .filter(|value| !value.is_null())
            .or_else(|| entry.get("story_date").filter(|value| !value.is_null()))
            .map(display)
            .unwrap_or_default();
        let chapter = entry
            .get("chapter_no")
            .filter(|value| !value.is_null())
            .map(display)
            .unwrap_or_default();
        lines.push(format!(
            "| {when} | {chapter} | {} | {} |",
            display(entry.get("description").unwrap_or(&Value::Null)),
            entry
                .get("who")
                .filter(|value| !value.is_null())
                .map(display)
                .unwrap_or_default()
        ));
    }

    lines.join("\n") + "\n"
}

// ─────────────────────────── helpers ───────────────────────────

fn character_id_of(character: &Value) -> i64 {
    character.get("id").and_then(num).unwrap_or(0)
}

/// `String(value)` for a JSON scalar; `null` renders as an empty string.
fn display(value: &Value) -> String {
    match value {
        Value::Null => String::new(),
        Value::String(text) => text.clone(),
        other => other.to_string(),
    }
}

/// `value ?? default`.
fn nullish(value: Option<&Value>, default: &str) -> String {
    match value {
        None | Some(Value::Null) => default.to_string(),
        Some(value) => display(value),
    }
}

fn truthy(value: &Value) -> bool {
    match value {
        Value::Null => false,
        Value::Bool(flag) => *flag,
        Value::Number(number) => number.as_f64().is_some_and(|value| value != 0.0),
        Value::String(text) => !text.is_empty(),
        _ => true,
    }
}

/// Rows may already be list items; do not add a second dash.
fn as_bullets(text: &str) -> Vec<String> {
    text.split('\n')
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .map(|line| {
            if line.starts_with("- ") {
                line.to_string()
            } else {
                format!("- {line}")
            }
        })
        .collect()
}

/// `writeFileSync(path, text, 'utf8')` after `mkdirSync(dirname, recursive)`.
fn write_file(path: &Path, text: &str) -> Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).map_err(|error| SfError::io(parent, error))?;
    }
    fs::write(path, text.as_bytes()).map_err(|error| SfError::io(path, error))
}

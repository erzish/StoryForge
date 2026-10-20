//! Markdown bible import: rebuild one book from `book.md`, `chapters/chNNN.md`
//! and the `bible/`, `plot/`, `state/`, `timeline/` folders.
//!
//! Ported from `server/import/importBook.js`, `importAll.js`, `preserve.js`,
//! `timeline.js` and `server/lib/md.js` of the JavaScript edition. The ordered
//! rebuild (book → arcs → characters → cards → relationships → chapters →
//! summaries → events → hidden lines → secrets → promises → world rules →
//! glossary → locations → timeline → `state/current.md` → docs → moments and
//! facts → restore of user data) is the same sequence, with the same SQL and
//! the same argument coercions.
//!
//! # One transaction, cascading delete
//!
//! The book row is deleted and re-inserted, so every table with
//! `ON DELETE CASCADE` is rebuilt. The import therefore protects what markdown
//! never contained — facts, knowledge, notes, versions, graph layout, activity
//! — by snapshotting it before the delete and restoring it afterwards, matching
//! rows by *business keys* (character name, chapter number, event `ext_id`,
//! secret title, old→new node id) rather than by ids, which change.
//!
//! # The known `preserve.js` bug, reproduced
//!
//! `captureUserData` reads `relationship_state` rows with a NULL `note` under
//! `relStates`, and `restoreUserData` never writes them back. User-authored
//! interval states are silently lost on re-import. That is the JavaScript
//! behaviour and it is kept here on purpose; see [`restore_user_data`].
//!
//! # Fidelity notes
//!
//! * Warning strings are copied verbatim, including the `«»` quotes.
//! * Markdown parsing reproduces `lib/md.js`: `##`…`####` sections end at the
//!   next heading of the same or a shallower level, tables keep their header
//!   order (so `glossary` joins the *remaining columns*, not alphabetical
//!   ones), bullets merge continuation lines, `гл.N` is the only chapter
//!   reference syntax.
//! * `stats` keeps the JavaScript key set. `Object.assign(stats, migrateFacts)`
//!   overwrites the relationship count with the number of `relationship_state`
//!   rows, so `stats.relationships` means "history rows", not "relationship
//!   rows"; the value is normally equal, the meaning is not.
//! * A missing `app/data.json` field and an empty string are different values
//!   (`null` vs `''`) exactly where JavaScript distinguished them; `??` and
//!   `||` are reproduced separately.
//! * Deliberate small deviations: directory listings are sorted, so warning
//!   order and character id assignment are deterministic (Node relied on the
//!   OS directory order); `serde_json` object iteration is alphabetical, so
//!   `data.json`'s `chars` registry is walked in key order rather than
//!   insertion order; `BEGIN IMMEDIATE` becomes `Db::with_tx`'s deferred
//!   transaction, which is equivalent under the single-writer mutex.

use std::collections::{HashMap, HashSet};
use std::fs;
use std::path::{Path, PathBuf};

use regex::Regex;
use rusqlite::Connection;
use serde_json::{Map, Value, json};

use crate::chapter_text::{chars_no_space, index_book_chapters, word_count};
use crate::db::Db;
use crate::error::{Result, SfError};
use crate::json_sql::{exec, insert, num, query, query_one, scalar_i64, text, truthy};
use crate::state::{moment_of_chapter, normalize_kind, normalize_stance};

/// Result of one book import; serialises as the route's response body.
#[derive(Debug, Clone)]
pub struct ImportOutcome {
    pub book_id: i64,
    pub title: String,
    /// Counters in the JavaScript edition's key set.
    pub stats: Map<String, Value>,
    pub warnings: Vec<String>,
}

impl ImportOutcome {
    /// `{bookId, title, stats, warnings}` as `POST /api/import` answered it.
    pub fn to_json(&self) -> Value {
        let mut object = Map::new();
        object.insert("bookId".into(), json!(self.book_id));
        object.insert("title".into(), json!(self.title));
        object.insert("stats".into(), Value::Object(self.stats.clone()));
        object.insert("warnings".into(), json!(self.warnings));
        Value::Object(object)
    }
}

/// Import (or re-import) one book folder in a single transaction.
///
/// Any error rolls the transaction back, leaving the previous book row and all
/// user data untouched.
pub fn import_book(db: &Db, book_dir: &Path) -> Result<ImportOutcome> {
    db.with_tx(|conn| import_on(conn, book_dir))
}

/// Every directory directly under `root` that contains `book.md`.
///
/// The JavaScript edition read one directory level, so a nested library layout
/// is not discovered. Sorted for a stable import order.
pub fn find_books(root: &Path) -> Vec<PathBuf> {
    let Ok(entries) = fs::read_dir(root) else {
        return Vec::new();
    };
    let mut books: Vec<PathBuf> = entries
        .filter_map(std::result::Result::ok)
        .map(|entry| entry.path())
        .filter(|path| path.is_dir() && path.join("book.md").exists())
        .collect();
    books.sort();
    books
}

/// Import every book under `root`, stopping at the first failure.
///
/// The JavaScript `importAll` rethrows, so earlier books stay committed while
/// the request fails; the same is true here.
pub fn import_all(db: &Db, root: &Path) -> Result<Vec<ImportOutcome>> {
    let mut results = Vec::new();
    for dir in find_books(root) {
        results.push(import_book(db, &dir)?);
    }
    Ok(results)
}

/// Folder scanned by `POST /api/import-all`.
///
/// The JavaScript route scanned `resolve(APP_ROOT, '..')`, i.e. the folder that
/// contains the application directory. A desktop build has no such checkout, so
/// the configured `library_root` wins, then the working directory, then the
/// executable's folder.
pub fn default_library_root() -> PathBuf {
    let config = crate::paths::load_config();
    if let Some(root) = config.library_root.filter(|root| !root.trim().is_empty()) {
        return PathBuf::from(root);
    }
    if let Ok(cwd) = std::env::current_dir() {
        return cwd;
    }
    std::env::current_exe()
        .ok()
        .and_then(|exe| exe.parent().map(Path::to_path_buf))
        .unwrap_or_else(|| PathBuf::from("."))
}

// ─────────────────────────── the rebuild ───────────────────────────

fn import_on(conn: &Connection, book_dir: &Path) -> Result<ImportOutcome> {
    let mut stats: Map<String, Value> = Map::new();
    let mut warnings: Vec<String> = Vec::new();

    let slug = book_dir
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_default();

    // ─────────────── 1. Book ───────────────
    let mut title = slug.clone();
    let mut meta = Value::Object(Map::new());
    let data_path = book_dir.join("app").join("data.json");
    if data_path.exists() {
        let parsed = fs::read(&data_path)
            .map(|bytes| String::from_utf8_lossy(&bytes).into_owned())
            .map_err(|error| error.to_string())
            .and_then(|body| {
                serde_json::from_str::<Value>(&body).map_err(|error| error.to_string())
            });
        match parsed {
            Ok(value) => {
                if let Some(meta_title) = value
                    .pointer("/meta/title")
                    .and_then(Value::as_str)
                    .filter(|title| !title.is_empty())
                {
                    title = meta_title.to_string();
                }
                meta = if value.is_null() {
                    Value::Object(Map::new())
                } else {
                    value
                };
            }
            Err(message) => {
                warnings.push(format!("app/data.json не разобран: {message}"));
            }
        }
    }
    let has_meta_title = meta
        .pointer("/meta/title")
        .and_then(Value::as_str)
        .is_some_and(|value| !value.is_empty());

    let mut fields = BookFields::default();
    let book_md_path = book_dir.join("book.md");
    if let Some(body) = read_opt(&book_md_path)? {
        if !has_meta_title {
            if let Some(value) = labelled(&body, r"Название \(рабочее\)") {
                if !value.is_empty() {
                    title = value;
                }
            }
        }
        let sections = sections(&body);
        fields.genre = fm_labelled(&sections, "Основное", "Жанр");
        fields.pov = fm_labelled(&sections, "Стиль и тон", "POV");
        fields.tone = fm_labelled(&sections, "Стиль и тон", "Тон");
        fields.premise = section_or_null(&sections, "Премисса");
        if let Some(profile) = labelled(section_text(&sections, "Стиль и тон"), "Профиль стиля") {
            if !profile.is_empty() {
                let without_prefix = profile.strip_prefix("styles/").unwrap_or(&profile);
                let name = without_prefix
                    .strip_suffix(".md")
                    .unwrap_or(without_prefix);
                fields.style = json!(name);
            }
        }
        let volume = labelled(section_text(&sections, "Основное"), "Целевой объём").unwrap_or_default();
        if let Some(capture) = Regex::new(r"≥\s*([0-9\s]+)\s*знак")
            .expect("volume regex")
            .captures(&volume)
        {
            let digits: String = capture[1].chars().filter(|c| !js_is_space(*c)).collect();
            fields.min_chars = digits.parse::<i64>().unwrap_or(0);
        }
    }
    if let Some(style) = meta
        .pointer("/meta/activeStyle")
        .and_then(Value::as_str)
        .filter(|style| !style.is_empty())
    {
        fields.style = json!(style);
    }

    // What re-import must not destroy is captured *before* the cascading delete.
    let preserved = capture_user_data(conn, &slug)?;

    if let Some(old) = query_one(conn, "SELECT id FROM book WHERE slug=?", &[json!(slug)])? {
        if let Some(old_id) = old.get("id").and_then(num) {
            exec(
                conn,
                "DELETE FROM chapter_search WHERE book_id=?",
                &[json!(old_id)],
            )?;
        }
    }
    exec(conn, "DELETE FROM book WHERE slug = ?", &[json!(slug)])?;

    let book_id = insert(
        conn,
        "INSERT INTO book (slug, title, root_path, genre, pov, tone, active_style, min_chars, target_chars, premise)
         VALUES (?,?,?,?,?,?,?,?,?,?)",
        &[
            json!(slug),
            json!(title),
            json!(book_dir.to_string_lossy()),
            fields.genre,
            fields.pov,
            fields.tone,
            fields.style,
            json!(fields.min_chars),
            json!(fields.target_chars),
            fields.premise,
        ],
    )?;

    // ─────────────── 2. Arcs ───────────────
    let mut arc_by_ext: HashMap<String, i64> = HashMap::new();
    for (index, arc) in arr(&meta, "arcs").iter().enumerate() {
        let ext_id = arc.get("id").cloned().unwrap_or(Value::Null);
        let color = arc.get("color").map(|value| value_or_null(Some(value))).unwrap_or(Value::Null);
        let id = insert(
            conn,
            "INSERT INTO arc (book_id, ext_id, name, color, ord) VALUES (?,?,?,?,?)",
            &[
                json!(book_id),
                ext_id.clone(),
                arc.get("name").cloned().unwrap_or(Value::Null),
                color,
                json!(index as i64),
            ],
        )?;
        arc_by_ext.insert(text(&ext_id), id);
    }
    stats.insert("arcs".into(), json!(arc_by_ext.len() as i64));

    // ─────────────── 3. Characters: registry from data.json ───────────────
    let mut char_by_name = NameIndex::default();
    let mut char_by_ext: HashMap<String, i64> = HashMap::new();

    if let Some(chars) = meta.get("chars").and_then(Value::as_object) {
        for (ext_id, character) in chars {
            let raw = character.get("n").and_then(Value::as_str).unwrap_or("");
            let clean = clean_name(raw);
            let name = if clean.is_empty() {
                raw.to_string()
            } else {
                clean.clone()
            };
            let aliases = if clean != raw {
                json!(raw)
            } else {
                Value::Null
            };
            let is_main = character.get("main").map(truthy).unwrap_or(false);
            let atmosphere = character
                .get("atm")
                .map(|value| value_or_null(Some(value)))
                .unwrap_or(Value::Null);
            let level = value_or_zero(character.get("atmLvl"));
            let id = insert(
                conn,
                "INSERT INTO character (book_id, ext_id, name, aliases, is_main, atmosphere_desc, atmosphere_level)
                 VALUES (?,?,?,?,?,?,?)",
                &[
                    json!(book_id),
                    json!(ext_id),
                    json!(name),
                    aliases,
                    json!(i64::from(is_main)),
                    atmosphere,
                    level,
                ],
            )?;
            char_by_ext.insert(ext_id.clone(), id);
            char_by_name.add(raw, id);
        }
    }

    // ─────────────── 4. Character cards ───────────────
    let mut rel_rows: Vec<(i64, Row)> = Vec::new();
    let mut state_count = 0i64;

    for file in list_md(&book_dir.join("bible").join("characters"))? {
        let body = read_text(&file)?;
        let raw_name = match h1(&body) {
            Some(name) if !name.is_empty() => name,
            _ => file_stem(&file),
        };
        let front = frontmatter(&body);
        let sections = sections(&body);

        let mut char_id = char_by_name.get(&raw_name);
        if char_id.is_none() {
            if let Some(name) = fm_get(&front, "имя") {
                char_id = char_by_name.get(name);
            }
        }
        let char_id = match char_id {
            Some(id) => id,
            None => {
                let clean = clean_name(&raw_name);
                let name = if clean.is_empty() {
                    raw_name.clone()
                } else {
                    clean
                };
                let aliases = if name != raw_name {
                    json!(raw_name)
                } else {
                    Value::Null
                };
                let id = insert(
                    conn,
                    "INSERT INTO character (book_id, ext_id, name, aliases, is_main, atmosphere_desc, atmosphere_level)
                     VALUES (?,?,?,?,?,?,?)",
                    &[
                        json!(book_id),
                        Value::Null,
                        json!(name),
                        aliases,
                        json!(0),
                        Value::Null,
                        json!(0),
                    ],
                )?;
                char_by_name.add(&raw_name, id);
                warnings.push(format!(
                    "Персонаж «{name}» есть в карточке, но отсутствует в data.json"
                ));
                id
            }
        };

        let character_section = section_text(&sections, "Характер");
        let summary = match labelled(character_section, "Текущий") {
            Some(value) if !value.is_empty() => json!(value),
            _ => {
                let first = character_section.split("\n\n").next().unwrap_or("");
                if first.is_empty() {
                    Value::Null
                } else {
                    json!(first)
                }
            }
        };
        let history = labelled_value(labelled(character_section, "История изменений"));
        let nickname = labelled(character_section, "Прозвище");

        let atmosphere = section_text(&sections, "Атмосфера");
        let atmosphere_desc = labelled_value(labelled(atmosphere, "Описание"));
        let atmosphere_level = level_out_of_5(
            labelled(atmosphere, "Влияние").unwrap_or_else(|| atmosphere.to_string()).as_str(),
        );

        // Voice and its restrictions must not overlap: a block without a
        // separate "Манера" line used to land in both fields.
        let voice_block = {
            let voice = section_text(&sections, "Голос (речь)");
            if voice.is_empty() {
                section_text(&sections, "Голос")
            } else {
                voice
            }
        };
        let voice_bullets = bullets(voice_block);
        let is_rule = Regex::new("(?i)никогда не скажет|⚠️|запрещ|нельзя").expect("voice regex");
        let voice = join_or_null(
            voice_bullets
                .iter()
                .filter(|bullet| !is_rule.is_match(bullet))
                .cloned()
                .collect(),
        );
        let voice_rules = join_or_null(
            voice_bullets
                .iter()
                .filter(|bullet| is_rule.is_match(bullet))
                .cloned()
                .collect(),
        );

        let arc_section = section_text(&sections, "Арка");

        let mut notes_parts: Vec<String> = Vec::new();
        let other = section_text(&sections, "Прочее");
        if !other.is_empty() {
            notes_parts.push(other.to_string());
        }
        if let Some(nickname) = nickname.filter(|value| !value.is_empty()) {
            notes_parts.push(format!("Прозвище: {nickname}"));
        }
        if !arc_section.is_empty() {
            notes_parts.push(arc_section.to_string());
        }
        let notes = if notes_parts.is_empty() {
            Value::Null
        } else {
            json!(notes_parts.join("\n\n"))
        };

        exec(
            conn,
            "UPDATE character SET
               aliases=COALESCE(aliases, ?), status=?, age=?, gender=?, group_name=?,
               summary=COALESCE(?,summary), character_history=?, voice=?, voice_rules=?,
               atmosphere_desc=COALESCE(?,atmosphere_desc),
               atmosphere_level=CASE WHEN ?>0 THEN ? ELSE atmosphere_level END,
               appearance=?, goals=?, beliefs=?, knows=?, notes=?, source_path=?
             WHERE id=?",
            &[
                fm_or_null(&front, "имя"),
                fm_or_null(&front, "статус"),
                fm_or_null(&front, "возраст"),
                fm_or_null(&front, "пол"),
                fm_or_null(&front, "группа"),
                summary,
                history,
                voice,
                voice_rules,
                atmosphere_desc,
                json!(atmosphere_level),
                json!(atmosphere_level),
                section_or_null(&sections, "Внешность"),
                section_or_null(&sections, "Цели"),
                section_or_null(&sections, "Взгляды и убеждения"),
                section_or_null(&sections, "Знает / Не знает"),
                notes,
                json!(rel_path(book_dir, &file)),
                json!(char_id),
            ],
        )?;

        // Key events → character evolution.
        for (index, bullet) in bullets(section_text(&sections, "Ключевые события"))
            .iter()
            .enumerate()
        {
            let chapter = chapter_no(bullet).map_or(Value::Null, |no| json!(no));
            insert(
                conn,
                "INSERT INTO character_state (character_id, chapter_no, kind, text, hidden, ord)
                 VALUES (?,?,?,?,?,?)",
                &[
                    json!(char_id),
                    chapter,
                    json!("event"),
                    json!(bullet),
                    json!(i64::from(is_hidden(bullet))),
                    json!(index as i64),
                ],
            )?;
            state_count += 1;
        }

        // Relationships wait until every character exists.
        for row in table(section_text(&sections, "Отношения")) {
            rel_rows.push((char_id, row));
        }
    }
    stats.insert("characters".into(), json!(char_by_name.size() as i64));
    stats.insert("character_states".into(), json!(state_count));

    // ─────────────── 5. Relationships ───────────────
    let split_targets = Regex::new(r",\s*").expect("relationship split");
    let mut rel_count = 0i64;
    for (from, row) in &rel_rows {
        let target_raw = js_trim(row.get("С кем"));
        if target_raw.is_empty() {
            continue;
        }
        let state = row.get("Текущее").to_string();
        let history = cell_or_null(row, "История");
        // "Клио, Сан" — one row for several characters.
        for part in split_targets.split(target_raw).map(js_trim) {
            if part.is_empty() {
                continue;
            }
            let to_id = char_by_name.get(part);
            let label = match to_id {
                Some(_) => Value::Null,
                None => json!(part),
            };
            insert(
                conn,
                "INSERT OR IGNORE INTO relationship (book_id, from_char, to_char, to_label, current_state, history, is_mutual)
                 VALUES (?,?,?,?,?,?,1)",
                &[
                    json!(book_id),
                    json!(from),
                    to_id.map_or(Value::Null, |id| json!(id)),
                    label,
                    json!(state),
                    history.clone(),
                ],
            )?;
            rel_count += 1;
        }
    }
    stats.insert("relationships".into(), json!(rel_count));

    // ─────────────── 6. Chapters ───────────────
    let mut chap_by_ext: HashMap<String, i64> = HashMap::new();
    let mut chap_by_no: HashMap<i64, i64> = HashMap::new();

    for chapter in arr(&meta, "chapters") {
        let number = chapter.get("n").and_then(num).unwrap_or(0);
        let file = book_dir
            .join("chapters")
            .join(format!("ch{:03}.md", number));
        let mut words = value_or_zero(chapter.get("words"));
        let mut chars = json!(0);
        let mut file_path = Value::Null;
        let mut status = json!("план");
        let mut title = chapter.get("title").cloned().unwrap_or(Value::Null);

        if file.exists() {
            let body = read_text(&file)?;
            words = json!(word_count(&body) as i64);
            chars = json!(chars_no_space(&body) as i64);
            file_path = json!(rel_path(book_dir, &file));
            status = json!("написано");
            if let Some(found) = h1(&body) {
                let stripped = strip_chapter_prefix(&found);
                if !stripped.is_empty() {
                    title = json!(stripped);
                }
            }
        }

        let arc = chapter
            .get("arc")
            .map(|value| arc_ref(Some(value), &arc_by_ext))
            .unwrap_or(Value::Null);
        let id = insert(
            conn,
            "INSERT INTO chapter (book_id, ext_id, number, title, file_path, words, chars_no_space, status, arc_id)
             VALUES (?,?,?,?,?,?,?,?,?)",
            &[
                json!(book_id),
                chapter.get("id").cloned().unwrap_or(Value::Null),
                json!(number),
                title,
                file_path,
                words,
                chars,
                status,
                arc,
            ],
        )?;
        if let Some(ext_id) = chapter.get("id") {
            chap_by_ext.insert(text(ext_id), id);
        }
        chap_by_no.insert(number, id);
    }

    // Chapters that exist as files only.
    let chapter_file = Regex::new(r"^ch(\d+)\.md$").expect("chapter file");
    for file in list_md(&book_dir.join("chapters"))? {
        let name = file_name(&file);
        let Some(capture) = chapter_file.captures(&name) else {
            continue;
        };
        let number = capture[1].parse::<i64>().unwrap_or(0);
        if chap_by_no.contains_key(&number) {
            continue;
        }
        let body = read_text(&file)?;
        let title = h1(&body)
            .map(|found| strip_chapter_prefix(&found))
            .filter(|value| !value.is_empty())
            .map_or(Value::Null, |value| json!(value));
        let id = insert(
            conn,
            "INSERT INTO chapter (book_id, ext_id, number, title, file_path, words, chars_no_space, status, arc_id)
             VALUES (?,?,?,?,?,?,?,?,?)",
            &[
                json!(book_id),
                json!(format!("ch{number}")),
                json!(number),
                title,
                json!(rel_path(book_dir, &file)),
                json!(word_count(&body) as i64),
                json!(chars_no_space(&body) as i64),
                json!("написано"),
                Value::Null,
            ],
        )?;
        chap_by_no.insert(number, id);
        warnings.push(format!(
            "Глава {number} есть файлом, но отсутствует в data.json"
        ));
    }
    stats.insert("chapters".into(), json!(chap_by_no.len() as i64));

    // Summaries.
    let mut summaries = 0i64;
    for file in list_md(&book_dir.join("summaries"))? {
        let name = file_name(&file);
        let Some(capture) = chapter_file.captures(&name) else {
            continue;
        };
        let number = capture[1].parse::<i64>().unwrap_or(0);
        let Some(chapter_id) = chap_by_no.get(&number).copied() else {
            continue;
        };
        exec(
            conn,
            "UPDATE chapter SET summary=? WHERE id=?",
            &[json!(js_trim(&read_text(&file)?)), json!(chapter_id)],
        )?;
        summaries += 1;
    }
    stats.insert("summaries".into(), json!(summaries));

    // ─────────────── 7. Events ───────────────
    let events = arr(&meta, "events").to_vec();
    let chapters = arr(&meta, "chapters").to_vec();
    let mut event_by_ext: HashMap<String, i64> = HashMap::new();
    for event in &events {
        let chapter_id = event
            .get("ch")
            .and_then(|value| chap_by_ext.get(&text(value)))
            .map_or(Value::Null, |id| json!(id));
        let chapter_end = event
            .get("chEnd")
            .filter(|value| truthy(value))
            .and_then(|value| chap_by_ext.get(&text(value)))
            .map_or(Value::Null, |id| json!(id));
        let arc = event
            .get("arc")
            .map(|value| arc_ref(Some(value), &arc_by_ext))
            .unwrap_or(Value::Null);
        let id = insert(
            conn,
            "INSERT INTO event (book_id, ext_id, chapter_id, chapter_end_id, title, summary, status, hidden, weight, arc_id)
             VALUES (?,?,?,?,?,?,?,?,?,?)",
            &[
                json!(book_id),
                event.get("id").cloned().unwrap_or(Value::Null),
                chapter_id,
                chapter_end,
                event.get("t").cloned().unwrap_or(Value::Null),
                cell_free_or_null(event, "s"),
                event
                    .get("status")
                    .cloned()
                    .filter(|value| truthy(value))
                    .unwrap_or_else(|| json!("план")),
                json!(i64::from(
                    event.get("hidden").map(truthy).unwrap_or(false)
                )),
                value_or_zero(event.get("w")),
                arc,
            ],
        )?;
        if let Some(ext_id) = event.get("id") {
            event_by_ext.insert(text(ext_id), id);
        }
    }

    let mut event_characters = 0i64;
    let mut event_links = 0i64;
    let mut hint_count = 0i64;
    for event in &events {
        let event_id = event
            .get("id")
            .map(|value| text(value))
            .and_then(|key| event_by_ext.get(&key).copied())
            .unwrap_or(0);

        for character in arr(event, "chars") {
            let key = text(character);
            let Some(character_id) = char_by_ext.get(&key).copied() else {
                warnings.push(format!("Событие {}: неизвестный персонаж {key}", text_of(event, "id")));
                continue;
            };
            let snapshot = event
                .get("cstate")
                .and_then(Value::as_object)
                .and_then(|state| state.get(&key))
                .filter(|value| truthy(value))
                .cloned()
                .unwrap_or(Value::Null);
            insert(
                conn,
                "INSERT OR IGNORE INTO event_character (event_id, character_id, state_snapshot)
                 VALUES (?,?,?)",
                &[json!(event_id), json!(character_id), snapshot],
            )?;
            event_characters += 1;
        }

        for link in arr(event, "links") {
            let key = text(link);
            let Some(target) = event_by_ext.get(&key).copied() else {
                warnings.push(format!(
                    "Событие {}: ссылка на неизвестное событие {key}",
                    text_of(event, "id")
                ));
                continue;
            };
            insert(
                conn,
                "INSERT OR IGNORE INTO event_link (from_event, to_event) VALUES (?,?)",
                &[json!(event_id), json!(target)],
            )?;
            event_links += 1;
        }

        for hint in arr(event, "hints") {
            let carrier_event = events.iter().find(|candidate| candidate.get("id") == hint.get("ev"));
            let carrier = match carrier_event {
                Some(candidate) if candidate.get("t").is_some_and(truthy) => {
                    candidate.get("t").cloned().unwrap_or(Value::Null)
                }
                _ => hint.get("ev").cloned().unwrap_or(Value::Null),
            };

            let chapter_value = event.get("ch").cloned().unwrap_or(Value::Null);
            let chapter_key = text(&chapter_value);
            let chapter_present =
                !chapter_key.is_empty() && chap_by_ext.contains_key(&chapter_key);
            let chapter_number = if chapter_present {
                chapters
                    .iter()
                    .find(|chapter| chapter.get("id") == Some(&chapter_value))
                    .and_then(|chapter| chapter.get("n"))
                    .and_then(num)
                    .map_or(Value::Null, |no| json!(no))
            } else {
                Value::Null
            };

            insert(
                conn,
                "INSERT INTO hint (hidden_line_id, chapter_no, text, carrier, level)
                 VALUES (NULL,?,?,?,?)",
                &[
                    chapter_number,
                    event.get("t").cloned().unwrap_or(Value::Null),
                    carrier,
                    nullish_or(hint.get("lvl"), json!(0)),
                ],
            )?;
            hint_count += 1;
        }
    }
    stats.insert("events".into(), json!(event_by_ext.len() as i64));
    stats.insert("event_characters".into(), json!(event_characters));
    stats.insert("event_links".into(), json!(event_links));

    // ─────────────── 8. Hidden lines ───────────────
    let mut hidden_beats = 0i64;
    let hidden_path = book_dir.join("plot").join("hidden.md");
    if hidden_path.exists() {
        let body = read_text(&hidden_path)?;
        let hidden_sections = sections(&body);
        let line_marker = Regex::new(r"^H\d+\s*—").expect("hidden line regex");
        let line_keys: Vec<(String, String)> = hidden_sections
            .iter()
            .filter(|(key, _)| line_marker.is_match(key))
            .cloned()
            .collect();

        for (key, line_body) in &line_keys {
            let mut pieces = key.split('—');
            let code = pieces.next().unwrap_or("");
            let name: Vec<&str> = pieces.collect();

            let grab = |label: &str| -> Value {
                let pattern = format!(r"(?im)^{label}:\s*(.+)$");
                match Regex::new(&pattern).ok().and_then(|re| re.captures(line_body).map(|c| c[1].to_string())) {
                    Some(value) => json!(js_trim(&value)),
                    None => Value::Null,
                }
            };

            let line_id = insert(
                conn,
                "INSERT INTO hidden_line (book_id, code, name, truth, who_knows, reader_learns, status)
                 VALUES (?,?,?,?,?,?,?)",
                &[
                    json!(book_id),
                    json!(js_trim(code)),
                    json!(js_trim(&name.join("—"))),
                    grab("ПРАВДА"),
                    grab("КТО ЗАМЕШАН"),
                    grab("ЧИТАТЕЛЬ УЗНАЁТ"),
                    grab("СТАТУС"),
                ],
            )?;

            let sub = sections(line_body);
            for row in table(section_text(&sub, "Хронология за кадром")) {
                let when = row.get("Когда");
                let chapter = row.get("Гл.");
                let source = if chapter.is_empty() { when } else { chapter };
                insert(
                    conn,
                    "INSERT INTO hidden_beat (hidden_line_id, chapter_no, offscreen, visible_effect, planned)
                     VALUES (?,?,?,?,?)",
                    &[
                        json!(line_id),
                        chapter_no(source).map_or(Value::Null, |no| json!(no)),
                        json!(row.get("Событие за кадром")),
                        cell_or_null(&row, "Видимое следствие"),
                        json!(i64::from(
                            Regex::new("(?i)план|не финализ")
                                .expect("planned regex")
                                .is_match(when)
                        )),
                    ],
                )?;
                hidden_beats += 1;
            }
            for row in table(section_text(&sub, "Посеянные намёки")) {
                insert(
                    conn,
                    "INSERT INTO hint (hidden_line_id, chapter_no, text, carrier, level) VALUES (?,?,?,?,?)",
                    &[
                        json!(line_id),
                        chapter_no(row.get("Гл.")).map_or(Value::Null, |no| json!(no)),
                        json!(row.get("Намёк")),
                        cell_or_null(&row, "Через что"),
                        json!(parse_level(row.get("Уровень"))),
                    ],
                )?;
                hint_count += 1;
            }
        }
        stats.insert("hidden_lines".into(), json!(line_keys.len() as i64));
    }
    stats.insert("hidden_beats".into(), json!(hidden_beats));
    stats.insert("hints".into(), json!(hint_count));

    // ─────────────── 9. Secrets ───────────────
    let mut secret_count = 0i64;
    let mut knowledge_count = 0i64;
    let secrets_path = book_dir.join("state").join("secrets.md");
    if secrets_path.exists() {
        let sections = sections(&read_text(&secrets_path)?);
        let parents = Regex::new(r"\([^)]*\)").expect("parentheses");
        let split = Regex::new(r"[,;]|\sи\s").expect("knowledge split");
        let prefix = Regex::new(r"(?i)^(только|все|весь)\s+").expect("knowledge prefix");

        for row in table(section_text(&sections, "Тайны и заблуждения")) {
            let secret_id = insert(
                conn,
                "INSERT INTO secret (book_id, ext_no, title, truth, reader_knows, reveal_plan)
                 VALUES (?,?,?,?,?,?)",
                &[
                    json!(book_id),
                    cell_or_null(&row, "#"),
                    json!(row.get("Что")),
                    Value::Null,
                    cell_or_null(&row, "Читатель знает?"),
                    cell_or_null(&row, "Раскроется"),
                ],
            )?;
            secret_count += 1;

            let mut link = |field: &str, stance: &str| -> Result<()> {
                let raw = row.get(field);
                if raw.is_empty() || raw == "—" {
                    return Ok(());
                }
                // Parentheses are removed *before* splitting: a `;` inside
                // them must not break the record.
                let cleaned = parents.replace_all(raw, "");
                for part in split.split(&cleaned).map(|part| {
                    js_trim(&prefix.replace(part, "")).to_string()
                }) {
                    if part.is_empty() || part == "—" {
                        continue;
                    }
                    let character_id = char_by_name.get(&part);
                    let label = match character_id {
                        Some(_) => Value::Null,
                        None => json!(part),
                    };
                    insert(
                        conn,
                        "INSERT INTO secret_knowledge (secret_id, character_id, label, stance)
                         VALUES (?,?,?,?)",
                        &[
                            json!(secret_id),
                            character_id.map_or(Value::Null, |id| json!(id)),
                            label,
                            json!(stance),
                        ],
                    )?;
                    knowledge_count += 1;
                }
                Ok(())
            };
            link("Кто знает правду", "знает")?;
            link("Кто заблуждается / не знает", "не знает")?;
        }
    }
    stats.insert("secrets".into(), json!(secret_count));
    stats.insert("secret_knowledge".into(), json!(knowledge_count));

    // ─────────────── 10. Promises ───────────────
    let mut promise_count = 0i64;
    let promises_path = book_dir.join("plot").join("promises.md");
    if promises_path.exists() {
        let body = read_text(&promises_path)?;
        for row in table(&body) {
            if row.get("Что заявлено").is_empty() {
                continue;
            }
            let status = row.get("Статус");
            let status = if status.is_empty() { "ожидает" } else { status };
            let fired = Regex::new("(?i)выстрелило")
                .expect("fired regex")
                .is_match(status);
            insert(
                conn,
                "INSERT INTO promise (book_id, ext_no, title, planted_ch, expected_payoff, status, fired_ch)
                 VALUES (?,?,?,?,?,?,?)",
                &[
                    json!(book_id),
                    cell_or_null(&row, "#"),
                    json!(row.get("Что заявлено")),
                    chapter_no(row.get("Где (гл.)")).map_or(Value::Null, |no| json!(no)),
                    cell_or_null(&row, "Должно выстрелить"),
                    json!(status),
                    if fired {
                        chapter_no(status).map_or(Value::Null, |no| json!(no))
                    } else {
                        Value::Null
                    },
                ],
            )?;
            promise_count += 1;
        }
    }
    stats.insert("promises".into(), json!(promise_count));

    // ─────────────── 11. World rules ───────────────
    let mut rule_count = 0i64;
    let rules_path = book_dir.join("bible").join("world_rules.md");
    if rules_path.exists() {
        let sections = sections(&read_text(&rules_path)?);
        for (category, block) in &sections {
            for (index, bullet) in bullets(block).iter().enumerate() {
                insert(
                    conn,
                    "INSERT INTO world_rule (book_id, category, text, locked, ord) VALUES (?,?,?,?,?)",
                    &[
                        json!(book_id),
                        json!(category),
                        json!(bullet),
                        json!(i64::from(bullet.contains('🔒'))),
                        json!(index as i64),
                    ],
                )?;
                rule_count += 1;
            }
        }
    }
    stats.insert("world_rules".into(), json!(rule_count));

    // ─────────────── 12. Glossary ───────────────
    let mut glossary_count = 0i64;
    let glossary_path = book_dir.join("bible").join("glossary.md");
    if glossary_path.exists() {
        let sections = sections(&read_text(&glossary_path)?);
        for (section, block) in &sections {
            for row in table(block) {
                let keys = row.keys();
                let Some(term) = keys.first() else {
                    continue;
                };
                if row.get(term).is_empty() {
                    continue;
                }
                let meaning: Vec<&str> = keys
                    .iter()
                    .skip(1)
                    .map(|key| row.get(key))
                    .filter(|value| !value.is_empty() && *value != "—")
                    .collect();
                let meaning = if meaning.is_empty() {
                    Value::Null
                } else {
                    json!(meaning.join(" · "))
                };
                insert(
                    conn,
                    "INSERT OR IGNORE INTO glossary (book_id, term, meaning, section) VALUES (?,?,?,?)",
                    &[json!(book_id), json!(row.get(term)), meaning, json!(section)],
                )?;
                glossary_count += 1;
            }
        }
    }
    stats.insert("glossary".into(), json!(glossary_count));

    // ─────────────── 13. Locations ───────────────
    let mut location_count = 0i64;
    for file in list_md(&book_dir.join("bible").join("locations"))? {
        let body = read_text(&file)?;
        let front = frontmatter(&body);
        let sections = sections(&body);
        let name = match fm_get(&front, "название") {
            Some(name) if !name.is_empty() => json!(name),
            _ => match h1(&body) {
                Some(title) if !title.is_empty() => json!(title),
                _ => json!(file_stem(&file)),
            },
        };
        insert(
            conn,
            "INSERT OR IGNORE INTO location (book_id, name, kind, where_desc, description, who_here, events_here, source_path)
             VALUES (?,?,?,?,?,?,?,?)",
            &[
                json!(book_id),
                name,
                fm_or_null(&front, "тип"),
                fm_or_null(&front, "где"),
                section_or_null(&sections, "Описание"),
                section_or_null(&sections, "Кто здесь"),
                section_or_null(&sections, "События здесь"),
                json!(rel_path(book_dir, &file)),
            ],
        )?;
        location_count += 1;
    }
    stats.insert("locations".into(), json!(location_count));

    // ─────────────── 14. Timeline ───────────────
    let mut timeline_count = 0i64;
    let timeline_path = book_dir.join("timeline").join("events.md");
    if timeline_path.exists() {
        let sections = sections(&read_text(&timeline_path)?);
        let digits_only = Regex::new(r"^\d+$").expect("chapter digits");
        for (_, block) in &sections {
            for row in table(block) {
                let when = {
                    let when = row.get("Когда");
                    if when.is_empty() {
                        row.get("Когда (дата мира)")
                    } else {
                        when
                    }
                };
                let description = row.get("Событие");
                if description.is_empty() {
                    continue;
                }
                // The "Глава" column holds a bare number, not `гл.N`.
                let raw_chapter = js_trim(row.get("Глава"));
                let chapter = if digits_only.is_match(raw_chapter) {
                    raw_chapter.parse::<i64>().ok()
                } else {
                    let source = if raw_chapter.is_empty() { when } else { raw_chapter };
                    chapter_no(source)
                };
                insert(
                    conn,
                    "INSERT INTO timeline_entry (book_id, chapter_no, story_date, description, who, hidden, ord)
                     VALUES (?,?,?,?,?,?,?)",
                    &[
                        json!(book_id),
                        chapter.map_or(Value::Null, |no| json!(no)),
                        if when.is_empty() {
                            Value::Null
                        } else {
                            json!(when)
                        },
                        json!(description),
                        cell_or_null(&row, "Кто затронут"),
                        json!(i64::from(is_hidden(description))),
                        json!(timeline_count),
                    ],
                )?;
                timeline_count += 1;
            }
        }
    }
    stats.insert("timeline".into(), json!(timeline_count));

    // ─────────────── 15. "Now" slice ───────────────
    let current_path = book_dir.join("state").join("current.md");
    if current_path.exists() {
        let body = read_text(&current_path)?;
        let sections = sections(&body);
        let last_chapter = labelled(&body, "Последняя глава").unwrap_or_default();
        insert(
            conn,
            "INSERT INTO current_state (book_id, last_chapter, story_date, positions, conflicts, open_questions)
             VALUES (?,?,?,?,?,?)",
            &[
                json!(book_id),
                chapter_no(&last_chapter).map_or(Value::Null, |no| json!(no)),
                labelled_value(labelled(&body, "Дата истории")),
                section_or_null(&sections, "Где кто находится"),
                section_or_null(&sections, "Активные конфликты"),
                section_or_null(&sections, "Открытые вопросы сцены"),
            ],
        )?;
    }

    // ─────────────── 16. Free-form documents ───────────────
    let mut docs: Vec<(String, PathBuf)> = vec![
        ("book".to_string(), PathBuf::from("book.md")),
        (
            "direction".to_string(),
            PathBuf::from("plot").join("direction.md"),
        ),
        ("arcs".to_string(), PathBuf::from("plot").join("arcs.md")),
        (
            "factions".to_string(),
            PathBuf::from("bible").join("factions.md"),
        ),
    ];
    let arc_plan = Regex::new(r"arc\d").expect("arc plan regex");
    for file in list_md(&book_dir.join("plot"))? {
        if arc_plan.is_match(&file_name(&file)) {
            docs.push(("arc_plan".to_string(), PathBuf::from(rel_path(book_dir, &file))));
        }
    }
    for file in list_md(&book_dir.join("styles"))? {
        docs.push(("style".to_string(), PathBuf::from(rel_path(book_dir, &file))));
    }

    let mut doc_count = 0i64;
    for (kind, relative) in docs {
        let full = book_dir.join(&relative);
        if !full.exists() {
            continue;
        }
        let body = read_text(&full)?;
        let rel = relative.to_string_lossy().replace('\\', "/");
        insert(
            conn,
            "INSERT OR REPLACE INTO doc (book_id, kind, rel_path, title, body) VALUES (?,?,?,?,?)",
            &[
                json!(book_id),
                json!(kind),
                json!(rel),
                h1(&body).map_or(Value::Null, |title| json!(title)),
                json!(body),
            ],
        )?;
        doc_count += 1;
    }
    stats.insert("docs".into(), json!(doc_count));

    // ─────────────── 17. Moment axis and facts ───────────────
    // Runs last: it needs the chapters, events and timeline imported above.
    let moments = build_timeline(conn, book_id)?;
    stats.insert("moments".into(), json!(moments));
    let migrated = migrate_facts(conn, book_id)?;
    stats.insert("facts".into(), json!(migrated.facts));
    stats.insert("knowledge".into(), json!(migrated.knowledge));
    // `Object.assign` in the JavaScript edition overwrites the relationship
    // count with the history-row count.
    stats.insert("relationships".into(), json!(migrated.relationships));

    // ─────────────── 18. Restore user data ───────────────
    let (restored, skipped) = restore_user_data(conn, book_id, preserved.as_ref())?;
    index_book_chapters(conn, book_id)?;
    if restored > 0 {
        stats.insert("восстановлено".into(), json!(restored));
    }
    if skipped > 0 {
        warnings.push(format!(
            "Не удалось вернуть {skipped} записей: персонаж или глава, к которым они привязаны, исчезли из markdown"
        ));
    }

    Ok(ImportOutcome {
        book_id,
        title,
        stats,
        warnings,
    })
}

// ─────────────────────────── timeline ───────────────────────────

/// Real world or book world, by the label of the moment.
fn world_of(label: &str) -> &'static str {
    if Regex::new("(?i)гринтаун|мир книги")
        .expect("world regex")
        .is_match(label)
    {
        "мир книги"
    } else {
        "реальный мир"
    }
}

/// Create one moment per distinct "Когда" label and anchor timeline rows,
/// chapters and events to them. Port of `buildTimeline`.
fn build_timeline(conn: &Connection, book_id: i64) -> Result<i64> {
    let entries = query(
        conn,
        "SELECT * FROM timeline_entry WHERE book_id=? ORDER BY ord",
        &[json!(book_id)],
    )?;

    let mut by_label: HashMap<String, i64> = HashMap::new();
    let mut seq = 0.0f64;

    for entry in &entries {
        let story_date = entry
            .get("story_date")
            .and_then(Value::as_str)
            .unwrap_or("");
        let trimmed = js_trim(story_date);
        let label = if trimmed.is_empty() {
            match entry.get("chapter_no").and_then(num) {
                Some(number) => format!("гл.{number}"),
                None => "гл.?".to_string(),
            }
        } else {
            trimmed.to_string()
        };

        if !by_label.contains_key(&label) {
            seq += 100.0;
            let world = world_of(&label);
            let id = insert(
                conn,
                "INSERT INTO moment (book_id, seq, label, world, story_date) VALUES (?,?,?,?,?)",
                &[
                    json!(book_id),
                    json!(seq),
                    json!(label),
                    json!(world),
                    json!(label),
                ],
            )?;
            by_label.insert(label.clone(), id);
        }
        let moment_id = by_label.get(&label).copied().unwrap_or(0);
        exec(
            conn,
            "UPDATE timeline_entry SET moment_id=? WHERE id=?",
            &[
                json!(moment_id),
                entry.get("id").cloned().unwrap_or(Value::Null),
            ],
        )?;
    }

    // No timeline at all: lay the chapters out in number order.
    if by_label.is_empty() {
        let chapters = query(
            conn,
            "SELECT * FROM chapter WHERE book_id=? ORDER BY number",
            &[json!(book_id)],
        )?;
        for chapter in &chapters {
            seq += 100.0;
            let label = format!("Глава {}", text_of(chapter, "number"));
            let id = insert(
                conn,
                "INSERT INTO moment (book_id, seq, label, world, story_date) VALUES (?,?,?,?,?)",
                &[json!(book_id), json!(seq), json!(label), Value::Null, Value::Null],
            )?;
            exec(
                conn,
                "UPDATE chapter SET from_moment=?, to_moment=? WHERE id=?",
                &[
                    json!(id),
                    json!(id),
                    chapter.get("id").cloned().unwrap_or(Value::Null),
                ],
            )?;
        }
        return Ok(by_label.len() as i64);
    }

    // A chapter spans from its first to its last moment.
    let spans = query(
        conn,
        "SELECT chapter_no, MIN(m.seq) a, MAX(m.seq) b
         FROM timeline_entry t JOIN moment m ON m.id = t.moment_id
         WHERE t.book_id=? AND t.chapter_no IS NOT NULL
         GROUP BY chapter_no",
        &[json!(book_id)],
    )?;
    let moments = query(
        conn,
        "SELECT id, seq FROM moment WHERE book_id=? ORDER BY seq",
        &[json!(book_id)],
    )?;
    let find_by_seq = |wanted: f64| -> Option<i64> {
        moments
            .iter()
            .find(|moment| moment.get("seq").and_then(Value::as_f64) == Some(wanted))
            .and_then(|moment| moment.get("id"))
            .and_then(num)
    };

    for span in &spans {
        let a = span.get("a").and_then(Value::as_f64).unwrap_or(f64::NAN);
        let b = span.get("b").and_then(Value::as_f64).unwrap_or(f64::NAN);
        exec(
            conn,
            "UPDATE chapter SET from_moment=?, to_moment=? WHERE book_id=? AND number=?",
            &[
                find_by_seq(a).map_or(Value::Null, |id| json!(id)),
                find_by_seq(b).map_or(Value::Null, |id| json!(id)),
                json!(book_id),
                span.get("chapter_no").cloned().unwrap_or(Value::Null),
            ],
        )?;
    }

    // Chapters without timeline rows (not written yet) go to the end.
    let unanchored = query(
        conn,
        "SELECT * FROM chapter WHERE book_id=? AND from_moment IS NULL ORDER BY number",
        &[json!(book_id)],
    )?;
    for chapter in &unanchored {
        seq += 100.0;
        let label = format!("Глава {} (план)", text_of(chapter, "number"));
        let id = insert(
            conn,
            "INSERT INTO moment (book_id, seq, label, world, story_date) VALUES (?,?,?,?,?)",
            &[json!(book_id), json!(seq), json!(label), Value::Null, Value::Null],
        )?;
        exec(
            conn,
            "UPDATE chapter SET from_moment=?, to_moment=? WHERE id=?",
            &[
                json!(id),
                json!(id),
                chapter.get("id").cloned().unwrap_or(Value::Null),
            ],
        )?;
    }

    // An event happens at the start of its chapter.
    exec(
        conn,
        "UPDATE event SET moment_id = (
           SELECT ch.from_moment FROM chapter ch WHERE ch.id = event.chapter_id
         ) WHERE book_id=? AND chapter_id IS NOT NULL",
        &[json!(book_id)],
    )?;

    scalar_i64(
        conn,
        "SELECT COUNT(*) FROM moment WHERE book_id=?",
        &[json!(book_id)],
    )
}

struct MigrationStats {
    facts: i64,
    knowledge: i64,
    relationships: i64,
}

/// Heuristic fact kind, from the wording of a character event.
fn guess_kind(value: &str) -> &'static str {
    let text = value.to_lowercase();
    let contains = |needles: &[&str]| needles.iter().any(|needle| text.contains(needle));
    if contains(&["одет", "футболк", "наряд", "внешност", "волос"]) {
        return "внешность";
    }
    if contains(&["погиб", "умер", "ранен", "шок", "пропал", "жив"]) {
        return "статус";
    }
    if contains(&["поселил", "живёт", "находится", "приходит в себя", "оказыва"]) {
        return "место";
    }
    if contains(&["характер", "цинизм", "трещина", "меняется"]) {
        return "черта";
    }
    "событие"
}

/// Move imported data onto the moment axis. Port of `migrateFacts`.
fn migrate_facts(conn: &Connection, book_id: i64) -> Result<MigrationStats> {
    let mut stats = MigrationStats {
        facts: 0,
        knowledge: 0,
        relationships: 0,
    };
    let prefix = Regex::new(r"(?i)^(до\s+)?гл\.?\s*\d+[^:]*:\s*").expect("fact prefix");

    for row in query(
        conn,
        "SELECT s.*, c.id AS cid FROM character_state s
         JOIN character c ON c.id = s.character_id
         WHERE c.book_id=? ORDER BY s.chapter_no, s.ord",
        &[json!(book_id)],
    )? {
        let raw = row.get("text").and_then(Value::as_str).unwrap_or("");
        let value = js_trim(&prefix.replace(raw, "")).to_string();
        let chapter = row.get("chapter_no").and_then(num);
        let kind = normalize_kind(conn, &json!(guess_kind(&value)))?;
        insert(
            conn,
            "INSERT INTO fact (book_id, subject_type, subject_id, kind, value,
               from_moment, from_chapter, hidden, status, source, note)
             VALUES (?,?,?,?,?,?,?,?,?,?,?)",
            &[
                json!(book_id),
                json!("character"),
                row.get("cid").cloned().unwrap_or(Value::Null),
                json!(kind),
                json!(value),
                moment_of_chapter(conn, book_id, chapter)?.map_or(Value::Null, |id| json!(id)),
                chapter.map_or(Value::Null, |no| json!(no)),
                json!(i64::from(row.get("hidden").map(truthy).unwrap_or(false))),
                json!("канон"),
                json!("импорт"),
                Value::Null,
            ],
        )?;
        stats.facts += 1;
    }

    // Secrets → knowledge, dated by the reveal plan.
    for row in query(
        conn,
        "SELECT k.*, s.reveal_plan FROM secret_knowledge k
         JOIN secret s ON s.id = k.secret_id WHERE s.book_id=?",
        &[json!(book_id)],
    )? {
        let plan = row.get("reveal_plan").and_then(Value::as_str).unwrap_or("");
        let chapter = chapter_no(plan).unwrap_or(1);
        let stance = normalize_stance(conn, row.get("stance").unwrap_or(&Value::Null))?;
        insert(
            conn,
            "INSERT INTO knowledge (book_id, character_id, holder_label, secret_id,
               stance, from_moment, from_chapter, source)
             VALUES (?,?,?,?,?,?,?,?)",
            &[
                json!(book_id),
                row.get("character_id").cloned().unwrap_or(Value::Null),
                row.get("label").cloned().unwrap_or(Value::Null),
                row.get("secret_id").cloned().unwrap_or(Value::Null),
                json!(stance),
                moment_of_chapter(conn, book_id, Some(chapter))?.map_or(Value::Null, |id| json!(id)),
                json!(chapter),
                json!("импорт"),
            ],
        )?;
        stats.knowledge += 1;
    }

    // Relationships → interval history (the field existed but stayed empty).
    for row in query(
        conn,
        "SELECT * FROM relationship WHERE book_id=?",
        &[json!(book_id)],
    )? {
        let history = row.get("history").and_then(Value::as_str).unwrap_or("");
        let chapter = chapter_no(history).unwrap_or(1);
        insert(
            conn,
            "INSERT INTO relationship_state (relationship_id, chapter_no, from_moment, state, note)
             VALUES (?,?,?,?,?)",
            &[
                row.get("id").cloned().unwrap_or(Value::Null),
                json!(chapter),
                moment_of_chapter(conn, book_id, Some(chapter))?.map_or(Value::Null, |id| json!(id)),
                row.get("current_state").cloned().unwrap_or(Value::Null),
                row.get("history").cloned().unwrap_or(Value::Null),
            ],
        )?;
        stats.relationships += 1;
    }

    // Locations and open obligations from the "now" slice.
    if let Some(current) = query_one(
        conn,
        "SELECT * FROM current_state WHERE book_id=?",
        &[json!(book_id)],
    )? {
        if let Some(questions) = current
            .get("open_questions")
            .and_then(Value::as_str)
            .filter(|value| !value.is_empty())
        {
            let last_chapter = crate::json_sql::scalar_opt_i64(
                conn,
                "SELECT MAX(number) FROM chapter WHERE book_id=? AND status='написано'",
                &[json!(book_id)],
            )?;
            for question in bullets(questions) {
                insert(
                    conn,
                    "INSERT INTO fact (book_id, subject_type, subject_id, kind, value,
                       from_moment, from_chapter, hidden, status, source, note)
                     VALUES (?,?,?,?,?,?,?,?,?,?,?)",
                    &[
                        json!(book_id),
                        json!("plot"),
                        Value::Null,
                        json!("обязательство"),
                        json!(question),
                        moment_of_chapter(conn, book_id, last_chapter)?
                            .map_or(Value::Null, |id| json!(id)),
                        last_chapter.map_or(Value::Null, |no| json!(no)),
                        json!(0),
                        json!("канон"),
                        json!("импорт"),
                        json!("из среза «сейчас»"),
                    ],
                )?;
                stats.facts += 1;
            }
        }
    }

    // The import inserted in bulk, so derive the axis order in one pass.
    backfill_seq(conn, book_id)?;
    Ok(stats)
}

/// Fill `from_seq`/`to_seq` from the linked moments (port of `backfillSeq`).
fn backfill_seq(conn: &Connection, book_id: i64) -> Result<()> {
    for table in ["fact", "knowledge"] {
        exec(
            conn,
            &format!(
                "UPDATE {table} SET
                   from_seq = (SELECT seq FROM moment WHERE id = {table}.from_moment),
                   to_seq   = (SELECT seq FROM moment WHERE id = {table}.to_moment)
                 WHERE book_id = ?"
            ),
            &[json!(book_id)],
        )?;
    }
    Ok(())
}

// ─────────────────────────── preserve ───────────────────────────

/// Everything the import did not create and therefore must not destroy.
struct Snapshot {
    facts: Vec<Value>,
    knowledge: Vec<Value>,
    annotations: Vec<Value>,
    versions: Vec<Value>,
    nodes: Vec<Value>,
    edges: Vec<Value>,
    /// Captured by `preserve.js` and never restored — see the module docs.
    rel_states: Vec<Value>,
    activity: Vec<Value>,
}

fn capture_user_data(conn: &Connection, slug: &str) -> Result<Option<Snapshot>> {
    let Some(book) = query_one(conn, "SELECT id FROM book WHERE slug = ?", &[json!(slug)])? else {
        return Ok(None);
    };
    let Some(book_id) = book.get("id").and_then(num) else {
        return Ok(None);
    };

    Ok(Some(Snapshot {
        facts: query(
            conn,
            "SELECT f.*, c.name AS subject_name_ref
             FROM fact f LEFT JOIN character c ON c.id = f.subject_id
             WHERE f.book_id = ? AND f.source <> 'импорт'",
            &[json!(book_id)],
        )?,
        knowledge: query(
            conn,
            "SELECT k.*, c.name AS holder_name_ref, s.title AS secret_title_ref
             FROM knowledge k
             LEFT JOIN character c ON c.id = k.character_id
             LEFT JOIN secret s ON s.id = k.secret_id
             WHERE k.book_id = ? AND k.source <> 'импорт'",
            &[json!(book_id)],
        )?,
        annotations: query(
            conn,
            "SELECT a.*, ch.number AS chapter_no
             FROM annotation a JOIN chapter ch ON ch.id = a.chapter_id
             WHERE ch.book_id = ?",
            &[json!(book_id)],
        )?,
        versions: query(
            conn,
            "SELECT v.*, ch.number AS chapter_no
             FROM chapter_version v JOIN chapter ch ON ch.id = v.chapter_id
             WHERE ch.book_id = ?",
            &[json!(book_id)],
        )?,
        nodes: query(
            conn,
            "SELECT n.*, ch.number AS chapter_no, e.ext_id AS event_ext
             FROM node n
             LEFT JOIN chapter ch ON ch.id = n.ref_id AND n.ref_table = 'chapter'
             LEFT JOIN event e   ON e.id  = n.ref_id AND n.ref_table = 'event'
             WHERE n.book_id = ?",
            &[json!(book_id)],
        )?,
        edges: query(conn, "SELECT * FROM edge WHERE book_id = ?", &[json!(book_id)])?,
        rel_states: query(
            conn,
            "SELECT rs.*, a.name AS from_name, COALESCE(b.name, r.to_label) AS to_name
             FROM relationship_state rs
             JOIN relationship r ON r.id = rs.relationship_id
             JOIN character a ON a.id = r.from_char
             LEFT JOIN character b ON b.id = r.to_char
             WHERE r.book_id = ? AND rs.note IS NULL",
            &[json!(book_id)],
        )?,
        activity: query(
            conn,
            "SELECT * FROM mcp_activity WHERE book_id = ?",
            &[json!(book_id)],
        )?,
    }))
}

/// Put the snapshot back, re-linking rows by names and numbers.
///
/// Known bug kept: `snapshot.rel_states` is deliberately not restored.
fn restore_user_data(
    conn: &Connection,
    book_id: i64,
    snapshot: Option<&Snapshot>,
) -> Result<(i64, i64)> {
    let Some(snapshot) = snapshot else {
        return Ok((0, 0));
    };

    // Captured but restored by neither edition (the documented bug).
    let _captured_but_unused = &snapshot.rel_states;

    let char_by_name: HashMap<String, i64> = query(
        conn,
        "SELECT id, name FROM character WHERE book_id=?",
        &[json!(book_id)],
    )?
    .into_iter()
    .filter_map(|row| {
        let id = row.get("id").and_then(num)?;
        let name = row.get("name").and_then(Value::as_str)?.to_string();
        Some((name, id))
    })
    .collect();

    let chapter_by_no: HashMap<i64, i64> = query(
        conn,
        "SELECT id, number FROM chapter WHERE book_id=?",
        &[json!(book_id)],
    )?
    .into_iter()
    .filter_map(|row| {
        let id = row.get("id").and_then(num)?;
        let number = row.get("number").and_then(num)?;
        Some((number, id))
    })
    .collect();

    let moment_by_chapter: HashMap<i64, Option<i64>> = query(
        conn,
        "SELECT number, from_moment FROM chapter WHERE book_id=?",
        &[json!(book_id)],
    )?
    .into_iter()
    .filter_map(|row| {
        let number = row.get("number").and_then(num)?;
        Some((number, row.get("from_moment").and_then(num)))
    })
    .collect();

    // The JavaScript `Map` had a `null` key for events without an `ext_id`;
    // keeping `Option<String>` reproduces the old lookup, including the bug
    // where a node pointing at a user-created event can land on another one.
    let event_by_ext: HashMap<Option<String>, i64> = query(
        conn,
        "SELECT id, ext_id FROM event WHERE book_id=?",
        &[json!(book_id)],
    )?
    .into_iter()
    .filter_map(|row| {
        let id = row.get("id").and_then(num)?;
        let ext = row
            .get("ext_id")
            .and_then(Value::as_str)
            .map(str::to_string);
        Some((ext, id))
    })
    .collect();

    let secret_by_title: HashMap<String, i64> = query(
        conn,
        "SELECT id, title FROM secret WHERE book_id=?",
        &[json!(book_id)],
    )?
    .into_iter()
    .filter_map(|row| {
        let id = row.get("id").and_then(num)?;
        let title = row.get("title").and_then(Value::as_str)?.to_string();
        Some((title, id))
    })
    .collect();

    let mut restored = 0i64;
    let mut skipped = 0i64;
    let moment_for = |chapter: Option<i64>| -> Value {
        chapter
            .and_then(|number| moment_by_chapter.get(&number).copied().flatten())
            .map_or(Value::Null, |id| json!(id))
    };

    // ── Facts
    for fact in &snapshot.facts {
        let subject_ref = fact
            .get("subject_name_ref")
            .and_then(Value::as_str)
            .filter(|name| !name.is_empty());
        let subject_id = match subject_ref {
            Some(name) => char_by_name.get(name).copied(),
            None => fact.get("subject_id").and_then(num),
        };
        let is_character = fact.get("subject_type").and_then(Value::as_str) == Some("character");
        if is_character && subject_ref.is_some() && subject_id.is_none() {
            skipped += 1;
            continue;
        }

        let from_chapter = fact.get("from_chapter").and_then(num);
        let to_chapter = fact.get("to_chapter").and_then(num);
        insert(
            conn,
            "INSERT INTO fact (book_id, subject_type, subject_id, subject_name, kind, key, value,
               weight, from_moment, to_moment, from_chapter, to_chapter, status, hidden, source, note)
             VALUES (?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?)",
            &[
                json!(book_id),
                fact.get("subject_type").cloned().unwrap_or(Value::Null),
                subject_id.map_or(Value::Null, |id| json!(id)),
                fact.get("subject_name").cloned().unwrap_or(Value::Null),
                fact.get("kind").cloned().unwrap_or(Value::Null),
                fact.get("key").cloned().unwrap_or(Value::Null),
                fact.get("value").cloned().unwrap_or(Value::Null),
                nullish_or(fact.get("weight"), json!(1)),
                moment_for(from_chapter),
                moment_for(to_chapter),
                from_chapter.map_or(Value::Null, |no| json!(no)),
                to_chapter.map_or(Value::Null, |no| json!(no)),
                nullish_or(fact.get("status"), json!("канон")),
                nullish_or(fact.get("hidden"), json!(0)),
                nullish_or(fact.get("source"), json!("вручную")),
                fact.get("note").cloned().unwrap_or(Value::Null),
            ],
        )?;
        restored += 1;
    }

    // ── Knowledge
    for knowledge in &snapshot.knowledge {
        let holder_ref = knowledge
            .get("holder_name_ref")
            .and_then(Value::as_str)
            .filter(|name| !name.is_empty());
        let character_id = holder_ref.and_then(|name| char_by_name.get(name).copied());
        if knowledge.get("character_id").is_some_and(truthy) && character_id.is_none() {
            skipped += 1;
            continue;
        }
        let secret_id = knowledge
            .get("secret_title_ref")
            .and_then(Value::as_str)
            .filter(|title| !title.is_empty())
            .and_then(|title| secret_by_title.get(title).copied());

        let from_chapter = knowledge.get("from_chapter").and_then(num);
        let to_chapter = knowledge.get("to_chapter").and_then(num);
        insert(
            conn,
            "INSERT INTO knowledge (book_id, character_id, holder_label, secret_id, topic,
               stance, belief, from_moment, to_moment, from_chapter, to_chapter, source, note)
             VALUES (?,?,?,?,?,?,?,?,?,?,?,?,?)",
            &[
                json!(book_id),
                character_id.map_or(Value::Null, |id| json!(id)),
                knowledge.get("holder_label").cloned().unwrap_or(Value::Null),
                secret_id.map_or(Value::Null, |id| json!(id)),
                knowledge.get("topic").cloned().unwrap_or(Value::Null),
                knowledge.get("stance").cloned().unwrap_or(Value::Null),
                knowledge.get("belief").cloned().unwrap_or(Value::Null),
                moment_for(from_chapter),
                moment_for(to_chapter),
                from_chapter.map_or(Value::Null, |no| json!(no)),
                to_chapter.map_or(Value::Null, |no| json!(no)),
                nullish_or(knowledge.get("source"), json!("вручную")),
                knowledge.get("note").cloned().unwrap_or(Value::Null),
            ],
        )?;
        restored += 1;
    }

    // ── Text notes
    for annotation in &snapshot.annotations {
        let Some(chapter_id) = annotation
            .get("chapter_no")
            .and_then(num)
            .and_then(|number| chapter_by_no.get(&number).copied())
        else {
            skipped += 1;
            continue;
        };
        insert(
            conn,
            "INSERT INTO annotation (chapter_id, quote, prefix, suffix, start_pos, end_pos,
               note, status, ai_result, created_at, resolved_at)
             VALUES (?,?,?,?,?,?,?,?,?,?,?)",
            &[
                json!(chapter_id),
                annotation.get("quote").cloned().unwrap_or(Value::Null),
                annotation.get("prefix").cloned().unwrap_or(Value::Null),
                annotation.get("suffix").cloned().unwrap_or(Value::Null),
                annotation.get("start_pos").cloned().unwrap_or(Value::Null),
                annotation.get("end_pos").cloned().unwrap_or(Value::Null),
                annotation.get("note").cloned().unwrap_or(Value::Null),
                annotation.get("status").cloned().unwrap_or(Value::Null),
                annotation.get("ai_result").cloned().unwrap_or(Value::Null),
                annotation.get("created_at").cloned().unwrap_or(Value::Null),
                annotation.get("resolved_at").cloned().unwrap_or(Value::Null),
            ],
        )?;
        restored += 1;
    }

    // ── Text versions
    for version in &snapshot.versions {
        let Some(chapter_id) = version
            .get("chapter_no")
            .and_then(num)
            .and_then(|number| chapter_by_no.get(&number).copied())
        else {
            skipped += 1;
            continue;
        };
        insert(
            conn,
            "INSERT INTO chapter_version (chapter_id, body, reason, created_at) VALUES (?,?,?,?)",
            &[
                json!(chapter_id),
                version.get("body").cloned().unwrap_or(Value::Null),
                version.get("reason").cloned().unwrap_or(Value::Null),
                version.get("created_at").cloned().unwrap_or(Value::Null),
            ],
        )?;
        restored += 1;
    }

    // ── Graph layout
    let mut node_id_map: HashMap<i64, i64> = HashMap::new();
    for node in &snapshot.nodes {
        let ref_table = node.get("ref_table").and_then(Value::as_str).unwrap_or("");
        let ref_id: Option<i64> = match ref_table {
            "chapter" => node
                .get("chapter_no")
                .and_then(num)
                .and_then(|number| chapter_by_no.get(&number).copied()),
            "event" => {
                let key = node
                    .get("event_ext")
                    .and_then(Value::as_str)
                    .map(str::to_string);
                event_by_ext.get(&key).copied()
            }
            _ => None,
        };
        if !ref_table.is_empty() && ref_id.is_none() {
            skipped += 1;
            continue;
        }

        let new_id = insert(
            conn,
            "INSERT INTO node (book_id, type, ref_table, ref_id, x, y, w, h, data, collapsed)
             VALUES (?,?,?,?,?,?,?,?,?,?)",
            &[
                json!(book_id),
                node.get("type").cloned().unwrap_or(Value::Null),
                node.get("ref_table").cloned().unwrap_or(Value::Null),
                ref_id.map_or(Value::Null, |id| json!(id)),
                node.get("x").cloned().unwrap_or(Value::Null),
                node.get("y").cloned().unwrap_or(Value::Null),
                node.get("w").cloned().unwrap_or(Value::Null),
                node.get("h").cloned().unwrap_or(Value::Null),
                node.get("data").cloned().unwrap_or(Value::Null),
                node.get("collapsed").cloned().unwrap_or(Value::Null),
            ],
        )?;
        if let Some(old_id) = node.get("id").and_then(num) {
            node_id_map.insert(old_id, new_id);
        }
        restored += 1;
    }

    for edge in &snapshot.edges {
        let from = edge
            .get("from_node")
            .and_then(num)
            .and_then(|id| node_id_map.get(&id).copied());
        let to = edge
            .get("to_node")
            .and_then(num)
            .and_then(|id| node_id_map.get(&id).copied());
        let (Some(from), Some(to)) = (from, to) else {
            skipped += 1;
            continue;
        };
        insert(
            conn,
            "INSERT INTO edge (book_id, from_node, from_port, to_node, to_port, kind, label)
             VALUES (?,?,?,?,?,?,?)",
            &[
                json!(book_id),
                json!(from),
                edge.get("from_port").cloned().unwrap_or(Value::Null),
                json!(to),
                edge.get("to_port").cloned().unwrap_or(Value::Null),
                edge.get("kind").cloned().unwrap_or(Value::Null),
                edge.get("label").cloned().unwrap_or(Value::Null),
            ],
        )?;
        restored += 1;
    }

    // ── AI activity feed
    for activity in &snapshot.activity {
        insert(
            conn,
            "INSERT INTO mcp_activity (book_id, tool, summary, detail, created_at) VALUES (?,?,?,?,?)",
            &[
                json!(book_id),
                activity.get("tool").cloned().unwrap_or(Value::Null),
                activity.get("summary").cloned().unwrap_or(Value::Null),
                activity.get("detail").cloned().unwrap_or(Value::Null),
                activity.get("created_at").cloned().unwrap_or(Value::Null),
            ],
        )?;
        restored += 1;
    }

    Ok((restored, skipped))
}

// ─────────────────────────── markdown ───────────────────────────

type Sections = Vec<(String, String)>;

/// `charsNoSpace` and `wordCount` live in [`crate::chapter_text`]; they are the
/// same three markdown cleanup passes.

/// First level-one heading, trimmed.
fn h1(text: &str) -> Option<String> {
    let pattern = Regex::new(r"(?m)^#\s+(.+)$").expect("h1 regex");
    pattern
        .captures(text)
        .map(|capture| js_trim(&capture[1]).to_string())
}

/// Frontmatter between `---` lines. Not YAML: split on the first colon.
fn frontmatter(text: &str) -> Vec<(String, String)> {
    let pattern = Regex::new(r"(?m)^---\n([\s\S]*?)\n---").expect("frontmatter regex");
    let Some(capture) = pattern.captures(text) else {
        return Vec::new();
    };
    let mut out: Vec<(String, String)> = Vec::new();
    for line in capture[1].split('\n') {
        let Some(index) = line.find(':') else {
            continue;
        };
        let key = js_trim(&line[..index]);
        let value = js_trim(&line[index + 1..]);
        if key.is_empty() {
            continue;
        }
        match out.iter_mut().find(|(existing, _)| existing == key) {
            Some(entry) => entry.1 = value.to_string(),
            None => out.push((key.to_string(), value.to_string())),
        }
    }
    out
}

/// Split on `##`…`####` headings; a section ends at the next heading of the
/// same or a shallower level. Later duplicates overwrite in place.
fn sections(text: &str) -> Sections {
    let pattern = Regex::new(r"(?m)^(#{2,4})\s+(.+)$").expect("section regex");
    let marks: Vec<(usize, String, usize, usize)> = pattern
        .captures_iter(text)
        .map(|capture| {
            let whole = capture.get(0).expect("whole match");
            (
                capture[1].len(),
                js_trim(&capture[2]).to_string(),
                whole.end(),
                whole.start(),
            )
        })
        .collect();

    let mut out: Sections = Vec::new();
    for (index, (level, title, body_start, _)) in marks.iter().enumerate() {
        let end = marks
            .iter()
            .skip(index + 1)
            .find(|(next_level, _, _, _)| next_level <= level)
            .map(|(_, _, _, start)| *start)
            .unwrap_or(text.len());
        let body = js_trim(&text[*body_start..end]).to_string();
        match out.iter_mut().find(|(existing, _)| existing == title) {
            Some(entry) => entry.1 = body,
            None => out.push((title.clone(), body)),
        }
    }
    out
}

/// Top-level list items; continuation lines are glued on.
fn bullets(block: &str) -> Vec<String> {
    if block.is_empty() {
        return Vec::new();
    }
    let pattern = Regex::new(r"^\s*[-*]\s+(.*)$").expect("bullet regex");
    let mut out: Vec<String> = Vec::new();
    let mut current: Option<String> = None;

    for line in block.split('\n') {
        if let Some(capture) = pattern.captures(line) {
            if let Some(previous) = current.take() {
                out.push(js_trim(&previous).to_string());
            }
            current = Some(capture[1].to_string());
        } else if let Some(text) = current.as_mut() {
            let line_trimmed = js_trim(line);
            if !line_trimmed.is_empty() && !line_trimmed.starts_with('|') && !line.starts_with('#') {
                text.push(' ');
                text.push_str(line_trimmed);
            } else if line_trimmed.is_empty() {
                out.push(js_trim(text).to_string());
                current = None;
            }
        }
    }
    if let Some(last) = current {
        out.push(js_trim(&last).to_string());
    }
    out.retain(|bullet| !bullet.is_empty());
    out
}

/// One markdown table row, keeping the header order.
#[derive(Debug, Clone, Default)]
struct Row {
    cells: Vec<(String, String)>,
}

impl Row {
    fn from_head(head: &[String], values: &[String]) -> Self {
        let cells = head
            .iter()
            .enumerate()
            .map(|(index, name)| {
                (
                    name.clone(),
                    values.get(index).cloned().unwrap_or_default(),
                )
            })
            .collect();
        Self { cells }
    }

    /// The JavaScript row object kept the last assignment for a duplicate
    /// header.
    fn get(&self, name: &str) -> &str {
        self.cells
            .iter()
            .rev()
            .find(|(key, _)| key == name)
            .map(|(_, value)| value.as_str())
            .unwrap_or("")
    }

    /// `Object.keys(row)`: unique header names in first-appearance order.
    fn keys(&self) -> Vec<&str> {
        let mut keys: Vec<&str> = Vec::new();
        for (key, _) in &self.cells {
            if !keys.contains(&key.as_str()) {
                keys.push(key);
            }
        }
        keys
    }
}

fn table(block: &str) -> Vec<Row> {
    if block.is_empty() {
        return Vec::new();
    }
    let lines: Vec<&str> = block
        .split('\n')
        .map(js_trim)
        .filter(|line| line.starts_with('|'))
        .collect();
    if lines.len() < 2 {
        return Vec::new();
    }

    let cells = |line: &str| -> Vec<String> {
        let trimmed = js_trim(line);
        let trimmed = trimmed.strip_prefix('|').unwrap_or(trimmed);
        let trimmed = trimmed.strip_suffix('|').unwrap_or(trimmed);
        trimmed.split('|').map(|cell| js_trim(cell).to_string()).collect()
    };

    let head = cells(lines[0]);
    let mut rows = Vec::new();
    for line in &lines[1..] {
        if is_separator(line) {
            continue;
        }
        let values = cells(line);
        if values.iter().all(|value| value.is_empty()) {
            continue;
        }
        rows.push(Row::from_head(&head, &values));
    }
    rows
}

fn is_separator(line: &str) -> bool {
    let trimmed = js_trim(line);
    let trimmed = trimmed.strip_prefix('|').unwrap_or(trimmed);
    let trimmed = trimmed.strip_suffix('|').unwrap_or(trimmed);
    !trimmed.is_empty()
        && trimmed
            .chars()
            .all(|c| c == ':' || c == '-' || c == '|' || js_is_space(c))
}

/// `гл.7`, `гл. 7`, `Гл.4` → 7.
fn chapter_no(text: &str) -> Option<i64> {
    let pattern = Regex::new(r"(?i)гл\.?\s*(\d+)").expect("chapter regex");
    pattern
        .captures(text)
        .and_then(|capture| capture[1].parse::<i64>().ok())
}

/// Marked as hidden / offscreen.
fn is_hidden(text: &str) -> bool {
    Regex::new("(?i)🔒|закадров|за кадром|скрыт")
        .expect("hidden regex")
        .is_match(text)
}

/// `**Влияние:** 4/5` → 4.
fn level_out_of_5(text: &str) -> i64 {
    Regex::new(r"(\d)\s*/\s*5")
        .expect("level regex")
        .captures(text)
        .and_then(|capture| capture[1].parse::<i64>().ok())
        .unwrap_or(0)
}

/// Value of a labelled bullet: `- **Описание:** похоть` → `похоть`.
fn labelled(block: &str, label: &str) -> Option<String> {
    if block.is_empty() {
        return None;
    }
    let bold = Regex::new(&format!(r"(?i)\*\*{label}[:：]?\*\*[:：]?\s*(.+)")).ok()?;
    if let Some(capture) = bold.captures(block) {
        return Some(js_trim(&capture[1]).to_string());
    }
    let plain = Regex::new(&format!(r"(?im)^\s*[-*]?\s*{label}[:：]\s*(.+)$")).ok()?;
    plain
        .captures(block)
        .map(|capture| js_trim(&capture[1]).to_string())
}

/// `«Мия (ГГ)»` → `«Мия»`; role notes are not part of the name.
fn clean_name(name: &str) -> String {
    let pattern = Regex::new(r"\s*\([^)]*\)\s*$").expect("clean name regex");
    js_trim(&pattern.replace(name, "")).to_string()
}

/// Strip the `Глава 7. ` prefix from a chapter heading.
fn strip_chapter_prefix(title: &str) -> String {
    let pattern = Regex::new(r"(?i)^Глава\s+\d+\.\s*").expect("chapter prefix");
    js_trim(&pattern.replace(title, "")).to_string()
}

// ─────────────────────────── small helpers ───────────────────────────

struct BookFields {
    genre: Value,
    pov: Value,
    tone: Value,
    style: Value,
    premise: Value,
    min_chars: i64,
    target_chars: i64,
}

impl Default for BookFields {
    fn default() -> Self {
        Self {
            genre: Value::Null,
            pov: Value::Null,
            tone: Value::Null,
            style: Value::Null,
            premise: Value::Null,
            min_chars: 10_000,
            target_chars: 12_000,
        }
    }
}

/// A name index that accepts every spelling of a character (`«Мия (ГГ)»` and
/// `«Мия»`), so links between cards are not lost.
#[derive(Default)]
struct NameIndex {
    map: HashMap<String, i64>,
}

impl NameIndex {
    fn add(&mut self, name: &str, id: i64) {
        for key in [name.to_string(), clean_name(name)] {
            if key.is_empty() {
                continue;
            }
            self.map.entry(key).or_insert(id);
        }
    }

    fn get(&self, name: &str) -> Option<i64> {
        if name.is_empty() {
            return None;
        }
        self.map
            .get(name)
            .copied()
            .or_else(|| self.map.get(&clean_name(name)).copied())
    }

    /// Distinct character count, like the JavaScript getter.
    fn size(&self) -> usize {
        self.map.values().copied().collect::<HashSet<_>>().len()
    }
}

/// `row[name] || null`.
fn cell_or_null(row: &Row, name: &str) -> Value {
    match row.get(name) {
        "" => Value::Null,
        value => json!(value),
    }
}

/// `value || null` for JSON fields.
fn value_or_null(value: Option<&Value>) -> Value {
    match value {
        Some(value) if truthy(value) => value.clone(),
        _ => Value::Null,
    }
}

/// `value || 0`.
fn value_or_zero(value: Option<&Value>) -> Value {
    match value {
        Some(value) if truthy(value) => value.clone(),
        _ => json!(0),
    }
}

/// `value ?? default`.
fn nullish_or(value: Option<&Value>, default: Value) -> Value {
    match value {
        None | Some(Value::Null) => default,
        Some(value) => value.clone(),
    }
}

/// A labelled value: `null` only when the label is absent.
fn labelled_value(value: Option<String>) -> Value {
    value.map_or(Value::Null, Value::String)
}

fn join_or_null(lines: Vec<String>) -> Value {
    if lines.is_empty() {
        Value::Null
    } else {
        json!(lines.join("\n"))
    }
}

/// `rows.get("field")` for a JSON object, as a SQL cell.
fn cell_free_or_null(value: &Value, key: &str) -> Value {
    value
        .get(key)
        .map(|inner| value_or_null(Some(inner)))
        .unwrap_or(Value::Null)
}

fn arr<'a>(value: &'a Value, key: &str) -> &'a [Value] {
    value
        .get(key)
        .and_then(Value::as_array)
        .map(Vec::as_slice)
        .unwrap_or(&[])
}

fn fm_get<'a>(front: &'a [(String, String)], key: &str) -> Option<&'a str> {
    front
        .iter()
        .rev()
        .find(|(name, _)| name == key)
        .map(|(_, value)| value.as_str())
}

fn fm_or_null(front: &[(String, String)], key: &str) -> Value {
    match fm_get(front, key) {
        Some(value) if !value.is_empty() => json!(value),
        _ => Value::Null,
    }
}

fn section_text<'a>(sections: &'a Sections, name: &str) -> &'a str {
    sections
        .iter()
        .rev()
        .find(|(title, _)| title == name)
        .map(|(_, body)| body.as_str())
        .unwrap_or("")
}

fn section_or_null(sections: &Sections, name: &str) -> Value {
    match section_text(sections, name) {
        "" => Value::Null,
        body => json!(body),
    }
}

/// `labelled(section(..), label) || null`.
fn fm_labelled(sections: &Sections, section: &str, label: &str) -> Value {
    match labelled(section_text(sections, section), label) {
        Some(value) if !value.is_empty() => json!(value),
        _ => Value::Null,
    }
}

fn arc_ref(value: Option<&Value>, arcs: &HashMap<String, i64>) -> Value {
    match value {
        Some(value) if truthy(value) => arcs
            .get(&text(value))
            .copied()
            .map_or(Value::Null, |id| json!(id)),
        _ => Value::Null,
    }
}

fn text_of(value: &Value, key: &str) -> String {
    value.get(key).map(text).unwrap_or_default()
}

/// `Number(row['Уровень']) || 0`.
fn parse_level(raw: &str) -> Value {
    match js_trim(raw).parse::<f64>() {
        Ok(value) if value != 0.0 && value.is_finite() => {
            if value.fract() == 0.0 && value.abs() <= 9_007_199_254_740_992.0 {
                json!(value as i64)
            } else {
                json!(value)
            }
        }
        _ => json!(0),
    }
}

fn read_text(path: &Path) -> Result<String> {
    let bytes = fs::read(path).map_err(|error| SfError::io(path, error))?;
    Ok(String::from_utf8_lossy(&bytes).into_owned())
}

fn read_opt(path: &Path) -> Result<Option<String>> {
    if path.exists() {
        Ok(Some(read_text(path)?))
    } else {
        Ok(None)
    }
}

/// Markdown files of one folder, ignoring `_`-prefixed drafts.
fn list_md(dir: &Path) -> Result<Vec<PathBuf>> {
    if !dir.exists() {
        return Ok(Vec::new());
    }
    let entries = fs::read_dir(dir).map_err(|error| SfError::io(dir, error))?;
    let mut files: Vec<PathBuf> = Vec::new();
    for entry in entries {
        let entry = entry.map_err(|error| SfError::io(dir, error))?;
        let name = entry.file_name().to_string_lossy().into_owned();
        if name.ends_with(".md") && !name.starts_with('_') {
            let path = entry.path();
            if path.is_file() {
                files.push(path);
            }
        }
    }
    files.sort();
    Ok(files)
}

fn rel_path(book_dir: &Path, file: &Path) -> String {
    file.strip_prefix(book_dir)
        .unwrap_or(file)
        .to_string_lossy()
        .into_owned()
}

fn file_name(path: &Path) -> String {
    path.file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_default()
}

fn file_stem(path: &Path) -> String {
    path.file_stem()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_default()
}

/// JavaScript `\s` (WhiteSpace plus LineTerminator, U+FEFF included, U+0085
/// excluded) — the set `String#trim` uses.
fn js_is_space(c: char) -> bool {
    if ('\u{2000}'..='\u{200A}').contains(&c) {
        return true;
    }
    matches!(
        c,
        '\t' | '\n'
            | '\u{000B}'
            | '\u{000C}'
            | '\r'
            | ' '
            | '\u{00A0}'
            | '\u{1680}'
            | '\u{2028}'
            | '\u{2029}'
            | '\u{202F}'
            | '\u{205F}'
            | '\u{3000}'
            | '\u{FEFF}'
    )
}

fn js_trim(text: &str) -> &str {
    text.trim_matches(js_is_space)
}

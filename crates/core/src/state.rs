//! Temporal state: the moment axis, world/character slices, facts, knowledge,
//! search, diff and the kind/stance dictionaries.
//!
//! Ported from `server/lib/state.js` and `server/lib/facts.js` of the JavaScript
//! edition: same SQL, same argument coercions, same JSON shapes — the frontend
//! reads those shapes (and array orderings) directly.
//!
//! ## Interval model
//!
//! `fact` and `knowledge` are half-open intervals on the moment axis:
//! `from_seq <= seq < to_seq`, with a NULL `from_seq` treated as far past
//! ([`FAR_PAST`]). `from_seq`/`to_seq` are denormalised copies of `moment.seq`
//! maintained by the `fact_seq_ins` / `fact_seq_upd` (and `knowledge_*`)
//! triggers. Nothing here ever writes or recomputes them; a write updates
//! `from_moment`/`to_moment` and lets the triggers derive the sequence pair.
//!
//! Slots close the previous value, they never overwrite it: a new fact in an
//! occupied `(subject, kind, key)` slot sets `to_moment` on the open rows, so
//! history stays queryable and the default slice stays bounded by the number of
//! slots rather than by the length of the book.
//!
//! ## Deliberate deviations
//!
//! * Numeric JSON fields are coerced with [`crate::json_sql::num`] (JavaScript
//!   `Number()`-ish), so `"5"` behaves like `5`. The JavaScript edition passed
//!   raw strings to SQLite, where TEXT never equals an INTEGER column, so a
//!   string moment id silently skipped slot closing. Real clients send numbers.
//! * A `kinds` filter that matches no dictionary row returns an empty list
//!   instead of raising SQLite's `IN ()` syntax error (the JS answered 500).
//! * JSON object keys come out alphabetically ordered because `serde_json`
//!   keeps objects in a `BTreeMap`; *array* order — the part clients depend on —
//!   is identical to `JSON.stringify`.
//! * A non-numeric `events`/`limit` query value yields no rows instead of
//!   binding `NaN` into `LIMIT`.

use std::collections::{BTreeMap, HashMap, HashSet};

use rusqlite::Connection;
use serde_json::{Map, Value, json};

use crate::error::Result;
use crate::json_sql::{exec, insert, num, query, query_one, scalar_i64, text, truthy};

/// Sentinel compared against a NULL `from_seq` (JavaScript `-1e9`).
pub const FAR_PAST: f64 = -1e9;

/// Kind whose facts accumulate forever instead of competing for a slot.
pub const EVENT_KIND: &str = "событие";

/// Only canon facts are visible in a slice.
pub const CANON_STATUS: &str = "канон";

/// Default `events` count for the world slice.
pub const DEFAULT_EVENT_LIMIT: f64 = 30.0;

/// Default `events` count for one character's slice.
pub const CHARACTER_EVENT_LIMIT: f64 = 20.0;

/// Default `limit` for fact search.
pub const SEARCH_LIMIT: f64 = 50.0;

/// Per-bucket cap inside [`diff`].
pub const DIFF_LIMIT: i64 = 200;

/// Label of the synthetic moment used when a book has no moments yet.
pub const NO_MOMENT_LABEL: &str = "начало";

/// Columns `PATCH /api/facts/:id` may touch (`EDITABLE.fact`).
pub const EDITABLE_FACT: &[&str] = &[
    "subject_type",
    "subject_id",
    "subject_name",
    "kind",
    "key",
    "value",
    "from_moment",
    "to_moment",
    "from_chapter",
    "to_chapter",
    "event_id",
    "status",
    "hidden",
    "note",
];

/// Columns `PATCH /api/knowledge/:id` may touch (`EDITABLE.knowledge`).
pub const EDITABLE_KNOWLEDGE: &[&str] = &[
    "character_id",
    "holder_label",
    "secret_id",
    "topic",
    "stance",
    "belief",
    "from_moment",
    "to_moment",
    "from_chapter",
    "to_chapter",
    "event_id",
    "note",
];

// ─────────────────────────── argument parsing ───────────────────────────
//
// These reproduce the JavaScript helpers in `server/index.js`, including their
// odd corners; they are pure and covered by unit tests at the bottom.

/// `Number(raw)` as far as it matters here: `''` is `0`, unparseable is `NaN`.
pub fn js_number(raw: &str) -> f64 {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        // `Number('')`, `Number('   ')` → 0.
        return 0.0;
    }
    if let Ok(value) = trimmed.parse::<f64>() {
        return value;
    }
    // `Number('0x10')` → 16; `f64::from_str` has no radix prefixes.
    let hex = trimmed
        .strip_prefix("0x")
        .or_else(|| trimmed.strip_prefix("0X"));
    if let Some(digits) = hex {
        if let Ok(value) = i64::from_str_radix(digits, 16) {
            return value as f64;
        }
    }
    f64::NAN
}

/// `idList` from `server/index.js`: `String(s ?? '').split(',').map(Number)
/// .filter(Boolean)` — drops `0` and `NaN`, keeps fractional values (which
/// simply match no row).
pub fn parse_id_list(raw: Option<&str>) -> Vec<f64> {
    raw.unwrap_or("")
        .split(',')
        .map(js_number)
        .filter(|value| *value != 0.0 && !value.is_nan())
        .collect()
}

/// `strList`: `String(s ?? '').split(',').map(trim).filter(Boolean)`.
pub fn parse_str_list(raw: Option<&str>) -> Vec<String> {
    raw.unwrap_or("")
        .split(',')
        .map(|part| part.trim().to_string())
        .filter(|part| !part.is_empty())
        .collect()
}

/// A JSON number for output: integral values print as integers (as JavaScript
/// did), `NaN`/`±∞` become `null` (what `JSON.stringify` produced).
pub fn number_value(value: f64) -> Value {
    if !value.is_finite() {
        return Value::Null;
    }
    if value.fract() == 0.0 && value.abs() <= 9_007_199_254_740_992.0 {
        return Value::from(value as i64);
    }
    Value::from(value)
}

/// `seq` of a resolved moment.
pub fn seq_of(at: &Value) -> f64 {
    at.get("seq").and_then(Value::as_f64).unwrap_or(0.0)
}

fn placeholders(count: usize) -> String {
    vec!["?"; count].join(",")
}

fn nums(values: &[f64]) -> Vec<Value> {
    values.iter().map(|value| number_value(*value)).collect()
}

fn opt_i64(value: Option<i64>) -> Value {
    value.map(Value::from).unwrap_or(Value::Null)
}

/// `LIMIT` needs an integer; SQLite truncates a REAL towards zero, and `NaN`
/// never reaches SQL (the caller returns no rows).
fn limit_rows(limit: f64) -> Option<i64> {
    if limit.is_nan() {
        return None;
    }
    if limit >= i64::MAX as f64 {
        return Some(i64::MAX);
    }
    if limit <= i64::MIN as f64 {
        return Some(i64::MIN);
    }
    Some(limit.trunc() as i64)
}

/// `value ?? null`: only a missing field or an explicit `null` collapse.
fn nullish(value: Option<&Value>) -> Value {
    match value {
        None | Some(Value::Null) => Value::Null,
        Some(other) => other.clone(),
    }
}

/// `field ?? null`, then `Number()`-coerced, as a SQL parameter.
fn opt_num(value: Option<&Value>) -> Value {
    value.and_then(num).map(Value::from).unwrap_or(Value::Null)
}

/// `input.key || default`: an empty string or other falsy value takes the
/// default, like JavaScript's `||`.
fn text_or(input: &Value, key: &str, default: &str) -> Value {
    match input.get(key) {
        Some(value) if truthy(value) => value.clone(),
        _ => Value::String(default.to_string()),
    }
}

/// `input.value ?? ''`.
fn value_or_empty(input: &Value) -> Value {
    match input.get("value") {
        None | Some(Value::Null) => Value::String(String::new()),
        Some(other) => other.clone(),
    }
}

// ─────────────────────────── moment resolution ───────────────────────────

/// Resolve "now": `?moment=ID` → `?chapter=N`'s `to_moment` → the last written
/// chapter's `to_moment` → the last moment of the book → a synthetic
/// `{id:null, seq:0, label:'начало'}`.
///
/// `chapter` is a chapter *number*, not a chapter id.
pub fn resolve_moment(
    conn: &Connection,
    book_id: i64,
    moment: Option<i64>,
    chapter: Option<i64>,
) -> Result<Value> {
    if let Some(id) = moment {
        if let Some(row) = query_one(
            conn,
            "SELECT * FROM moment WHERE id=? AND book_id=?",
            &[Value::from(id), Value::from(book_id)],
        )? {
            return Ok(row);
        }
    }

    if let Some(number) = chapter.filter(|number| *number != 0) {
        let row = query_one(
            conn,
            "SELECT to_moment FROM chapter WHERE book_id=? AND number=?",
            &[Value::from(book_id), Value::from(number)],
        )?;
        let to_moment = row
            .as_ref()
            .and_then(|row| row.get("to_moment"))
            .and_then(num)
            .filter(|value| *value != 0);
        if let Some(to_moment) = to_moment {
            if let Some(moment) = query_one(
                conn,
                "SELECT * FROM moment WHERE id=?",
                &[Value::from(to_moment)],
            )? {
                return Ok(moment);
            }
        }
    }

    if let Some(row) = query_one(
        conn,
        "SELECT m.* FROM chapter ch JOIN moment m ON m.id = ch.to_moment
         WHERE ch.book_id=? AND ch.status='написано' ORDER BY m.seq DESC LIMIT 1",
        &[Value::from(book_id)],
    )? {
        return Ok(row);
    }

    if let Some(row) = query_one(
        conn,
        "SELECT * FROM moment WHERE book_id=? ORDER BY seq DESC LIMIT 1",
        &[Value::from(book_id)],
    )? {
        return Ok(row);
    }

    Ok(json!({
        "id": Value::Null,
        "seq": 0,
        "label": NO_MOMENT_LABEL,
    }))
}

/// `moment` id stored on a chapter, by chapter number.
pub fn moment_of_chapter(
    conn: &Connection,
    book_id: i64,
    chapter_no: Option<i64>,
) -> Result<Option<i64>> {
    let Some(number) = chapter_no.filter(|number| *number != 0) else {
        return Ok(None);
    };
    let row = query_one(
        conn,
        "SELECT from_moment FROM chapter WHERE book_id=? AND number=?",
        &[Value::from(book_id), Value::from(number)],
    )?;
    Ok(row
        .as_ref()
        .and_then(|row| row.get("from_moment"))
        .and_then(num))
}

fn moment_seq(conn: &Connection, moment: Option<i64>) -> Result<Option<f64>> {
    let Some(id) = moment else {
        return Ok(None);
    };
    let row = query_one(conn, "SELECT seq FROM moment WHERE id=?", &[Value::from(id)])?;
    Ok(row
        .as_ref()
        .and_then(|row| row.get("seq"))
        .and_then(Value::as_f64))
}

// ─────────────────────────── kind dictionary ───────────────────────────

/// `expandKinds`: a branch code brings its descendants along.
pub fn expand_kinds(conn: &Connection, kinds: &[String]) -> Result<Vec<String>> {
    if kinds.is_empty() {
        return Ok(Vec::new());
    }
    let sql = format!(
        "WITH RECURSIVE tree(code) AS (
           SELECT code FROM fact_kind WHERE code IN ({})
           UNION
           SELECT k.code FROM fact_kind k JOIN tree t ON k.parent = t.code
         )
         SELECT code FROM tree",
        placeholders(kinds.len())
    );
    let params: Vec<Value> = kinds.iter().map(|kind| Value::from(kind.as_str())).collect();
    Ok(query(conn, &sql, &params)?
        .iter()
        .filter_map(|row| row.get("code").and_then(Value::as_str))
        .map(str::to_string)
        .collect())
}

/// Codes of fact kinds that hold one value per slot.
pub fn slotted_kinds(conn: &Connection) -> Result<HashSet<String>> {
    Ok(query(conn, "SELECT code FROM fact_kind WHERE slotted = 1", &[])?
        .iter()
        .filter_map(|row| row.get("code").and_then(Value::as_str))
        .map(str::to_string)
        .collect())
}

/// `kindTree`: the whole dictionary as roots with nested `children` arrays.
///
/// Rows come in `ord, code` order, which is both the siblings' order and the
/// roots' order; a child is attached to its parent even if the parent sorts
/// later, exactly like the JavaScript two-pass version.
pub fn kind_tree(conn: &Connection) -> Result<Vec<Value>> {
    let rows = query(conn, "SELECT * FROM fact_kind ORDER BY ord, code", &[])?;
    let mut nodes: Vec<Value> = rows
        .into_iter()
        .map(|row| {
            let mut row = row;
            if let Some(object) = row.as_object_mut() {
                object.insert("children".to_string(), Value::Array(Vec::new()));
            }
            row
        })
        .collect();

    let index: HashMap<String, usize> = nodes
        .iter()
        .enumerate()
        .filter_map(|(position, node)| {
            node.get("code")
                .and_then(Value::as_str)
                .map(|code| (code.to_string(), position))
        })
        .collect();

    let mut children_of: HashMap<usize, Vec<usize>> = HashMap::new();
    let mut roots: Vec<usize> = Vec::new();
    for position in 0..nodes.len() {
        match nodes[position]
            .get("parent")
            .and_then(Value::as_str)
            .and_then(|code| index.get(code).copied())
        {
            Some(parent) => children_of.entry(parent).or_default().push(position),
            None => roots.push(position),
        }
    }

    fn build(position: usize, nodes: &[Value], children_of: &HashMap<usize, Vec<usize>>) -> Value {
        let mut node = nodes[position].clone();
        if let Some(children) = children_of.get(&position) {
            let built: Vec<Value> = children
                .iter()
                .map(|child| build(*child, nodes, children_of))
                .collect();
            if let Some(object) = node.as_object_mut() {
                object.insert("children".to_string(), Value::Array(built));
            }
        }
        node
    }

    Ok(roots
        .into_iter()
        .map(|root| build(root, &nodes, &children_of))
        .collect())
}#[derive(Debug, Clone)]
struct KindMeta {
    code: String,
    title: String,
    parent: Option<String>,
    ord: i64,
}

/// Walk parents until the root, with the JavaScript cycle guard: a kind that
/// parents itself (directly or through a loop) keeps the node the walk started
/// from instead of spinning.
fn root_branch<'a>(meta: &'a HashMap<String, KindMeta>, code: &str) -> (String, String, i64) {
    let mut current = meta.get(code);
    let mut seen: HashSet<&'a str> = HashSet::new();
    while let Some(node) = current {
        let Some(parent) = node.parent.as_deref() else {
            break;
        };
        if !meta.contains_key(parent) || seen.contains(node.code.as_str()) {
            break;
        }
        seen.insert(node.code.as_str());
        current = meta.get(parent);
    }
    match current {
        Some(node) => (node.code.clone(), node.title.clone(), node.ord),
        None => (code.to_string(), code.to_string(), 999),
    }
}

fn kind_meta(conn: &Connection) -> Result<HashMap<String, KindMeta>> {
    let rows = query(conn, "SELECT code, title, parent, ord FROM fact_kind", &[])?;
    let mut meta = HashMap::with_capacity(rows.len());
    for row in &rows {
        let Some(code) = row.get("code").and_then(Value::as_str) else {
            continue;
        };
        meta.insert(
            code.to_string(),
            KindMeta {
                code: code.to_string(),
                title: row
                    .get("title")
                    .and_then(Value::as_str)
                    .unwrap_or(code)
                    .to_string(),
                parent: row.get("parent").and_then(Value::as_str).map(str::to_string),
                ord: row.get("ord").and_then(Value::as_i64).unwrap_or(0),
            },
        );
    }
    Ok(meta)
}

/// Facts laid out by dictionary branch: `облик`, `при себе`, `тело`, …
///
/// Branch order is `ord` (stable, so ties keep first-appearance order); inside
/// a branch, kinds keep the order in which they first appeared in `facts`.
pub fn group_by_branch(conn: &Connection, facts: &[Value]) -> Result<Vec<Value>> {
    struct Group {
        code: String,
        title: String,
        items: Vec<Value>,
    }
    struct Branch {
        code: String,
        title: String,
        ord: i64,
        kinds: Vec<Group>,
    }

    let meta = kind_meta(conn)?;
    let mut branches: Vec<Branch> = Vec::new();
    let mut position: HashMap<String, usize> = HashMap::new();

    for fact in facts {
        let kind = fact.get("kind").and_then(Value::as_str).unwrap_or_default();
        let (root_code, root_title, root_ord) = root_branch(&meta, kind);
        let branch_index = match position.get(&root_code) {
            Some(index) => *index,
            None => {
                let index = branches.len();
                branches.push(Branch {
                    code: root_code.clone(),
                    title: root_title,
                    ord: root_ord,
                    kinds: Vec::new(),
                });
                position.insert(root_code, index);
                index
            }
        };
        let title = meta
            .get(kind)
            .map(|meta| meta.title.clone())
            .unwrap_or_else(|| kind.to_string());
        let branch = &mut branches[branch_index];
        match branch.kinds.iter_mut().find(|group| group.code == kind) {
            Some(group) => group.items.push(fact.clone()),
            None => branch.kinds.push(Group {
                code: kind.to_string(),
                title,
                items: vec![fact.clone()],
            }),
        }
    }

    branches.sort_by_key(|branch| branch.ord);
    Ok(branches
        .into_iter()
        .map(|branch| {
            json!({
                "code": branch.code,
                "title": branch.title,
                "ord": branch.ord,
                "kinds": branch.kinds.into_iter().map(|group| json!({
                    "code": group.code,
                    "title": group.title,
                    "items": group.items,
                })).collect::<Vec<Value>>(),
            })
        })
        .collect())
}

// ─────────────────────────── slice queries ───────────────────────────

/// Filters for [`facts_at`], mirroring the `opts` object of `factsAt`.
#[derive(Debug, Clone, Default)]
pub struct FactFilter {
    /// Limit character-subject facts to these ids; other subject types stay.
    pub characters: Vec<f64>,
    /// Branch codes; expanded through the dictionary. `None` means no filter,
    /// `Some([])` also means no filter (but is echoed in `scope`).
    pub kinds: Option<Vec<String>>,
    /// `f.weight >= min_weight`.
    pub min_weight: Option<f64>,
    /// Only kinds the dictionary marks as slots (`full` slices turn this off).
    pub slotted_only: bool,
    pub subject_type: Option<String>,
    pub subject_id: Option<i64>,
    pub limit: Option<i64>,
}

/// Facts true at `seq`: `from_seq <= seq < to_seq`, canon only.
pub fn facts_at(
    conn: &Connection,
    book_id: i64,
    seq: f64,
    filter: &FactFilter,
) -> Result<Vec<Value>> {
    let mut where_parts: Vec<String> = vec![
        "f.book_id = ?".to_string(),
        format!("COALESCE(f.from_seq, {FAR_PAST}) <= ?"),
        "(f.to_seq IS NULL OR f.to_seq > ?)".to_string(),
        format!("f.status = '{CANON_STATUS}'"),
    ];
    let mut params: Vec<Value> = vec![Value::from(book_id), Value::from(seq), Value::from(seq)];

    if !filter.characters.is_empty() {
        where_parts.push(format!(
            "(f.subject_type <> 'character' OR f.subject_id IN ({}))",
            placeholders(filter.characters.len())
        ));
        params.extend(nums(&filter.characters));
    }

    if let Some(kinds) = &filter.kinds {
        if !kinds.is_empty() {
            let expanded = expand_kinds(conn, kinds)?;
            if expanded.is_empty() {
                // JavaScript produced `IN ()` here and SQLite raised a syntax
                // error (500). Answer with an empty set instead.
                where_parts.push("0".to_string());
            } else {
                where_parts.push(format!("f.kind IN ({})", placeholders(expanded.len())));
                params.extend(expanded.iter().map(|kind| Value::from(kind.as_str())));
            }
        }
    }

    if let Some(min_weight) = filter.min_weight {
        where_parts.push("f.weight >= ?".to_string());
        params.push(number_value(min_weight));
    }

    if filter.slotted_only {
        where_parts.push("f.kind IN (SELECT code FROM fact_kind WHERE slotted = 1)".to_string());
    }

    if let Some(subject_type) = &filter.subject_type {
        where_parts.push("f.subject_type = ?".to_string());
        params.push(Value::from(subject_type.as_str()));
    }

    if let Some(subject_id) = filter.subject_id {
        where_parts.push("f.subject_id = ?".to_string());
        params.push(Value::from(subject_id));
    }

    let mut sql = format!(
        "SELECT f.* FROM fact f WHERE {}
         ORDER BY f.weight DESC, f.from_seq DESC, f.id DESC",
        where_parts.join(" AND ")
    );
    if let Some(limit) = filter.limit {
        sql.push_str(&format!(" LIMIT {limit}"));
    }
    query(conn, &sql, &params)
}

/// The most recent `событие` facts at or before `seq`.
///
/// Events are not slotted: they accumulate, which is why the slice caps them
/// with `limit` instead of relying on the dictionary. `limit` is a row count
/// (0 or negative means none / unbounded, as SQLite's `LIMIT` does).
pub fn recent_events(
    conn: &Connection,
    book_id: i64,
    seq: f64,
    characters: &[f64],
    limit: f64,
    min_weight: Option<f64>,
) -> Result<Vec<Value>> {
    let Some(limit) = limit_rows(limit) else {
        return Ok(Vec::new());
    };

    let mut where_parts: Vec<String> = vec![
        "f.book_id = ?".to_string(),
        format!("f.kind = '{EVENT_KIND}'"),
        format!("COALESCE(f.from_seq, {FAR_PAST}) <= ?"),
        format!("f.status = '{CANON_STATUS}'"),
    ];
    let mut params: Vec<Value> = vec![Value::from(book_id), Value::from(seq)];

    if !characters.is_empty() {
        where_parts.push(format!("f.subject_id IN ({})", placeholders(characters.len())));
        params.extend(nums(characters));
    }

    if let Some(min_weight) = min_weight {
        where_parts.push("f.weight >= ?".to_string());
        params.push(number_value(min_weight));
    }

    params.push(Value::from(limit));
    let sql = format!(
        "SELECT f.* FROM fact f WHERE {}
         ORDER BY f.from_seq DESC, f.id DESC LIMIT ?",
        where_parts.join(" AND ")
    );
    query(conn, &sql, &params)
}

/// Knowledge true at `seq`. Unlike facts there is no status filter.
///
/// `strict` keeps only rows tied to a real character: collective holders
/// («весь клуб») must not be attributed to every member of the cast.
pub fn knowledge_at(
    conn: &Connection,
    book_id: i64,
    seq: f64,
    characters: &[f64],
    strict: bool,
) -> Result<Vec<Value>> {
    let mut where_parts: Vec<String> = vec![
        "k.book_id = ?".to_string(),
        format!("COALESCE(k.from_seq, {FAR_PAST}) <= ?"),
        "(k.to_seq IS NULL OR k.to_seq > ?)".to_string(),
    ];
    let mut params: Vec<Value> = vec![Value::from(book_id), Value::from(seq), Value::from(seq)];

    if !characters.is_empty() {
        let list = placeholders(characters.len());
        where_parts.push(if strict {
            format!("k.character_id IN ({list})")
        } else {
            format!("(k.character_id IS NULL OR k.character_id IN ({list}))")
        });
        params.extend(nums(characters));
    }

    let sql = format!(
        "SELECT k.*, c.name AS who, s.title AS secret_title
         FROM knowledge k
         LEFT JOIN character c ON c.id = k.character_id
         LEFT JOIN secret s ON s.id = k.secret_id
         WHERE {}
         ORDER BY c.name, k.id",
        where_parts.join(" AND ")
    );
    query(conn, &sql, &params)
}

/// Relationship states as of `seq`: the newest `relationship_state` whose
/// `from_moment.seq` is at or before `at`, else the current state.
pub fn relationships_at(
    conn: &Connection,
    book_id: i64,
    seq: f64,
    characters: &[f64],
) -> Result<Vec<Value>> {
    let mut where_parts: Vec<String> = vec!["r.book_id = ?".to_string()];
    // The first `?` is inside the correlated subquery, so `seq` leads.
    let mut params: Vec<Value> = vec![Value::from(seq), Value::from(book_id)];

    if !characters.is_empty() {
        let list = placeholders(characters.len());
        where_parts.push(format!("(r.from_char IN ({list}) OR r.to_char IN ({list}))"));
        params.extend(nums(characters));
        params.extend(nums(characters));
    }

    let sql = format!(
        "SELECT r.id, f.name AS from_name, f.id AS from_id,
                COALESCE(t.name, r.to_label) AS to_name, t.id AS to_id,
                COALESCE(rs.state, r.current_state) AS state, rs.chapter_no
         FROM relationship r
         JOIN character f ON f.id = r.from_char
         LEFT JOIN character t ON t.id = r.to_char
         LEFT JOIN relationship_state rs ON rs.id = (
           SELECT rs2.id FROM relationship_state rs2
           LEFT JOIN moment m2 ON m2.id = rs2.from_moment
           WHERE rs2.relationship_id = r.id AND COALESCE(m2.seq, {FAR_PAST}) <= ?
           ORDER BY COALESCE(m2.seq, 0) DESC LIMIT 1
         )
         WHERE {}
         ORDER BY f.name",
        where_parts.join(" AND ")
    );
    query(conn, &sql, &params)
}

// ─────────────────────────── slices ───────────────────────────

/// Options for [`world_state`], mirroring the `opts` object of `worldState`.
#[derive(Debug, Clone, Default)]
pub struct WorldStateOpts {
    /// Character ids to keep; empty means every character.
    pub characters: Vec<f64>,
    /// Branch codes; `None` means "all" (and prints as `'все'` in `scope`).
    pub kinds: Option<Vec<String>>,
    /// How many recent events; `None` means [`DEFAULT_EVENT_LIMIT`].
    pub events: Option<f64>,
    pub min_weight: Option<f64>,
    /// Everything, including non-slot kinds, and never events.
    pub full: bool,
}

/// The compact world slice.
///
/// Shape (identical key set to `worldState`, object keys alphabetical because
/// of `serde_json`; array order matches the JavaScript edition):
///
/// ```text
/// { at, scope: {characters, events, kinds, full},
///   totals: {facts_in_slice, facts_total, knowledge_in_slice},
///   characters: [{id, name, is_main, status, group_name, summary, voice,
///                 atmosphere_desc, atmosphere_level,
///                 facts, knows, suspects, misbeliefs, unaware}],
///   relationships: [{id, from_name, from_id, to_name, to_id, state, chapter_no}],
///   plot: [fact], world: [fact], locations: [fact],
///   open_promises: [promise] }
/// ```
pub fn world_state(
    conn: &Connection,
    book_id: i64,
    at: &Value,
    opts: &WorldStateOpts,
) -> Result<Value> {
    let seq = seq_of(at);
    let characters: Option<&[f64]> = if opts.characters.is_empty() {
        None
    } else {
        Some(&opts.characters)
    };
    let event_limit = opts.events.unwrap_or(DEFAULT_EVENT_LIMIT);

    // Slots: one current value per attribute. Their number is bounded by the
    // dictionary, not by the length of the book.
    let slots = facts_at(
        conn,
        book_id,
        seq,
        &FactFilter {
            characters: characters.unwrap_or(&[]).to_vec(),
            kinds: opts.kinds.clone(),
            min_weight: opts.min_weight,
            slotted_only: !opts.full,
            subject_type: None,
            subject_id: None,
            limit: None,
        },
    )?;

    // Events are mixed into the slice only when the filter did not exclude
    // them: asking for `kinds=эмоция` means emotions, not the event feed.
    let kinds_allow_events = match &opts.kinds {
        Some(kinds) => kinds.is_empty() || kinds.iter().any(|kind| kind == EVENT_KIND),
        None => true,
    };
    let events_wanted = !opts.full && event_limit > 0.0 && kinds_allow_events;
    let events = if events_wanted {
        recent_events(
            conn,
            book_id,
            seq,
            characters.unwrap_or(&[]),
            event_limit,
            opts.min_weight,
        )?
    } else {
        Vec::new()
    };

    let facts_in_slice = slots.len() + events.len();

    let mut by_character: BTreeMap<i64, Vec<Value>> = Default::default();
    let mut plot: Vec<Value> = Vec::new();
    let mut world: Vec<Value> = Vec::new();
    let mut locations: Vec<Value> = Vec::new();

    for fact in slots.into_iter().chain(events) {
        let subject_type = fact
            .get("subject_type")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string();
        let subject_id = fact.get("subject_id").cloned().unwrap_or(Value::Null);
        if subject_type == "character" && truthy(&subject_id) {
            if let Some(id) = num(&subject_id) {
                by_character.entry(id).or_default().push(fact);
            }
            // A non-numeric character subject has no character row to attach
            // to; the JavaScript edition dropped it at render time too.
        } else if subject_type == "plot" {
            plot.push(fact);
        } else if subject_type == "location" {
            locations.push(fact);
        } else {
            world.push(fact);
        }
    }

    let knowledge = knowledge_at(conn, book_id, seq, characters.unwrap_or(&[]), false)?;
    let mut knowledge_by_character: BTreeMap<i64, Vec<Value>> = Default::default();
    for row in &knowledge {
        let holder = row.get("character_id").cloned().unwrap_or(Value::Null);
        if !truthy(&holder) {
            continue;
        }
        if let Some(id) = num(&holder) {
            knowledge_by_character.entry(id).or_default().push(row.clone());
        }
    }

    let mut char_sql = String::from(
        "SELECT id, name, is_main, status, group_name, summary, voice,
                atmosphere_desc, atmosphere_level
         FROM character WHERE book_id=?",
    );
    let mut char_params: Vec<Value> = vec![Value::from(book_id)];
    if let Some(characters) = characters {
        char_sql.push_str(&format!(" AND id IN ({})", placeholders(characters.len())));
        char_params.extend(nums(characters));
    }
    char_sql.push_str(" ORDER BY is_main DESC, name");
    let char_rows = query(conn, &char_sql, &char_params)?;

    let stance_is = |rows: &[Value], stance: &str| -> Value {
        Value::Array(
            rows.iter()
                .filter(|row| row.get("stance").and_then(Value::as_str) == Some(stance))
                .cloned()
                .collect(),
        )
    };

    let mut chars: Vec<Value> = Vec::with_capacity(char_rows.len());
    for row in char_rows {
        let Value::Object(mut object) = row else {
            continue;
        };
        let id = object.get("id").and_then(num).unwrap_or(0);
        let known = knowledge_by_character.remove(&id).unwrap_or_default();
        object.insert(
            "facts".to_string(),
            Value::Array(by_character.remove(&id).unwrap_or_default()),
        );
        object.insert("knows".to_string(), stance_is(&known, "знает"));
        object.insert("suspects".to_string(), stance_is(&known, "подозревает"));
        object.insert("misbeliefs".to_string(), stance_is(&known, "заблуждается"));
        object.insert("unaware".to_string(), stance_is(&known, "не знает"));
        chars.push(Value::Object(object));
    }

    let relationships = relationships_at(conn, book_id, seq, characters.unwrap_or(&[]))?;
    let facts_total = scalar_i64(
        conn,
        "SELECT COUNT(*) FROM fact WHERE book_id=?",
        &[Value::from(book_id)],
    )?;
    let open_promises = query(
        conn,
        "SELECT * FROM promise WHERE book_id=? AND status NOT LIKE 'выстрелило%' ORDER BY id",
        &[Value::from(book_id)],
    )?;

    let scope_characters = match characters {
        Some(ids) => Value::Array(nums(ids)),
        None => Value::String("все".to_string()),
    };
    let scope_kinds = match &opts.kinds {
        Some(kinds) => Value::Array(kinds.iter().map(|kind| Value::from(kind.as_str())).collect()),
        None => Value::String("все".to_string()),
    };

    Ok(json!({
        "at": at,
        "scope": {
            "characters": scope_characters,
            "events": number_value(event_limit),
            "kinds": scope_kinds,
            "full": opts.full,
        },
        "totals": {
            "facts_in_slice": Value::from(facts_in_slice as i64),
            "facts_total": Value::from(facts_total),
            "knowledge_in_slice": Value::from(knowledge.len() as i64),
        },
        "characters": chars,
        "relationships": relationships,
        "plot": plot,
        "world": world,
        "locations": locations,
        "open_promises": open_promises,
    }))
}

/// Options for [`character_state`].
#[derive(Debug, Clone, Default)]
pub struct CharacterStateOpts {
    /// All facts, not just slot kinds.
    pub full: bool,
    /// How many events; `None` means [`CHARACTER_EVENT_LIMIT`].
    pub events: Option<f64>,
}

/// One character's slice: current slots, grouped history, awareness.
///
/// Shape (object keys alphabetical, array order as in `characterState`):
///
/// ```text
/// { at,
///   character: {id, name, is_main, status, age, gender, group_name, summary,
///               voice, atmosphere_desc, atmosphere_level},
///   facts: {<kind>: [fact]},
///   branches: [{code, title, ord, kinds: [{code, title, items}]}],
///   knowledge: [knowledge],
///   awareness: {knows, suspects, misbeliefs, unaware},
///   relationships: [...],
///   history_available: <count> }
/// ```
///
/// `Ok(None)` means the character does not exist (the route answers 404).
pub fn character_state(
    conn: &Connection,
    character_id: i64,
    at: &Value,
    opts: &CharacterStateOpts,
) -> Result<Option<Value>> {
    let Some(character) = query_one(
        conn,
        "SELECT * FROM character WHERE id=?",
        &[Value::from(character_id)],
    )?
    else {
        return Ok(None);
    };
    let book_id = character.get("book_id").and_then(num).unwrap_or(0);
    let seq = seq_of(at);

    let current = facts_at(
        conn,
        book_id,
        seq,
        &FactFilter {
            subject_type: Some("character".to_string()),
            subject_id: Some(character_id),
            slotted_only: !opts.full,
            ..FactFilter::default()
        },
    )?;
    let events = recent_events(
        conn,
        book_id,
        seq,
        &[character_id as f64],
        opts.events.unwrap_or(CHARACTER_EVENT_LIMIT),
        None,
    )?;

    let mut all: Vec<Value> = current;
    all.extend(events);

    let mut grouped: BTreeMap<String, Vec<Value>> = Default::default();
    for fact in &all {
        let kind = fact.get("kind").and_then(Value::as_str).unwrap_or_default();
        grouped.entry(kind.to_string()).or_default().push(fact.clone());
    }
    let mut facts_object = Map::new();
    for (kind, items) in grouped {
        facts_object.insert(kind, Value::Array(items));
    }

    let knowledge = knowledge_at(conn, book_id, seq, &[character_id as f64], true)?;
    let stance_is = |stance: &str| -> Value {
        Value::Array(
            knowledge
                .iter()
                .filter(|row| row.get("stance").and_then(Value::as_str) == Some(stance))
                .cloned()
                .collect(),
        )
    };

    let branches = group_by_branch(conn, &all)?;
    let relationships = relationships_at(conn, book_id, seq, &[character_id as f64])?;
    let history_available = scalar_i64(
        conn,
        "SELECT COUNT(*) FROM fact WHERE book_id=? AND subject_id=? AND subject_type='character'",
        &[Value::from(book_id), Value::from(character_id)],
    )?;

    Ok(Some(json!({
        "at": at,
        "character": {
            "id": character.get("id").cloned().unwrap_or(Value::Null),
            "name": character.get("name").cloned().unwrap_or(Value::Null),
            "is_main": character.get("is_main").cloned().unwrap_or(Value::Null),
            "status": character.get("status").cloned().unwrap_or(Value::Null),
            "age": character.get("age").cloned().unwrap_or(Value::Null),
            "gender": character.get("gender").cloned().unwrap_or(Value::Null),
            "group_name": character.get("group_name").cloned().unwrap_or(Value::Null),
            "summary": character.get("summary").cloned().unwrap_or(Value::Null),
            "voice": character.get("voice").cloned().unwrap_or(Value::Null),
            "atmosphere_desc": character.get("atmosphere_desc").cloned().unwrap_or(Value::Null),
            "atmosphere_level": character.get("atmosphere_level").cloned().unwrap_or(Value::Null),
        },
        "facts": Value::Object(facts_object),
        "branches": branches,
        "knowledge": knowledge,
        "awareness": {
            "knows": stance_is("знает"),
            "suspects": stance_is("подозревает"),
            "misbeliefs": stance_is("заблуждается"),
            "unaware": stance_is("не знает"),
        },
        "relationships": relationships,
        "history_available": Value::from(history_available),
    })))
}

/// Fact search: `value` or `key` contains `q`.
///
/// The `LIKE` is SQLite's: case-insensitive for ASCII only, so Cyrillic
/// matching is effectively case-sensitive. `%` and `_` in the query stay
/// wildcards, as before.
pub fn search_facts(
    conn: &Connection,
    book_id: i64,
    q: Option<&str>,
    at: Option<f64>,
    characters: &[f64],
    kinds: Option<&[String]>,
    limit: f64,
) -> Result<Vec<Value>> {
    let Some(limit) = limit_rows(limit) else {
        return Ok(Vec::new());
    };

    let mut where_parts: Vec<String> = vec!["f.book_id = ?".to_string()];
    let mut params: Vec<Value> = vec![Value::from(book_id)];

    if let Some(q) = q.filter(|q| !q.is_empty()) {
        where_parts.push("(f.value LIKE ? OR f.key LIKE ?)".to_string());
        let pattern = format!("%{q}%");
        params.push(Value::from(pattern.clone()));
        params.push(Value::from(pattern));
    }

    if let Some(at) = at {
        where_parts.push(format!("COALESCE(f.from_seq, {FAR_PAST}) <= ?"));
        params.push(Value::from(at));
    }

    if !characters.is_empty() {
        where_parts.push(format!("f.subject_id IN ({})", placeholders(characters.len())));
        params.extend(nums(characters));
    }

    if let Some(kinds) = kinds.filter(|kinds| !kinds.is_empty()) {
        let expanded = expand_kinds(conn, kinds)?;
        if expanded.is_empty() {
            where_parts.push("0".to_string());
        } else {
            where_parts.push(format!("f.kind IN ({})", placeholders(expanded.len())));
            params.extend(expanded.iter().map(|kind| Value::from(kind.as_str())));
        }
    }

    params.push(Value::from(limit));
    let sql = format!(
        "SELECT f.*, c.name AS subject, m.label AS from_label
         FROM fact f
         LEFT JOIN character c ON c.id = f.subject_id AND f.subject_type='character'
         LEFT JOIN moment m ON m.id = f.from_moment
         WHERE {}
         ORDER BY f.from_seq DESC, f.id DESC LIMIT ?",
        where_parts.join(" AND ")
    );
    query(conn, &sql, &params)
}

/// Delta between two moments: what appeared, what ended, what was learned.
///
/// Arguments are normalised so the earlier moment is `from`; each bucket is
/// capped at [`DIFF_LIMIT`] and, like the original, nothing is filtered by
/// status or hidden flag.
pub fn diff(conn: &Connection, book_id: i64, from: &Value, to: &Value) -> Result<Value> {
    let (a, b) = if seq_of(from) <= seq_of(to) {
        (from, to)
    } else {
        (to, from)
    };
    let (a_seq, b_seq) = (Value::from(seq_of(a)), Value::from(seq_of(b)));
    let book = Value::from(book_id);

    let appeared = query(
        conn,
        &format!(
            "SELECT f.*, c.name AS subject FROM fact f
             LEFT JOIN character c ON c.id = f.subject_id AND f.subject_type='character'
             WHERE f.book_id=? AND f.from_seq > ? AND f.from_seq <= ?
             ORDER BY f.from_seq LIMIT {DIFF_LIMIT}"
        ),
        &[book.clone(), a_seq.clone(), b_seq.clone()],
    )?;

    let ended = query(
        conn,
        &format!(
            "SELECT f.*, c.name AS subject FROM fact f
             LEFT JOIN character c ON c.id = f.subject_id AND f.subject_type='character'
             WHERE f.book_id=? AND f.to_seq > ? AND f.to_seq <= ?
             ORDER BY f.to_seq LIMIT {DIFF_LIMIT}"
        ),
        &[book.clone(), a_seq.clone(), b_seq.clone()],
    )?;

    let learned = query(
        conn,
        &format!(
            "SELECT k.*, c.name AS who, s.title AS secret_title FROM knowledge k
             LEFT JOIN character c ON c.id = k.character_id
             LEFT JOIN secret s ON s.id = k.secret_id
             WHERE k.book_id=? AND k.from_seq > ? AND k.from_seq <= ?
             ORDER BY k.from_seq LIMIT {DIFF_LIMIT}"
        ),
        &[book, a_seq, b_seq],
    )?;

    Ok(json!({
        "from": a,
        "to": b,
        "appeared": appeared,
        "ended": ended,
        "learned": learned,
    }))
}

// ─────────────────────────── dictionary normalisation ───────────────────────────

/// Unknown kinds do not fail the write: they become [`EVENT_KIND`].
pub fn normalize_kind(conn: &Connection, kind: &Value) -> Result<String> {
    let candidate = if truthy(kind) {
        text(kind)
    } else {
        EVENT_KIND.to_string()
    };
    let code = candidate.trim().to_lowercase();
    let known = query_one(
        conn,
        "SELECT code FROM fact_kind WHERE code=?",
        &[Value::from(code.as_str())],
    )?;
    Ok(if known.is_some() {
        code
    } else {
        EVENT_KIND.to_string()
    })
}

/// Unknown stances do not fail the write: they become `знает`.
pub fn normalize_stance(conn: &Connection, stance: &Value) -> Result<String> {
    let candidate = if truthy(stance) {
        text(stance)
    } else {
        "знает".to_string()
    };
    let code = candidate.trim().to_lowercase();
    let known = query_one(
        conn,
        "SELECT code FROM knowledge_stance WHERE code=?",
        &[Value::from(code.as_str())],
    )?;
    Ok(if known.is_some() {
        code
    } else {
        "знает".to_string()
    })
}

// ─────────────────────────── slotted writes ───────────────────────────

/// Add a fact, closing whatever occupied its slot before.
///
/// Returns `{fact, closed}` where `closed` holds `{id, value}` of the rows that
/// were ended. Slot identity is `(subject_type, subject_id, kind, key)`; only
/// `slotted` kinds close, only canon rows close, and only when the new fact has
/// a resolvable `from_seq` (an unanchored fact closes nothing).
///
/// Like the original, the close and the insert are **not** wrapped in a
/// transaction: the triggers, not this function, maintain `from_seq`/`to_seq`.
pub fn add_fact(conn: &Connection, book_id: i64, input: &Value) -> Result<Value> {
    let kind = normalize_kind(conn, input.get("kind").unwrap_or(&Value::Null))?;
    let key = nullish(input.get("key"));
    let subject_type = text_or(input, "subject_type", "character");
    let subject_id = input.get("subject_id").and_then(num);
    let from_chapter = input.get("from_chapter").and_then(num);

    // `input.from_moment ?? momentOfChapter(input.from_chapter)` — presence is
    // what matters: an explicit but unusable moment does not fall back.
    let from_moment = match input.get("from_moment") {
        Some(moment) if !moment.is_null() => num(moment),
        _ => moment_of_chapter(conn, book_id, from_chapter)?,
    };
    let from_seq = moment_seq(conn, from_moment)?;

    let mut closed: Vec<Value> = Vec::new();
    if let Some(from_seq) = from_seq {
        if slotted_kinds(conn)?.contains(&kind) {
            let subject_param = opt_i64(subject_id);
            let previous = query(
                conn,
                "SELECT id, value FROM fact
                 WHERE book_id=? AND subject_type=? AND kind=?
                   AND (subject_id IS ? OR subject_id = ?)
                   AND (key IS ? OR key = ?)
                   AND from_seq < ?
                   AND (to_seq IS NULL OR to_seq > ?)
                   AND status='канон'",
                &[
                    Value::from(book_id),
                    subject_type.clone(),
                    Value::from(kind.as_str()),
                    subject_param.clone(),
                    subject_param,
                    key.clone(),
                    key.clone(),
                    Value::from(from_seq),
                    Value::from(from_seq),
                ],
            )?;
            for row in &previous {
                exec(
                    conn,
                    "UPDATE fact SET to_moment=?, to_chapter=? WHERE id=?",
                    &[
                        opt_i64(from_moment),
                        opt_i64(from_chapter),
                        row.get("id").cloned().unwrap_or(Value::Null),
                    ],
                )?;
            }
            closed = previous;
        }
    }

    let hidden = if truthy(input.get("hidden").unwrap_or(&Value::Null)) {
        1
    } else {
        0
    };
    let weight = match input.get("weight") {
        None | Some(Value::Null) => Value::from(1),
        Some(weight) => weight.clone(),
    };

    let id = insert(
        conn,
        "INSERT INTO fact (book_id, subject_type, subject_id, subject_name, kind, key, value,
           weight, from_moment, from_chapter, to_moment, to_chapter, event_id, status, hidden, source,
           note, source_chapter_id, source_quote)
         VALUES (?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?)",
        &[
            Value::from(book_id),
            subject_type,
            opt_i64(subject_id),
            nullish(input.get("subject_name")),
            Value::from(kind.as_str()),
            key,
            value_or_empty(input),
            weight,
            opt_i64(from_moment),
            opt_i64(from_chapter),
            opt_num(input.get("to_moment")),
            opt_num(input.get("to_chapter")),
            opt_num(input.get("event_id")),
            text_or(input, "status", CANON_STATUS),
            Value::from(hidden),
            text_or(input, "source", "вручную"),
            nullish(input.get("note")),
            opt_num(input.get("source_chapter_id")),
            nullish(input.get("source_quote")),
        ],
    )?;

    let fact = query_one(conn, "SELECT * FROM fact WHERE id=?", &[Value::from(id)])?
        .unwrap_or(Value::Null);
    Ok(json!({ "fact": fact, "closed": closed }))
}

/// Add knowledge, closing the previous stance of the same slot.
///
/// Returns `{knowledge, closed}` with `{id, stance}` in `closed`. Slot identity
/// is `(character_id, holder_label, secret_id, topic)`; closure needs a holder
/// (character or label) and a resolvable `from_seq`, and — unlike facts — has
/// no `status` filter.
pub fn add_knowledge(conn: &Connection, book_id: i64, input: &Value) -> Result<Value> {
    let from_chapter = input.get("from_chapter").and_then(num);
    let from_moment = match input.get("from_moment") {
        Some(moment) if !moment.is_null() => num(moment),
        _ => moment_of_chapter(conn, book_id, from_chapter)?,
    };
    let from_seq = moment_seq(conn, from_moment)?;

    let character_id = input.get("character_id").and_then(num);
    let holder_label = nullish(input.get("holder_label"));
    let secret_id = input.get("secret_id").and_then(num);
    let topic = nullish(input.get("topic"));
    let has_holder = character_id.is_some()
        || input
            .get("holder_label")
            .map(truthy)
            .unwrap_or(false);

    let mut closed: Vec<Value> = Vec::new();
    if from_seq.is_some() && has_holder {
        let from_seq = from_seq.unwrap_or_default();
        let character_param = opt_i64(character_id);
        let secret_param = opt_i64(secret_id);
        let previous = query(
            conn,
            "SELECT id, stance FROM knowledge
             WHERE book_id=?
               AND (character_id IS ? OR character_id = ?)
               AND (holder_label IS ? OR holder_label = ?)
               AND (secret_id IS ? OR secret_id = ?)
               AND (topic IS ? OR topic = ?)
               AND from_seq < ?
               AND (to_seq IS NULL OR to_seq > ?)",
            &[
                Value::from(book_id),
                character_param.clone(),
                character_param,
                holder_label.clone(),
                holder_label.clone(),
                secret_param.clone(),
                secret_param,
                topic.clone(),
                topic.clone(),
                Value::from(from_seq),
                Value::from(from_seq),
            ],
        )?;
        for row in &previous {
            exec(
                conn,
                "UPDATE knowledge SET to_moment=?, to_chapter=? WHERE id=?",
                &[
                    opt_i64(from_moment),
                    opt_i64(from_chapter),
                    row.get("id").cloned().unwrap_or(Value::Null),
                ],
            )?;
        }
        closed = previous;
    }

    let stance = normalize_stance(conn, input.get("stance").unwrap_or(&Value::Null))?;
    let id = insert(
        conn,
        "INSERT INTO knowledge (book_id, character_id, holder_label, secret_id, topic,
           stance, belief, from_moment, from_chapter, event_id, source, note,
           source_chapter_id, source_quote)
         VALUES (?,?,?,?,?,?,?,?,?,?,?,?,?,?)",
        &[
            Value::from(book_id),
            opt_i64(character_id),
            holder_label,
            opt_i64(secret_id),
            topic,
            Value::from(stance.as_str()),
            nullish(input.get("belief")),
            opt_i64(from_moment),
            opt_i64(from_chapter),
            opt_num(input.get("event_id")),
            text_or(input, "source", "вручную"),
            nullish(input.get("note")),
            opt_num(input.get("source_chapter_id")),
            nullish(input.get("source_quote")),
        ],
    )?;

    let knowledge = query_one(conn, "SELECT * FROM knowledge WHERE id=?", &[Value::from(id)])?
        .unwrap_or(Value::Null);
    Ok(json!({ "knowledge": knowledge, "closed": closed }))
}

// ─────────────────────────── row editing ───────────────────────────

/// Whitelisted partial update, the `crud.patch` equivalent.
///
/// Only keys present in `allowed` are written, in body order; nothing else can
/// reach the SQL, and values stay bound. Returns the number of changed rows (0
/// when the body carries no writable field).
pub fn patch_row(
    conn: &Connection,
    table: &str,
    id: i64,
    allowed: &[&str],
    body: &Value,
) -> Result<usize> {
    let Some(object) = body.as_object() else {
        return Ok(0);
    };
    let fields: Vec<(&str, &Value)> = object
        .iter()
        .filter(|(key, _)| allowed.contains(&key.as_str()))
        .map(|(key, value)| (key.as_str(), value))
        .collect();
    if fields.is_empty() {
        return Ok(0);
    }
    let assignments = fields
        .iter()
        .map(|(key, _)| format!("{key}=?"))
        .collect::<Vec<_>>()
        .join(", ");
    let mut params: Vec<Value> = fields.iter().map(|(_, value)| (*value).clone()).collect();
    params.push(Value::from(id));
    exec(
        conn,
        &format!("UPDATE {table} SET {assignments} WHERE id=?"),
        &params,
    )
}

// ─────────────────────────── tests ───────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    fn kind(code: &str, parent: Option<&str>, ord: i64) -> (String, KindMeta) {
        (
            code.to_string(),
            KindMeta {
                code: code.to_string(),
                title: format!("T:{code}"),
                parent: parent.map(str::to_string),
                ord,
            },
        )
    }

    #[test]
    fn id_list_drops_zero_and_nan() {
        assert_eq!(parse_id_list(Some("1,0,2,abc,3")), vec![1.0, 2.0, 3.0]);
        assert_eq!(parse_id_list(Some(" 4 , 5 ")), vec![4.0, 5.0]);
        assert_eq!(parse_id_list(Some("1.5")), vec![1.5]);
        assert!(parse_id_list(Some("0")).is_empty());
        assert!(parse_id_list(Some("")).is_empty());
        assert!(parse_id_list(None).is_empty());
    }

    #[test]
    fn str_list_trims_and_drops_empty() {
        assert_eq!(
            parse_str_list(Some(" статус , ,место ")),
            vec!["статус".to_string(), "место".to_string()]
        );
        assert!(parse_str_list(Some(",,")).is_empty());
        assert!(parse_str_list(None).is_empty());
        assert_eq!(
            parse_str_list(Some("внешность")),
            vec!["внешность".to_string()]
        );
    }

    #[test]
    fn js_number_matches_number_coercion() {
        assert_eq!(js_number(""), 0.0);
        assert_eq!(js_number("   "), 0.0);
        assert_eq!(js_number(" 12 "), 12.0);
        assert_eq!(js_number("1e3"), 1000.0);
        assert_eq!(js_number("0x10"), 16.0);
        assert_eq!(js_number("-2.5"), -2.5);
        assert!(js_number("abc").is_nan());
    }

    #[test]
    fn output_numbers_keep_javascript_text() {
        // `JSON.stringify(30)` → 30, not 30.0.
        assert_eq!(number_value(30.0), json!(30));
        assert_eq!(number_value(-1.0), json!(-1));
        assert_eq!(number_value(1.5), json!(1.5));
        // `JSON.stringify(NaN)` / `Infinity` → null.
        assert_eq!(number_value(f64::NAN), Value::Null);
        assert_eq!(number_value(f64::INFINITY), Value::Null);
    }

    #[test]
    fn limits_truncate_towards_zero() {
        assert_eq!(limit_rows(30.0), Some(30));
        assert_eq!(limit_rows(0.0), Some(0));
        assert_eq!(limit_rows(2.9), Some(2));
        // A negative limit means "no upper bound" in SQLite.
        assert_eq!(limit_rows(-1.0), Some(-1));
        assert_eq!(limit_rows(f64::NAN), None);
    }

    #[test]
    fn far_past_renders_as_the_javascript_literal() {
        // The SQL fragment must read `COALESCE(f.from_seq, -1000000000)`.
        assert_eq!(format!("{FAR_PAST}"), "-1000000000");
    }

    #[test]
    fn interval_predicate_is_half_open() {
        // Mirrors `from_seq <= seq < to_seq`, with NULL `to_seq` open-ended and
        // NULL `from_seq` at far past — the predicate the SQL builders emit.
        let at = |from: Option<f64>, to: Option<f64>, seq: f64| {
            from.map(|from| from <= seq).unwrap_or(FAR_PAST <= seq)
                && to.map(|to| to > seq).unwrap_or(true)
        };
        assert!(at(Some(100.0), Some(200.0), 100.0), "left edge is inside");
        assert!(at(Some(100.0), Some(200.0), 199.5));
        assert!(!at(Some(100.0), Some(200.0), 200.0), "right edge is outside");
        assert!(!at(Some(100.0), Some(200.0), 99.0));
        assert!(at(Some(100.0), None, 10_000.0), "open interval");
        assert!(at(None, Some(500.0), -900_000_000.0), "NULL from is far past");
        assert!(at(None, None, 42.0), "no bounds at all");
    }

    #[test]
    fn root_branch_walks_to_the_root() {
        let map: HashMap<String, KindMeta> = [
            kind("внешность", None, 10),
            kind("одежда", Some("внешность"), 14),
            kind("головной убор", Some("одежда"), 15),
        ]
        .into_iter()
        .collect();
        assert_eq!(
            root_branch(&map, "головной убор"),
            ("внешность".to_string(), "T:внешность".to_string(), 10)
        );
        assert_eq!(
            root_branch(&map, "внешность"),
            ("внешность".to_string(), "T:внешность".to_string(), 10)
        );
        // Not in the dictionary: the code stands in for itself, ord 999.
        assert_eq!(
            root_branch(&map, "нетакого"),
            ("нетакого".to_string(), "нетакого".to_string(), 999)
        );
    }

    #[test]
    fn root_branch_survives_a_cycle() {
        let map: HashMap<String, KindMeta> =
            [kind("a", Some("b"), 1), kind("b", Some("a"), 2)]
                .into_iter()
                .collect();
        assert_eq!(root_branch(&map, "a").0, "a");
        assert_eq!(root_branch(&map, "b").0, "b");
    }

    #[test]
    fn nullish_keeps_only_null_and_missing() {
        assert_eq!(nullish(None), Value::Null);
        assert_eq!(nullish(Some(&Value::Null)), Value::Null);
        assert_eq!(nullish(Some(&json!(0))), json!(0));
        assert_eq!(nullish(Some(&json!(""))), json!(""));
    }

    #[test]
    fn text_or_matches_javascript_default() {
        let body = json!({"status": "", "source": "ИИ"});
        assert_eq!(text_or(&body, "status", CANON_STATUS), json!(CANON_STATUS));
        assert_eq!(text_or(&body, "source", "вручную"), json!("ИИ"));
        assert_eq!(text_or(&body, "missing", "x"), json!("x"));
    }
}

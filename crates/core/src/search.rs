//! Full-text search over chapter prose and offscreen events.
//!
//! Port of `server/lib/search.js`. The chapter side is an FTS5 query over the
//! derived `chapter_search` index (`snippet()` marks hits with `[`…`]` and
//! joins distant fragments with `…`); the hidden-line side is a plain `LIKE`
//! over `hidden_beat.offscreen` / `visible_effect`, so a phrase hidden behind an
//! invented spelling still shows up as an offscreen lead.
//!
//! The query length check (`2..=150`) is a `400` — the same rule the old route
//! threw before touching SQLite.

use rusqlite::Connection;
use serde_json::{Value, json};

use crate::error::{Result, SfError};
use crate::json_sql::query;

/// Shortest accepted query, in characters.
pub const MIN_QUERY_CHARS: usize = 2;
/// Longest accepted query, in characters.
pub const MAX_QUERY_CHARS: usize = 150;
/// `limit` when the caller passed nothing (or `0`).
pub const DEFAULT_LIMIT: f64 = 30.0;

/// Search one book's prose and offscreen events.
///
/// `limit` is already `Number()`-coerced by the route: `None`/`0`/`NaN` mean
/// the default, and the result is clamped to `1..=100` like the old
/// `Math.max(1, Math.min(100, Number(limit) || 30))`.
pub fn search_book_text(
    conn: &Connection,
    book_id: i64,
    search: &str,
    limit: Option<f64>,
) -> Result<Value> {
    let q = search.trim();
    let length = q.chars().count();
    if !(MIN_QUERY_CHARS..=MAX_QUERY_CHARS).contains(&length) {
        return Err(SfError::Invalid("Запрос: от 2 до 150 символов".into()));
    }

    let limit = clamped_limit(limit);
    let phrase = phrase_for(q);
    let chapters = query(
        conn,
        "SELECT cs.chapter_id, ch.number, ch.title,
           snippet(chapter_search, 2, '[', ']', '…', 22) AS snippet
         FROM chapter_search cs JOIN chapter ch ON ch.id=cs.chapter_id
         WHERE cs.book_id=? AND chapter_search MATCH ? ORDER BY rank LIMIT ?",
        &[json!(book_id), json!(phrase), json!(limit)],
    )?;

    let pattern = format!("%{q}%");
    let offscreen = query(
        conn,
        "SELECT hb.id, hb.chapter_no, hl.name AS line,
           hb.offscreen, hb.visible_effect, hb.planned
         FROM hidden_beat hb JOIN hidden_line hl ON hl.id=hb.hidden_line_id
         WHERE hl.book_id=? AND (hb.offscreen LIKE ? OR hb.visible_effect LIKE ?)
         ORDER BY hb.chapter_no LIMIT ?",
        &[
            json!(book_id),
            json!(pattern),
            json!(pattern),
            json!(limit),
        ],
    )?;

    Ok(json!({ "chapters": chapters, "offscreen": offscreen }))
}

/// FTS5 phrase: the query is quoted so operators like `NEAR`/`OR` in the
/// author's phrase stay literal; embedded quotes are doubled.
fn phrase_for(q: &str) -> String {
    format!("\"{}\"", q.replace('"', "\"\""))
}

/// `Math.max(1, Math.min(100, Number(limit) || 30))`, truncated to an integer.
fn clamped_limit(limit: Option<f64>) -> i64 {
    let raw = match limit {
        Some(value) if value.is_finite() && value != 0.0 => value,
        _ => DEFAULT_LIMIT,
    };
    raw.min(100.0).max(1.0) as i64
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn quotes_embedded_double_quotes() {
        assert_eq!(phrase_for("он сказал \"да\""), "\"он сказал \"\"да\"\"\"");
        assert_eq!(phrase_for("NEAR(a b)"), "\"NEAR(a b)\"");
    }

    #[test]
    fn clamps_and_defaults_the_limit() {
        assert_eq!(clamped_limit(None), 30);
        assert_eq!(clamped_limit(Some(0.0)), 30);
        assert_eq!(clamped_limit(Some(f64::NAN)), 30);
        assert_eq!(clamped_limit(Some(-5.0)), 1);
        assert_eq!(clamped_limit(Some(7.0)), 7);
        assert_eq!(clamped_limit(Some(1000.0)), 100);
    }

    #[test]
    fn length_rule_matches_the_java_script_bounds() {
        assert!((MIN_QUERY_CHARS..=MAX_QUERY_CHARS).contains(&2));
        assert!((MIN_QUERY_CHARS..=MAX_QUERY_CHARS).contains(&150));
        assert!(!(MIN_QUERY_CHARS..=MAX_QUERY_CHARS).contains(&1));
        assert!(!(MIN_QUERY_CHARS..=MAX_QUERY_CHARS).contains(&151));
    }
}

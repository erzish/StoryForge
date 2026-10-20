//! Chapter prose on disk: paths, revisions, save ordering and the FTS mirror.
//!
//! This is the Rust counterpart of `server/lib/chapterText.js`, plus the two
//! metrics from `server/lib/md.js` that saving a chapter derives
//! (`charsNoSpace` and `wordCount`). The prose file is the source of truth;
//! `chapter.file_path`, `chapter.words`, `chapter.chars_no_space`,
//! `chapter.status` and the `chapter_search` row are caches rewritten on every
//! save.
//!
//! Parity notes that are easy to lose:
//!
//! * `chars_no_space` counts **UTF-16 code units**, not `char`s — JavaScript's
//!   `String#length`. An astral character (an emoji) is two units, and the
//!   number feeds `book.min_chars` checks and reports.
//! * The markdown projection is exactly three passes over the text: strip
//!   `^#{1,6}\s*` at line starts, remove every `**`, remove every `---`. The
//!   heading pass uses JavaScript's multiline `^` (also true after `\r`,
//!   U+2028 and U+2029) and JavaScript's `\s`, which includes U+FEFF and
//!   excludes U+0085 — neither matches the `regex` crate's `(?m)^`/`\s`, so
//!   both are hand-rolled here.
//! * Save order is observable: revision check, unchanged short-circuit, then
//!   `BEGIN IMMEDIATE` → version row (only when the file exists) → atomic
//!   write → chapter row → FTS row → commit. Any failure rolls back and
//!   restores the previous file best-effort.
//!
//! A chapter's file name is `chapters/chNNN.md` (three-digit `number`) unless
//! `chapter.file_path` says otherwise; every path is resolved against
//! `book.root_path` and anything absolute or escaping it is refused.

use std::path::{Component, Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

use rusqlite::Connection;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};

use crate::db::Db;
use crate::error::{Result, SfError};
use crate::json_sql;

/// Reason recorded for a plain save (the default of R23).
pub const REASON_MANUAL: &str = "ручная правка";
/// Reason recorded when a version is restored (R26).
pub const REASON_RESTORE: &str = "восстановление версии";

const REVISION_CONFLICT: &str =
    "Глава изменилась после открытия. Сравните её с черновиком перед сохранением.";
const RESTORE_FAILED_SUFFIX: &str = "; не удалось восстановить файл — проверьте главу на диске";
const CHAPTER_NOT_FOUND: &str = "Глава не найдена";
const INVALID_PATH: &str = "Недопустимый путь главы";

// ─────────────────────────── revisions and metrics ───────────────────────────

/// Lowercase hex SHA-256 of the UTF-8 body.
///
/// The revision is a public API value (returned by the text, version and
/// AI routes and required back by the save routes), so the algorithm is part
/// of the contract, not an implementation detail.
pub fn revision_of(body: &str) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let digest = Sha256::digest(body.as_bytes());
    let mut out = String::with_capacity(digest.len() * 2);
    for byte in digest {
        out.push(HEX[usize::from(byte >> 4)] as char);
        out.push(HEX[usize::from(byte & 0x0f)] as char);
    }
    out
}

/// `charsNoSpace` from `lib/md.js`: markdown markers stripped, all whitespace
/// removed, remaining length in **UTF-16 code units**.
/// `md.charsNoSpace` / `md.wordCount` cleanup, copied from `lib/md.js`:
/// strip leading heading marks, then every `**` and `---`.
///
/// This is deliberately narrower than [`markdown_to_text`]: the counters in the
/// editor must match the JavaScript edition exactly, not a nicer plaintext.
fn js_metric_clean(text: &str) -> String {
    // JavaScript's multiline `^` also matches after \r, \u2028 and \u2029.
    let normalized = text
        .replace("\r\n", "\n")
        .replace('\r', "\n")
        .replace('\u{2028}', "\n")
        .replace('\u{2029}', "\n");
    let mut out = String::with_capacity(text.len());
    for line in normalized.split('\n') {
        let hashes = line.chars().take_while(|c| *c == '#').count();
        // `#{1,6}` is capped: a seventh `#` survives in the text.
        let stripped = hashes.min(6);
        let rest = if stripped > 0 {
            line[stripped..].trim_start_matches([' ', '\t'])
        } else {
            line
        };
        out.push_str(rest);
        out.push('\n');
    }
    out.replace("**", "").replace("---", "")
}

pub fn chars_no_space(text: &str) -> usize {
    let clean = js_metric_clean(text);
    clean
        .chars()
        .filter(|c| !is_js_whitespace(*c))
        .map(char::len_utf16)
        .sum()
}

/// `wordCount` from `lib/md.js`: non-empty whitespace-separated tokens after
/// the same markdown cleanup.
pub fn word_count(text: &str) -> usize {
    let clean = js_metric_clean(text);
    clean
        .split(is_js_whitespace)
        .filter(|token| !token.is_empty())
        .count()
}

/// The three cleanup passes shared by both metrics.
fn markdown_to_text(text: &str) -> String {
    let stripped = strip_heading_markers(text);
    stripped.replace("**", "").replace("---", "")
}

/// JavaScript's `\s`.
///
/// ECMAScript WhiteSpace plus LineTerminator. Notably it contains U+FEFF and
/// not U+0085, which is the opposite of the Unicode `White_Space` property the
/// `regex` crate uses.
fn is_js_whitespace(c: char) -> bool {
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

/// ECMAScript LineTerminator, the set JavaScript's multiline `^` looks back at.
fn is_line_terminator(c: char) -> bool {
    matches!(c, '\n' | '\r' | '\u{2028}' | '\u{2029}')
}

/// `text.replace(/^#{1,6}\s*/gm, '')`.
///
/// `^` under JavaScript's `m` flag is true at the start of the text and after
/// any LineTerminator, and `\s*` is greedy across newlines, so `"#\n#"` loses
/// both markers. The `regex` crate's `(?m)^` only looks back at `\n`, hence the
/// manual scan.
fn strip_heading_markers(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    let mut line_start = true;
    while let Some(c) = rest.chars().next() {
        if line_start && c == '#' {
            // `#{1,6}` is greedy but capped: a seventh `#` stays in the text.
            let hashes = rest.chars().take_while(|ch| *ch == '#').take(6).count();
            let after_hashes = &rest[hashes..];
            let whitespace: usize = after_hashes
                .chars()
                .take_while(|ch| is_js_whitespace(*ch))
                .map(char::len_utf8)
                .sum();
            line_start = after_hashes[..whitespace]
                .chars()
                .next_back()
                .is_some_and(is_line_terminator);
            rest = &after_hashes[whitespace..];
            continue;
        }
        out.push(c);
        line_start = is_line_terminator(c);
        rest = &rest[c.len_utf8()..];
    }
    out
}

// ─────────────────────────── paths and reading ───────────────────────────

/// A chapter's prose location.
#[derive(Debug, Clone)]
pub struct ChapterFile {
    /// The `chapter` row, joined with `book.root_path`.
    pub chapter: Value,
    /// Path as stored in (or derived for) `chapter.file_path`.
    pub rel: PathBuf,
    /// Absolute path on disk, lexically resolved against `book.root_path`.
    pub full: PathBuf,
}

/// A chapter plus its prose.
#[derive(Debug, Clone)]
pub struct ChapterText {
    /// The `chapter` row, joined with `book.root_path`.
    pub chapter: Value,
    /// Path as stored in (or derived for) `chapter.file_path`.
    pub rel: PathBuf,
    /// Absolute path on disk.
    pub full: PathBuf,
    /// File contents, empty when the file is absent.
    pub body: String,
    /// [`revision_of`] the body.
    pub revision: String,
    /// `chapter.file_path` is set but the file is gone. A chapter that never
    /// had a `file_path` is *not* missing.
    pub missing: bool,
    /// `chapter.file_path` was a non-empty string.
    pub has_file_path: bool,
}

/// Resolve a chapter's prose file, refusing paths outside the book folder.
pub fn chapter_file(conn: &Connection, chapter_id: i64) -> Result<ChapterFile> {
    let chapter = json_sql::query_one(
        conn,
        "SELECT ch.*, b.root_path FROM chapter ch JOIN book b ON b.id=ch.book_id WHERE ch.id=?",
        &[json!(chapter_id)],
    )?
    .ok_or_else(|| SfError::NotFound(CHAPTER_NOT_FOUND.into()))?;

    let root_path = chapter
        .get("root_path")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string();
    let number = chapter.get("number").and_then(json_sql::num).unwrap_or(0);
    let file_path = chapter
        .get("file_path")
        .and_then(Value::as_str)
        .filter(|path| !path.is_empty());

    let rel = match file_path {
        Some(path) => PathBuf::from(path),
        None => Path::new("chapters").join(format!("ch{}.md", pad_start(number, 3))),
    };

    if rel.is_absolute()
        || rel.has_root()
        || rel.components().any(|c| matches!(c, Component::Prefix(_)))
    {
        return Err(SfError::Invalid(INVALID_PATH.into()));
    }
    // `join` in Node normalizes lexically before the containment check, so a
    // relative `..` is resolved here the same way; one that survives at the
    // front leaves the book folder and is refused.
    let rel_normalized = normalize_relative(&rel);
    if matches!(
        rel_normalized.components().next(),
        Some(Component::ParentDir)
    ) {
        return Err(SfError::Invalid(INVALID_PATH.into()));
    }

    let full = PathBuf::from(&root_path).join(&rel_normalized);
    Ok(ChapterFile { chapter, rel, full })
}

/// Read a chapter's prose. A missing file is not an error: the body is empty
/// and [`ChapterText::missing`] says whether the row expected a file.
pub fn read_chapter_text(conn: &Connection, chapter_id: i64) -> Result<ChapterText> {
    let file = chapter_file(conn, chapter_id)?;
    let has_file_path = file
        .chapter
        .get("file_path")
        .and_then(Value::as_str)
        .is_some_and(|path| !path.is_empty());
    let exists = file.full.exists();
    let body = if exists {
        read_text(&file.full)?
    } else {
        String::new()
    };
    let revision = revision_of(&body);
    Ok(ChapterText {
        chapter: file.chapter,
        rel: file.rel,
        full: file.full,
        body,
        revision,
        missing: has_file_path && !exists,
        has_file_path,
    })
}

/// Rebuild the FTS index of one book from the prose files.
///
/// Only chapters with a `file_path` and an existing file are indexed, exactly
/// like `indexBookChapters` in the JavaScript edition. Import runs this after
/// rebuilding a book.
pub fn index_book_chapters(conn: &Connection, book_id: i64) -> Result<()> {
    json_sql::exec(
        conn,
        "DELETE FROM chapter_search WHERE book_id=?",
        &[json!(book_id)],
    )?;
    let chapters = json_sql::query(
        conn,
        "SELECT id FROM chapter WHERE book_id=? AND file_path IS NOT NULL",
        &[json!(book_id)],
    )?;
    for row in chapters {
        let Some(chapter_id) = row.get("id").and_then(json_sql::num) else {
            continue;
        };
        let file = chapter_file(conn, chapter_id)?;
        if !file.full.exists() {
            continue;
        }
        let body = read_text(&file.full)?;
        json_sql::exec(
            conn,
            "INSERT INTO chapter_search (book_id, chapter_id, body) VALUES (?,?,?)",
            &[json!(book_id), json!(chapter_id), json!(body)],
        )?;
    }
    Ok(())
}

// ─────────────────────────── saving ───────────────────────────

/// Options of [`save_chapter_text`].
#[derive(Debug, Clone)]
pub struct SaveOptions {
    /// Required to match the current revision; `None` skips the check, which
    /// is what a plain save without `expected_revision` does.
    pub expected_revision: Option<String>,
    /// Reason recorded on the version row. Defaults to [`REASON_MANUAL`].
    pub reason: String,
    /// `ai_run` that produced the text, when the caller knows one.
    pub ai_run_id: Option<i64>,
}

impl Default for SaveOptions {
    fn default() -> Self {
        Self {
            expected_revision: None,
            reason: REASON_MANUAL.to_string(),
            ai_run_id: None,
        }
    }
}

/// Result of a successful save.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SaveOutcome {
    /// The body was already on disk; nothing was written and no version row
    /// was created.
    pub unchanged: bool,
    /// [`revision_of`] the saved body.
    pub revision: String,
    /// Metric stored on the chapter row.
    pub chars_no_space: usize,
    /// Metric stored on the chapter row.
    pub words: usize,
}

/// Save a chapter's prose: version the previous body, write the file
/// atomically, refresh the chapter row and the FTS row, all in one
/// `BEGIN IMMEDIATE` transaction.
///
/// On failure the transaction is rolled back and the previous file content is
/// restored best-effort; if that restore fails too, the error message says so.
pub fn save_chapter_text(
    db: &Db,
    chapter_id: i64,
    body: &str,
    options: &SaveOptions,
) -> Result<SaveOutcome> {
    db.with_conn(|conn| save_on(conn, chapter_id, body, options))
}

fn save_on(
    conn: &Connection,
    chapter_id: i64,
    body: &str,
    options: &SaveOptions,
) -> Result<SaveOutcome> {
    // The read comes first: an unknown chapter is 404 even with an
    // `expected_revision` that could never match, and the revision check runs
    // before the unchanged short-circuit (which writes nothing at all).
    let current = read_chapter_text(conn, chapter_id)?;

    if let Some(expected) = options.expected_revision.as_deref()
        && expected != current.revision.as_str()
    {
        return Err(SfError::Conflict(REVISION_CONFLICT.into()));
    }

    if body == current.body && current.full.exists() {
        return Ok(SaveOutcome {
            unchanged: true,
            revision: current.revision,
            chars_no_space: chars_no_space(body),
            words: word_count(body),
        });
    }

    json_sql::exec_batch(conn, "BEGIN IMMEDIATE")?;
    let mut file_changed = false;
    match write_chapter(conn, chapter_id, body, &current, options, &mut file_changed) {
        Ok(outcome) => Ok(outcome),
        Err(error) => {
            // The original failure matters more than the rollback's.
            let _ = conn.execute_batch("ROLLBACK");
            if !file_changed {
                return Err(error);
            }
            match restore_file(&current) {
                Ok(()) => Err(error),
                Err(_) => Err(with_restore_note(error)),
            }
        }
    }
}

/// The write half of a save, including `COMMIT`.
///
/// `file_changed` is set as soon as the prose file has been replaced, which is
/// what decides whether the caller must restore it.
fn write_chapter(
    conn: &Connection,
    chapter_id: i64,
    body: &str,
    current: &ChapterText,
    options: &SaveOptions,
    file_changed: &mut bool,
) -> Result<SaveOutcome> {
    // Only an existing file has a previous body worth keeping.
    if current.full.exists() {
        json_sql::exec(
            conn,
            "INSERT INTO chapter_version (chapter_id, body, reason, ai_run_id) VALUES (?,?,?,?)",
            &[
                json!(chapter_id),
                json!(current.body),
                json!(options.reason),
                options.ai_run_id.map_or(Value::Null, |id| json!(id)),
            ],
        )?;
    }

    atomic_write(&current.full, body)?;
    *file_changed = true;

    let chars = chars_no_space(body);
    let words = word_count(body);
    let rel = current.rel.to_string_lossy().replace('\\', "/");
    json_sql::exec(
        conn,
        "UPDATE chapter SET file_path=?, words=?, chars_no_space=?, status='написано', \
         updated_at=datetime('now') WHERE id=?",
        &[json!(rel), json!(words), json!(chars), json!(chapter_id)],
    )?;

    let book_id = current
        .chapter
        .get("book_id")
        .cloned()
        .unwrap_or(Value::Null);
    json_sql::exec(
        conn,
        "DELETE FROM chapter_search WHERE chapter_id=?",
        &[json!(chapter_id)],
    )?;
    json_sql::exec(
        conn,
        "INSERT INTO chapter_search (book_id, chapter_id, body) VALUES (?,?,?)",
        &[book_id, json!(chapter_id), json!(body)],
    )?;

    json_sql::exec_batch(conn, "COMMIT")?;
    Ok(SaveOutcome {
        unchanged: false,
        revision: revision_of(body),
        chars_no_space: chars,
        words,
    })
}

/// Put the previous state back after a failed save.
fn restore_file(current: &ChapterText) -> Result<()> {
    if current.has_file_path || !current.body.is_empty() {
        atomic_write(&current.full, &current.body)
    } else {
        std::fs::remove_file(&current.full).map_err(|e| SfError::io(&current.full, e))
    }
}

/// Replace a file by writing a temporary sibling and renaming it over the
/// target, creating parent directories first.
fn atomic_write(full: &Path, body: &str) -> Result<()> {
    let dir = parent_dir(full);
    std::fs::create_dir_all(dir).map_err(|e| SfError::io(dir, e))?;
    let tmp = dir.join(format!(".{}.tmp", temp_name()));
    if let Err(error) = std::fs::write(&tmp, body.as_bytes()) {
        remove_if_present(&tmp);
        return Err(SfError::io(&tmp, error));
    }
    if let Err(error) = std::fs::rename(&tmp, full) {
        remove_if_present(&tmp);
        return Err(SfError::io(full, error));
    }
    Ok(())
}

fn remove_if_present(path: &Path) {
    if path.exists() {
        let _ = std::fs::remove_file(path);
    }
}

/// A directory to write into; an empty parent means the current directory.
fn parent_dir(path: &Path) -> &Path {
    match path.parent() {
        Some(parent) if !parent.as_os_str().is_empty() => parent,
        _ => Path::new("."),
    }
}

/// Unique temporary file name, the replacement for the JavaScript edition's
/// `.<randomUUID()>.tmp`.
fn temp_name() -> String {
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let seq = COUNTER.fetch_add(1, Ordering::Relaxed);
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|elapsed| elapsed.as_nanos())
        .unwrap_or(0);
    format!("{}-{nanos:x}-{seq:x}", std::process::id())
}

/// Keep the status of the original error while noting the failed restore.
fn with_restore_note(error: SfError) -> SfError {
    let status = error.status();
    let message = format!("{error}{RESTORE_FAILED_SUFFIX}");
    match status {
        400 => SfError::Invalid(message),
        404 => SfError::NotFound(message),
        409 => SfError::Conflict(message),
        _ => SfError::Other(message),
    }
}

/// Resolve `.` and `..` lexically, without touching the filesystem, for a path
/// that is already known to be relative.
fn normalize_relative(path: &Path) -> PathBuf {
    let mut out = PathBuf::new();
    for component in path.components() {
        match component {
            Component::CurDir => {}
            Component::ParentDir => {
                if !out.pop() {
                    out.push("..");
                }
            }
            Component::Normal(name) => out.push(name),
            // Rejected by the caller; kept so the function stays total.
            Component::Prefix(prefix) => out.push(prefix.as_os_str()),
            Component::RootDir => {}
        }
    }
    out
}

/// `String(value).padStart(width, '0')` for a chapter number.
fn pad_start(value: i64, width: usize) -> String {
    let text = value.to_string();
    let length = text.chars().count();
    if length >= width {
        text
    } else {
        format!("{}{text}", "0".repeat(width - length))
    }
}

/// Read a text file as UTF-8, replacing invalid sequences the way
/// `readFileSync(path, 'utf8')` does.
fn read_text(path: &Path) -> Result<String> {
    let bytes = std::fs::read(path).map_err(|e| SfError::io(path, e))?;
    Ok(String::from_utf8_lossy(&bytes).into_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn revision_is_lowercase_sha256_of_utf8() {
        assert_eq!(
            revision_of(""),
            "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"
        );
        assert_eq!(
            revision_of("abc"),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
        // Cyrillic text is hashed as its UTF-8 bytes, not as UTF-16.
        let cyrillic = revision_of("Привет");
        assert_eq!(cyrillic.len(), 64);
        assert!(
            cyrillic
                .chars()
                .all(|c| c.is_ascii_hexdigit() && !c.is_ascii_uppercase())
        );
        assert_ne!(revision_of("Привет"), revision_of("привет"));
    }

    #[test]
    fn chars_count_utf16_code_units() {
        // A single astral character is two UTF-16 units, one `char`.
        assert_eq!("😀".chars().count(), 1);
        assert_eq!(chars_no_space("😀"), 2);
        assert_eq!(chars_no_space("a😀b"), 4);
        assert_eq!(chars_no_space("😀😀"), 4);
        // Cyrillic is one UTF-16 unit per letter.
        assert_eq!(chars_no_space("Привет, мир!"), 11);
        // Whitespace never counts.
        assert_eq!(chars_no_space("  spaced\tout\n"), 9);
        assert_eq!(chars_no_space(""), 0);
        assert_eq!(chars_no_space(" \t\n\u{00A0}"), 0);
        // JavaScript's `\s` includes U+FEFF ...
        assert_eq!(chars_no_space("\u{FEFF}текст"), 5);
        // ... and excludes U+0085, unlike the Unicode White_Space property.
        assert_eq!(chars_no_space("a\u{0085}b"), 3);
    }

    #[test]
    fn markdown_projection_strips_headings_bold_and_rules() {
        assert_eq!(chars_no_space("# Глава 1\n\nОн пришёл."), 15);
        assert_eq!(chars_no_space("**жирный**"), 6);
        assert_eq!(chars_no_space("a---b"), 2);
        assert_eq!(chars_no_space("a\rb"), 2);
        // `#######` keeps one `#`: `#{1,6}` is capped.
        assert_eq!(chars_no_space("####### семь"), 5);
        // A heading marker only counts at a line start.
        assert_eq!(chars_no_space("a#b"), 3);
        assert_eq!(chars_no_space("x\n## y"), 2);
        // `\s*` after the hashes is greedy across newlines, so both markers go.
        assert_eq!(chars_no_space("#\n#"), 0);
        // JavaScript's multiline `^` is also true after `\r` and U+2028.
        assert_eq!(chars_no_space("x\r# y"), 2);
        assert_eq!(chars_no_space("x\u{2028}# y"), 2);
    }

    #[test]
    fn word_count_matches_js_token_count() {
        assert_eq!(word_count(""), 0);
        assert_eq!(word_count("   \n\t"), 0);
        assert_eq!(word_count("# Глава 1\n\nОн пришёл."), 4);
        assert_eq!(word_count("**раз** два --- три"), 3);
        // Removing `---` joins the neighbours into one token.
        assert_eq!(word_count("два---три"), 1);
    }

    #[test]
    fn chapter_file_names_are_three_digit_and_slash_separated() {
        assert_eq!(pad_start(1, 3), "001");
        assert_eq!(pad_start(12, 3), "012");
        assert_eq!(pad_start(1234, 3), "1234");
        assert_eq!(
            Path::new("chapters").join(format!("ch{}.md", pad_start(7, 3))),
            PathBuf::from("chapters").join("ch007.md")
        );
    }

    #[test]
    fn relative_escape_is_normalized_lexically() {
        assert_eq!(
            normalize_relative(Path::new("chapters/./ch001.md")),
            PathBuf::from("chapters/ch001.md")
        );
        assert_eq!(
            normalize_relative(Path::new("chapters/../chapters/ch001.md")),
            PathBuf::from("chapters/ch001.md")
        );
        assert_eq!(
            normalize_relative(Path::new("../outside.md")),
            PathBuf::from("../outside.md")
        );
        assert_eq!(normalize_relative(Path::new("..")), PathBuf::from(".."));
    }

    /// The whole save lifecycle against a throwaway database and book folder:
    /// revision gate, versioning only once a file exists, atomic write into
    /// `chapters/chNNN.md`, forced `написано`, FTS mirror, unchanged shortcut,
    /// `missing`, and the path guard.
    #[test]
    fn save_versions_the_previous_body_and_mirrors_fts() {
        let dir = std::env::temp_dir().join(format!("storyforge-chapter-{}", temp_name()));
        std::fs::create_dir_all(&dir).expect("temp dir");
        {
            let db = Db::open(dir.join("story.db")).expect("open database");
            let book_id = db
                .with_conn(|conn| {
                    json_sql::insert(
                        conn,
                        "INSERT INTO book (slug, title, root_path) VALUES (?,?,?)",
                        &[json!("demo"), json!("Demo"), json!(dir.to_string_lossy())],
                    )
                })
                .expect("insert book");
            let chapter_id = db
                .with_conn(|conn| {
                    json_sql::insert(
                        conn,
                        "INSERT INTO chapter (book_id, number) VALUES (?,?)",
                        &[json!(book_id), json!(1)],
                    )
                })
                .expect("insert chapter");

            let version_count = |db: &Db| -> i64 {
                db.with_conn(|conn| {
                    json_sql::scalar_i64(
                        conn,
                        "SELECT COUNT(*) FROM chapter_version WHERE chapter_id=?",
                        &[json!(chapter_id)],
                    )
                })
                .expect("version count")
            };

            let first = db
                .with_conn(|conn| read_chapter_text(conn, chapter_id))
                .expect("read empty chapter");
            assert_eq!(first.body, "");
            assert_ne!(first.revision, "");
            // No `file_path` yet: empty prose is not "missing".
            assert!(!first.missing);

            let saved = save_chapter_text(
                &db,
                chapter_id,
                "Первый текст",
                &SaveOptions {
                    expected_revision: Some(first.revision.clone()),
                    ..SaveOptions::default()
                },
            )
            .expect("first save");
            assert!(!saved.unchanged);
            assert_eq!(saved.revision, revision_of("Первый текст"));

            let prose = dir.join("chapters").join("ch001.md");
            assert_eq!(
                std::fs::read_to_string(&prose).expect("prose file"),
                "Первый текст"
            );
            // The file did not exist before, so there is nothing to version.
            assert_eq!(version_count(&db), 0);

            let stale = save_chapter_text(
                &db,
                chapter_id,
                "Потерянная правка",
                &SaveOptions {
                    expected_revision: Some(first.revision.clone()),
                    ..SaveOptions::default()
                },
            );
            assert!(matches!(stale, Err(SfError::Conflict(_))));
            assert_eq!(
                std::fs::read_to_string(&prose).expect("prose file"),
                "Первый текст"
            );

            let second = save_chapter_text(
                &db,
                chapter_id,
                "Второй текст",
                &SaveOptions {
                    expected_revision: Some(saved.revision.clone()),
                    ..SaveOptions::default()
                },
            )
            .expect("second save");
            assert_eq!(version_count(&db), 1);
            let version = db
                .with_conn(|conn| {
                    json_sql::query_one(
                        conn,
                        "SELECT body, reason FROM chapter_version WHERE chapter_id=?",
                        &[json!(chapter_id)],
                    )
                })
                .expect("version row")
                .expect("version exists");
            assert_eq!(version["body"], json!("Первый текст"));
            assert_eq!(version["reason"], json!(REASON_MANUAL));

            // Saving the same body again writes neither a version nor the DB.
            let again = save_chapter_text(&db, chapter_id, "Второй текст", &SaveOptions::default())
                .expect("unchanged save");
            assert!(again.unchanged);
            assert_eq!(again.revision, second.revision);
            assert_eq!(version_count(&db), 1);

            let row = db
                .with_conn(|conn| {
                    json_sql::query_one(
                        conn,
                        "SELECT file_path, status, chars_no_space, words FROM chapter WHERE id=?",
                        &[json!(chapter_id)],
                    )
                })
                .expect("chapter row")
                .expect("chapter exists");
            assert_eq!(row["file_path"], json!("chapters/ch001.md"));
            assert_eq!(row["status"], json!("написано"));
            assert_eq!(row["chars_no_space"], json!(chars_no_space("Второй текст")));
            assert_eq!(row["words"], json!(word_count("Второй текст")));

            let indexed = db
                .with_conn(|conn| {
                    json_sql::scalar_i64(
                        conn,
                        "SELECT COUNT(*) FROM chapter_search WHERE chapter_id=?",
                        &[json!(chapter_id)],
                    )
                })
                .expect("fts count");
            assert_eq!(indexed, 1);

            // `index_book_chapters` rebuilds the same row from the file.
            db.with_conn(|conn| {
                json_sql::exec(conn, "DELETE FROM chapter_search", &[])?;
                index_book_chapters(conn, book_id)
            })
            .expect("reindex");
            assert_eq!(indexed, 1);

            std::fs::remove_file(&prose).expect("remove prose");
            let missing = db
                .with_conn(|conn| read_chapter_text(conn, chapter_id))
                .expect("read missing chapter");
            assert!(missing.missing);
            assert_eq!(missing.body, "");

            // A `file_path` that leaves the book folder is refused with 400.
            for escape in ["../escape.md", "/absolute.md"] {
                db.with_conn(|conn| {
                    json_sql::exec(
                        conn,
                        "UPDATE chapter SET file_path=? WHERE id=?",
                        &[json!(escape), json!(chapter_id)],
                    )
                })
                .expect("patch file_path");
                let refused = db.with_conn(|conn| read_chapter_text(conn, chapter_id));
                assert!(matches!(refused, Err(SfError::Invalid(_))), "{escape}");
            }
        }
        let _ = std::fs::remove_dir_all(&dir);
    }
}

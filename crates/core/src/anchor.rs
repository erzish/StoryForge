//! Text anchoring for chapter notes — a port of `server/lib/anchor.js`.
//!
//! A note stores the quoted fragment plus roughly forty characters of context
//! on either side. The prose file is the source of truth, so positions are
//! re-derived from the quote on every read; the stored `start_pos`/`end_pos`
//! are only the first hint. The four-step ladder lives in [`locate_note`], and
//! it reports *how* a fragment was found and how many times it occurs instead
//! of silently picking one.
//!
//! # Offsets are UTF-16 code units
//!
//! JavaScript string indices count UTF-16 code units, and the editor sends
//! selection offsets in the same units. Everything public here — paragraph
//! ranges, `find_flexible` matches, `locate_note` positions and planned edit
//! starts — therefore counts UTF-16 code units, not bytes and not `char`s. An
//! astral character (an emoji) is two units; the accompanying tests pin this
//! down.
//!
//! # JavaScript details kept on purpose
//!
//! * Whitespace is ECMAScript's `\s` (WhiteSpace plus LineTerminator): it
//!   includes U+FEFF and excludes U+0085, which is the opposite of the Unicode
//!   `White_Space` property. `str::trim` and the `regex` crate would both be
//!   subtly wrong, so the matcher is hand-written over UTF-16 units.
//! * `findFlexible` counts exact, non-overlapping occurrences first and only
//!   then falls back to a whitespace-tolerant pattern built from the trimmed
//!   needle. That pattern is a sequence of literal runs separated by `\s+`;
//!   matching it needs no backtracking because literal runs contain no
//!   whitespace.
//! * Rejection reasons are wire strings the UI displays; they are reproduced
//!   verbatim below.
//!
//! # Deliberate limits
//!
//! `plan_edits` expects JSON edits and JSON note rows, the shapes the HTTP
//! routes already produce. A malformed edit item (a JSON `null`, say) is
//! treated as an empty object; the JavaScript edition would throw while
//! reading `edit.old`.

use std::collections::HashMap;

use serde_json::{Map, Value, json};

/// One paragraph of prose, with its half-open range in UTF-16 code units.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Paragraph {
    pub start: usize,
    pub end: usize,
    pub text: String,
}

/// Result of [`find_flexible`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FlexibleMatch {
    pub start: usize,
    pub end: usize,
    pub count: usize,
    /// The needle matched literally, without the whitespace-tolerant fallback.
    pub exact: bool,
}

/// A note resolved against the current chapter text.
///
/// For a lost anchor (`found == false`) only `found` and `occurrences` carry
/// meaning; `how` is `None` and the paragraph fields stay empty, matching the
/// JavaScript `{found:false, occurrences:0, how:null}` result.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct LocatedNote {
    pub found: bool,
    pub how: Option<&'static str>,
    pub start: Option<usize>,
    pub end: Option<usize>,
    pub occurrences: usize,
    pub unique: bool,
    pub paragraph_index: Option<usize>,
    pub paragraph_count: usize,
    /// Spanned paragraphs joined with a blank line; `None` when unknown or
    /// empty.
    pub paragraph: Option<String>,
    pub before: Option<String>,
    pub after: Option<String>,
}

/// A validated replacement, ready for [`apply_planned`].
#[derive(Debug, Clone, PartialEq)]
pub struct PlannedEdit {
    /// Raw `annotation_id` value as sent by the client, when the key existed.
    /// `None` means the key was absent (JavaScript `undefined`), which is
    /// different from an explicit JSON `null`.
    pub annotation_id: Option<Value>,
    pub old: String,
    pub new: String,
    /// Start offset of `old` in the chapter text, in UTF-16 code units.
    pub start: usize,
    /// The original edit object, kept so an overlap rejection can echo it.
    raw: Value,
}

impl PlannedEdit {
    fn old_len(&self) -> usize {
        self.old.encode_utf16().count()
    }
}

/// Outcome of [`plan_edits`]: what may be applied and what the UI must show.
#[derive(Debug, Clone, Default)]
pub struct PlanOutcome {
    pub planned: Vec<PlannedEdit>,
    pub rejected: Vec<Value>,
}

// ─────────────────────────── UTF-16 helpers ───────────────────────────

fn utf16_units(text: &str) -> Vec<u16> {
    text.encode_utf16().collect()
}

fn from_units(units: &[u16]) -> String {
    String::from_utf16_lossy(units)
}

/// ECMAScript `\s`: WhiteSpace plus LineTerminator.
fn is_js_whitespace(unit: u16) -> bool {
    matches!(
        unit,
        0x0009
            | 0x000A
            | 0x000B
            | 0x000C
            | 0x000D
            | 0x0020
            | 0x00A0
            | 0x1680
            | 0x2000..=0x200A
            | 0x2028
            | 0x2029
            | 0x202F
            | 0x205F
            | 0x3000
            | 0xFEFF
    )
}

/// `string.trim()`, including its definition of whitespace.
fn trim_units(units: &[u16]) -> &[u16] {
    let mut start = 0;
    while start < units.len() && is_js_whitespace(units[start]) {
        start += 1;
    }
    let mut end = units.len();
    while end > start && is_js_whitespace(units[end - 1]) {
        end -= 1;
    }
    &units[start..end]
}

fn is_blank(units: &[u16]) -> bool {
    units.iter().copied().all(is_js_whitespace)
}

/// `countOccurrences`: non-overlapping, advancing by the needle length.
fn count_occurrences(haystack: &[u16], needle: &[u16]) -> usize {
    if needle.is_empty() {
        return 0;
    }
    let mut count = 0;
    let mut index = 0;
    while index + needle.len() <= haystack.len() {
        if haystack[index..index + needle.len()] == *needle {
            count += 1;
            index += needle.len();
        } else {
            index += 1;
        }
    }
    count
}

/// `haystack.indexOf(needle, from)`.
fn index_of_from(haystack: &[u16], needle: &[u16], from: usize) -> Option<usize> {
    if needle.is_empty() || needle.len() > haystack.len() {
        return None;
    }
    let last = haystack.len() - needle.len();
    for index in from..=last {
        if haystack[index..index + needle.len()] == *needle {
            return Some(index);
        }
    }
    None
}

// ─────────────────────────── paragraphs ───────────────────────────

/// Split text on blank lines (`/\n\s*\n/g`) and drop blank paragraphs.
///
/// Ranges are half-open and count UTF-16 code units. A paragraph that is only
/// whitespace is dropped entirely, so whitespace-only text yields `[]`.
pub fn paragraphs(text: &str) -> Vec<Paragraph> {
    paragraphs_units(&utf16_units(text))
}

fn paragraphs_units(units: &[u16]) -> Vec<Paragraph> {
    let mut out = Vec::new();
    let mut start = 0usize;
    let mut search = 0usize;

    while search < units.len() {
        let Some(newline) = (search..units.len()).find(|&index| units[index] == 0x000A) else {
            break;
        };

        // `\s*` is greedy, then the second `\n` backtracks into the run, so a
        // match ends at the last newline of the maximal whitespace run.
        let mut run_end = newline + 1;
        while run_end < units.len() && is_js_whitespace(units[run_end]) {
            run_end += 1;
        }
        let last_newline = (newline + 1..run_end)
            .rev()
            .find(|&index| units[index] == 0x000A);

        match last_newline {
            Some(end) => {
                push_paragraph(&mut out, units, start, newline);
                start = end + 1;
                search = end + 1;
            }
            None => search = newline + 1,
        }
    }

    push_paragraph(&mut out, units, start, units.len());
    out
}

fn push_paragraph(out: &mut Vec<Paragraph>, units: &[u16], start: usize, end: usize) {
    let slice = &units[start..end];
    if is_blank(slice) {
        return;
    }
    out.push(Paragraph {
        start,
        end,
        text: from_units(slice),
    });
}

// ─────────────────────────── flexible search ───────────────────────────

/// Search a fragment tolerantly: exact occurrences first, then a pattern that
/// treats every whitespace run as `\s+`.
///
/// Returns `None` when the fragment is empty or absent, mirroring the
/// JavaScript `{start:-1,end:-1,count:0}` result.
pub fn find_flexible(text: &str, needle: &str) -> Option<FlexibleMatch> {
    find_flexible_units(&utf16_units(text), &utf16_units(needle))
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum Segment {
    Literal(Vec<u16>),
    /// One or more JavaScript whitespace units.
    Whitespace,
}

fn find_flexible_units(haystack: &[u16], needle: &[u16]) -> Option<FlexibleMatch> {
    if needle.is_empty() {
        return None;
    }

    let exact = count_occurrences(haystack, needle);
    if exact > 0 {
        let start = index_of_from(haystack, needle, 0)?;
        return Some(FlexibleMatch {
            start,
            end: start + needle.len(),
            count: exact,
            exact: true,
        });
    }

    // `needle.trim().replace(/\s+/g, '\\s+')` with every metacharacter escaped
    // is, for a literal matcher, just the trimmed needle split at whitespace.
    let trimmed = trim_units(needle);
    let mut segments = Vec::new();
    let mut index = 0;
    while index < trimmed.len() {
        if is_js_whitespace(trimmed[index]) {
            while index < trimmed.len() && is_js_whitespace(trimmed[index]) {
                index += 1;
            }
            segments.push(Segment::Whitespace);
        } else {
            let start = index;
            while index < trimmed.len() && !is_js_whitespace(trimmed[index]) {
                index += 1;
            }
            segments.push(Segment::Literal(trimmed[start..index].to_vec()));
        }
    }

    // A whitespace-only needle trims to the empty pattern, which JavaScript's
    // global regex matches at every position (including the end).
    if segments.is_empty() {
        return Some(FlexibleMatch {
            start: 0,
            end: 0,
            count: haystack.len() + 1,
            exact: false,
        });
    }

    let mut first: Option<(usize, usize)> = None;
    let mut count = 0;
    let mut index = 0;
    while index <= haystack.len() {
        match match_at(haystack, index, &segments) {
            Some(end) => {
                if first.is_none() {
                    first = Some((index, end));
                }
                count += 1;
                // A non-empty pattern always advances; the empty case above
                // already returned.
                index = end;
            }
            None => index += 1,
        }
    }

    first.map(|(start, end)| FlexibleMatch {
        start,
        end,
        count,
        exact: false,
    })
}

fn match_at(haystack: &[u16], start: usize, segments: &[Segment]) -> Option<usize> {
    let mut cursor = start;
    for segment in segments {
        match segment {
            Segment::Literal(literal) => {
                if cursor + literal.len() > haystack.len() {
                    return None;
                }
                if haystack[cursor..cursor + literal.len()] != literal[..] {
                    return None;
                }
                cursor += literal.len();
            }
            Segment::Whitespace => {
                let run_start = cursor;
                while cursor < haystack.len() && is_js_whitespace(haystack[cursor]) {
                    cursor += 1;
                }
                if cursor == run_start {
                    return None;
                }
            }
        }
    }
    Some(cursor)
}

// ─────────────────────────── note location ───────────────────────────

/// Find a note's fragment in the current text.
///
/// Attempts, in order:
///
/// 1. stored coordinates — `text.slice(start_pos, end_pos) == quote` (`"позиция"`);
/// 2. `prefix + quote + suffix` found flexibly, with the quote rebased inside
///    it (`"контекст"`);
/// 3. `prefix + quote` found flexibly (non-empty prefix only) (`"префикс"`);
/// 4. the quote alone (`"цитата"` exactly, `"цитата (с допуском)"` after
///    whitespace-tolerant matching).
///
/// A lost anchor reports `found: false`, `occurrences: 0` and `how: None`
/// rather than guessing a position.
pub fn locate_note(text: &str, note: &Value) -> LocatedNote {
    locate_note_units(&utf16_units(text), note)
}

fn note_string<'a>(note: &'a Value, key: &str) -> &'a str {
    note.get(key).and_then(Value::as_str).unwrap_or("")
}

fn note_integer(note: &Value, key: &str) -> Option<i64> {
    match note.get(key) {
        Some(Value::Number(number)) => number
            .as_i64()
            .or_else(|| number.as_f64().map(|value| value.trunc() as i64)),
        Some(Value::String(value)) => value.trim().parse().ok(),
        _ => None,
    }
}

/// JavaScript `String#slice` bounds. A `null` offset (the column's usual
/// value) behaves as `0`, which is what `slice(null, …)` does.
fn js_slice_bounds(len: usize, start: Option<i64>, end: Option<i64>) -> (usize, usize) {
    (clamp_index(start.unwrap_or(0), len), clamp_index(end.unwrap_or(0), len))
}

fn clamp_index(value: i64, len: usize) -> usize {
    let len = len as i64;
    if value < 0 {
        (len + value).max(0) as usize
    } else {
        value.min(len) as usize
    }
}

fn locate_note_units(units: &[u16], note: &Value) -> LocatedNote {
    let quote = note_string(note, "quote");
    let prefix = note_string(note, "prefix");
    let suffix = note_string(note, "suffix");
    let quote_units = utf16_units(quote);

    let mut start = None;
    let mut end = None;
    let mut how = None;

    if !quote.is_empty() {
        let (slice_start, slice_end) = js_slice_bounds(
            units.len(),
            note_integer(note, "start_pos"),
            note_integer(note, "end_pos"),
        );
        if slice_start < slice_end
            && units[slice_start..slice_end].len() == quote_units.len()
            && units[slice_start..slice_end] == quote_units[..]
        {
            start = Some(slice_start);
            how = Some("позиция");
        }
    }

    if start.is_none() && !quote.is_empty() && !prefix.is_empty() && !suffix.is_empty() {
        let mut context = utf16_units(prefix);
        context.extend_from_slice(&quote_units);
        context.extend(utf16_units(suffix));
        if let Some(found) = find_flexible_units(units, &context)
            && let Some(inner) = find_flexible_units(&units[found.start..found.end], &quote_units)
        {
            start = Some(found.start + inner.start);
            end = Some(found.start + inner.end);
            how = Some("контекст");
        }
    }

    if start.is_none() && !quote.is_empty() && !prefix.is_empty() {
        let mut context = utf16_units(prefix);
        context.extend_from_slice(&quote_units);
        if let Some(found) = find_flexible_units(units, &context)
            && let Some(inner) = find_flexible_units(&units[found.start..found.end], &quote_units)
        {
            start = Some(found.start + inner.start);
            end = Some(found.start + inner.end);
            how = Some("префикс");
        }
    }

    if start.is_none() && !quote.is_empty()
        && let Some(found) = find_flexible_units(units, &quote_units)
    {
        start = Some(found.start);
        end = Some(found.end);
        how = Some(if found.exact {
            "цитата"
        } else {
            "цитата (с допуском)"
        });
    }

    let Some(start) = start else {
        return LocatedNote {
            found: false,
            how: None,
            occurrences: 0,
            ..LocatedNote::default()
        };
    };

    // Step 1 only records the start; its end comes from the quote length.
    let end = end.unwrap_or(start + quote_units.len());

    let paragraphs = paragraphs_units(units);
    let from = paragraphs
        .iter()
        .position(|paragraph| start >= paragraph.start && start <= paragraph.end);
    let to_index = paragraphs
        .iter()
        .position(|paragraph| end >= paragraph.start && end <= paragraph.end);
    let last = match to_index {
        Some(index) => Some(from.map_or(index, |from| from.max(index))),
        None => from,
    };

    let spanned = match from {
        Some(first) => last.map_or(&[][..], |last| &paragraphs[first..=last]),
        None => &[][..],
    };
    let joined = spanned
        .iter()
        .map(|paragraph| paragraph.text.as_str())
        .collect::<Vec<_>>()
        .join("\n\n");
    let paragraph = if joined.is_empty() {
        None
    } else {
        Some(joined)
    };

    let before = from
        .filter(|index| *index > 0)
        .and_then(|index| paragraphs.get(index - 1))
        .map(|paragraph| paragraph.text.clone());
    let after = last
        .and_then(|index| paragraphs.get(index + 1))
        .map(|paragraph| paragraph.text.clone());

    let occurrences = count_occurrences(units, &quote_units).max(1);

    LocatedNote {
        found: true,
        how,
        start: Some(start),
        end: Some(end),
        occurrences,
        unique: occurrences == 1,
        paragraph_index: from,
        paragraph_count: spanned.len(),
        paragraph,
        before,
        after,
    }
}

// ─────────────────────────── edit planning ───────────────────────────

/// Validate replacements against the current text.
///
/// Edits are considered in input order. An edit is rejected when:
///
/// * `old` or `new` is not a string (`old и new должны быть строками`);
/// * its `annotation_id` names a note that is not in `notes_by_id`
///   (`заметка не принадлежит этой главе`);
/// * `old` is empty (`не указан исходный текст`);
/// * `old` does not occur (`фрагмент не найден в главе`);
/// * `old` occurs several times and neither the note prefix nor the note's
///   neighbourhood narrows it down
///   (`фрагмент встречается N раз — неоднозначно`);
/// * its range overlaps an edit already accepted (`пересекается с другой правкой`).
///
/// Overlap rejection runs after planning, on edits sorted by `start`, so of two
/// overlapping fragments the earlier one wins.
pub fn plan_edits(
    text: &str,
    edits: &[Value],
    notes_by_id: &HashMap<i64, Value>,
) -> PlanOutcome {
    let units = utf16_units(text);
    let mut planned = Vec::new();
    let mut rejected = Vec::new();

    for edit in edits {
        let raw = edit_object(edit);
        let (old, new) = match (edit_string(edit, "old"), edit_string(edit, "new")) {
            (Some(old), Some(new)) => (old, new),
            _ => {
                rejected.push(with_reason(&raw, "old и new должны быть строками"));
                continue;
            }
        };

        let annotation_id = edit.get("annotation_id").cloned();
        let note = match annotation_id.as_ref() {
            None | Some(Value::Null) => None,
            Some(value) => match integral_number(value).and_then(|id| notes_by_id.get(&id)) {
                Some(note) => Some(note),
                None => {
                    rejected.push(with_reason(&raw, "заметка не принадлежит этой главе"));
                    continue;
                }
            },
        };

        if old.is_empty() {
            rejected.push(with_reason(&raw, "не указан исходный текст"));
            continue;
        }

        let old_units = utf16_units(&old);
        let count = count_occurrences(&units, &old_units);

        if count == 1 {
            if let Some(start) = index_of_from(&units, &old_units, 0) {
                planned.push(PlannedEdit {
                    annotation_id,
                    old,
                    new,
                    start,
                    raw,
                });
            }
            continue;
        }

        if count == 0 {
            rejected.push(with_reason(&raw, "фрагмент не найден в главе"));
            continue;
        }

        // The fragment occurs more than once: narrow it with the note itself.
        let mut resolved = None;
        if let Some(note) = note {
            if let Some(prefix) = note
                .get("prefix")
                .and_then(Value::as_str)
                .filter(|prefix| !prefix.is_empty())
            {
                let prefix_units = utf16_units(prefix);
                let mut needle = prefix_units.clone();
                needle.extend_from_slice(&old_units);
                if let Some(index) = index_of_from(&units, &needle, 0) {
                    resolved = Some(index + prefix_units.len());
                }
            }
            if resolved.is_none() && !note_string(note, "quote").is_empty() {
                let anchor = locate_note_units(&units, note);
                if anchor.found {
                    let anchor_start = anchor.start.unwrap_or(0);
                    let anchor_end = anchor.end.unwrap_or(anchor_start);
                    if let Some(near) = index_of_from(
                        &units,
                        &old_units,
                        anchor_start.saturating_sub(400),
                    ) && near < anchor_end + 400
                    {
                        resolved = Some(near);
                    }
                }
            }
        }

        match resolved {
            Some(start) => planned.push(PlannedEdit {
                annotation_id,
                old,
                new,
                start,
                raw,
            }),
            None => rejected.push(with_reason(
                &raw,
                &format!("фрагмент встречается {count} раз — неоднозначно"),
            )),
        }
    }

    // Even exact matches cannot coexist when their ranges overlap. Sorting is
    // stable, so equal starts keep input order and the first one wins.
    planned.sort_by_key(|edit| edit.start);
    let mut safe = Vec::new();
    let mut last_end = None;
    for edit in planned {
        if last_end.is_some_and(|end| edit.start < end) {
            rejected.push(overlap_rejection(&edit));
        } else {
            last_end = Some(edit.start + edit.old_len());
            safe.push(edit);
        }
    }

    PlanOutcome {
        planned: safe,
        rejected,
    }
}

/// Apply planned edits from the end so earlier offsets stay valid.
///
/// Positions are UTF-16 code units. A planned edit whose range is outside the
/// text is skipped; [`plan_edits`] never produces one.
pub fn apply_planned(text: &str, planned: &[PlannedEdit]) -> String {
    let mut units = utf16_units(text);
    let mut order: Vec<&PlannedEdit> = planned.iter().collect();
    order.sort_by_key(|edit| std::cmp::Reverse(edit.start));

    for edit in order {
        let old_len = edit.old_len();
        let start = edit.start;
        if start > units.len() || start + old_len > units.len() {
            continue;
        }
        units.splice(start..start + old_len, utf16_units(&edit.new));
    }

    from_units(&units)
}

/// `{...edit}` for an object; a non-object contributes no fields.
fn edit_object(edit: &Value) -> Value {
    if edit.is_object() {
        edit.clone()
    } else {
        Value::Object(Map::new())
    }
}

/// `edit[key] ?? ''`, with `None` reserved for "not a string".
fn edit_string(edit: &Value, key: &str) -> Option<String> {
    match edit.get(key) {
        None | Some(Value::Null) => Some(String::new()),
        Some(Value::String(value)) => Some(value.clone()),
        Some(_) => None,
    }
}

fn with_reason(edit: &Value, reason: &str) -> Value {
    let mut value = edit.clone();
    if let Some(map) = value.as_object_mut() {
        map.insert("reason".into(), json!(reason));
    }
    value
}

/// `{...planned, reason}` — overlaps echo the computed position too.
fn overlap_rejection(edit: &PlannedEdit) -> Value {
    let mut value = edit.raw.clone();
    if let Some(map) = value.as_object_mut() {
        map.insert("start".into(), json!(edit.start));
        map.insert("old".into(), json!(edit.old));
        map.insert("new".into(), json!(edit.new));
        map.insert("reason".into(), json!("пересекается с другой правкой"));
    }
    value
}

/// `Number(value)` reduced to an integral map key.
fn integral_number(value: &Value) -> Option<i64> {
    match value {
        Value::Number(number) => number.as_i64().or_else(|| {
            number
                .as_f64()
                .filter(|value| value.is_finite() && value.fract() == 0.0)
                .map(|value| value as i64)
        }),
        Value::String(text) => {
            let text = text.trim();
            if text.is_empty() {
                Some(0)
            } else {
                text.parse::<i64>().ok().or_else(|| {
                    text.parse::<f64>()
                        .ok()
                        .filter(|value| value.is_finite() && value.fract() == 0.0)
                        .map(|value| value as i64)
                })
            }
        }
        Value::Bool(flag) => Some(i64::from(*flag)),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn note(value: Value) -> Value {
        value
    }

    #[test]
    fn stored_position_wins_over_context() {
        let text = "Первая строка.\n\nВторая строка с цитатой.";
        let located = locate_note(
            text,
            &note(json!({
                "quote": "цитатой",
                "prefix": "с ",
                "suffix": ".",
                "start_pos": 32,
                "end_pos": 39,
            })),
        );

        assert!(located.found);
        assert_eq!(located.how, Some("позиция"));
        assert_eq!(located.start, Some(32));
        assert_eq!(located.end, Some(39));
        assert_eq!(located.occurrences, 1);
        assert!(located.unique);
        assert_eq!(located.paragraph_index, Some(1));
        assert_eq!(located.paragraph_count, 1);
        assert_eq!(
            located.paragraph.as_deref(),
            Some("Вторая строка с цитатой.")
        );
        assert_eq!(located.before.as_deref(), Some("Первая строка."));
        assert_eq!(located.after, None);
    }

    #[test]
    fn exact_quote_is_found_without_positions() {
        let text = "Первая строка.\n\nВторая строка с цитатой.";
        let located = locate_note(
            text,
            &note(json!({
                "quote": "Вторая строка",
                "prefix": null,
                "suffix": null,
                "start_pos": null,
                "end_pos": null,
            })),
        );

        assert!(located.found);
        assert_eq!(located.how, Some("цитата"));
        assert_eq!(located.start, Some(16));
        assert_eq!(located.paragraph_index, Some(1));
    }

    #[test]
    fn whitespace_differences_are_tolerated() {
        let text = "Он сказал:\n   «Привет, мир».\n\nКонец.";
        let located = locate_note(
            text,
            &note(json!({
                "quote": "сказал: «Привет",
                "prefix": null,
                "suffix": null,
                "start_pos": null,
                "end_pos": null,
            })),
        );

        assert!(located.found);
        assert_eq!(located.how, Some("цитата (с допуском)"));
        assert_eq!(located.start, Some(3));
        let start = located.start.unwrap();
        let end = located.end.unwrap();
        let matched: String = text
            .encode_utf16()
            .skip(start)
            .take(end - start)
            .map(|unit| char::from_u32(u32::from(unit)).unwrap())
            .collect();
        assert_eq!(matched, "сказал:\n   «Привет");
        assert_eq!(located.occurrences, 1);
    }

    #[test]
    fn context_and_prefix_steps_are_used_before_the_bare_quote() {
        let text = "до фрагмент после";
        let with_context = locate_note(
            text,
            &note(json!({
                "quote": "фрагмент",
                "prefix": "до ",
                "suffix": " после",
                "start_pos": null,
                "end_pos": null,
            })),
        );
        assert_eq!(with_context.how, Some("контекст"));
        assert_eq!(with_context.start, Some(3));

        // A suffix that no longer matches forces the prefix-only attempt.
        let with_prefix = locate_note(
            text,
            &note(json!({
                "quote": "фрагмент",
                "prefix": "до ",
                "suffix": " исчез",
                "start_pos": null,
                "end_pos": null,
            })),
        );
        assert_eq!(with_prefix.how, Some("префикс"));
        assert_eq!(with_prefix.start, Some(3));
    }

    #[test]
    fn ambiguity_is_reported_not_guessed() {
        let text = "abc abc";
        let located = locate_note(
            text,
            &note(json!({
                "quote": "abc",
                "prefix": null,
                "suffix": null,
                "start_pos": null,
                "end_pos": null,
            })),
        );
        assert_eq!(located.occurrences, 2);
        assert!(!located.unique);

        let plan = plan_edits(
            text,
            &[json!({ "old": "abc", "new": "x" })],
            &HashMap::new(),
        );
        assert!(plan.planned.is_empty());
        assert_eq!(plan.rejected.len(), 1);
        assert_eq!(
            plan.rejected[0]["reason"],
            json!("фрагмент встречается 2 раз — неоднозначно")
        );
    }

    #[test]
    fn ambiguity_is_narrowed_by_the_note_prefix() {
        let text = "one abc two abc";
        let mut notes = HashMap::new();
        notes.insert(
            1,
            note(json!({
                "id": 1,
                "quote": "abc",
                "prefix": "one ",
                "suffix": " two",
                "start_pos": null,
                "end_pos": null,
            })),
        );

        let plan = plan_edits(
            text,
            &[json!({ "annotation_id": 1, "old": "abc", "new": "X" })],
            &notes,
        );
        assert_eq!(plan.planned.len(), 1);
        assert_eq!(plan.planned[0].start, 4);
        assert_eq!(
            apply_planned(text, &plan.planned),
            "one X two abc".to_string()
        );
    }

    #[test]
    fn overlapping_edits_keep_only_the_first() {
        let text = "abcd";
        let plan = plan_edits(
            text,
            &[
                json!({ "old": "abc", "new": "X" }),
                json!({ "old": "bcd", "new": "Y" }),
            ],
            &HashMap::new(),
        );

        assert_eq!(plan.planned.len(), 1);
        assert_eq!(plan.planned[0].old, "abc");
        assert_eq!(plan.rejected.len(), 1);
        assert_eq!(
            plan.rejected[0]["reason"],
            json!("пересекается с другой правкой")
        );
        assert_eq!(plan.rejected[0]["start"], json!(1));
        assert_eq!(apply_planned(text, &plan.planned), "Xd".to_string());
    }

    #[test]
    fn lost_anchor_reports_failure_instead_of_a_position() {
        let text = "Совсем другой текст.";
        let located = locate_note(
            text,
            &note(json!({
                "quote": "исчезнувшая цитата",
                "prefix": "до ",
                "suffix": " после",
                "start_pos": 0,
                "end_pos": 5,
            })),
        );

        assert!(!located.found);
        assert_eq!(located.how, None);
        assert_eq!(located.occurrences, 0);
        assert!(!located.unique);
        assert_eq!(located.start, None);
        assert_eq!(located.end, None);
        assert_eq!(located.paragraph_index, None);
        assert_eq!(located.paragraph_count, 0);
        assert!(located.paragraph.is_none());
        assert!(located.before.is_none());
        assert!(located.after.is_none());

        let plan = plan_edits(
            text,
            &[json!({ "old": "исчезнувшая цитата", "new": "x" })],
            &HashMap::new(),
        );
        assert!(plan.planned.is_empty());
        assert_eq!(
            plan.rejected[0]["reason"],
            json!("фрагмент не найден в главе")
        );
    }

    #[test]
    fn offsets_count_utf16_code_units() {
        // The emoji is two code units, so `abc` starts at 3, not at 2 (chars)
        // or 5 (bytes).
        let text = "😀 abc";
        let located = locate_note(
            text,
            &note(json!({
                "quote": "abc",
                "start_pos": 3,
                "end_pos": 6,
            })),
        );
        assert_eq!(located.how, Some("позиция"));
        assert_eq!(located.start, Some(3));
        assert_eq!(located.end, Some(6));

        let plan = plan_edits(
            text,
            &[json!({ "old": "abc", "new": "xyz" })],
            &HashMap::new(),
        );
        assert_eq!(plan.planned[0].start, 3);
        assert_eq!(apply_planned(text, &plan.planned), "😀 xyz".to_string());
    }

    #[test]
    fn edits_apply_right_to_left() {
        let text = "aXbYc";
        let plan = plan_edits(
            text,
            &[
                json!({ "old": "X", "new": "1" }),
                json!({ "old": "Y", "new": "2" }),
            ],
            &HashMap::new(),
        );
        assert_eq!(plan.planned.len(), 2);
        assert_eq!(apply_planned(text, &plan.planned), "a1b2c".to_string());
    }
}

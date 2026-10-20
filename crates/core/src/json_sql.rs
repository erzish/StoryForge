//! SQLite rows to JSON, the way the JavaScript edition worked.
//!
//! `node:sqlite` returned plain objects straight from `stmt.all()`, so route
//! handlers were little more than SQL plus a reshape. Keeping that shape in
//! Rust means the ported handlers stay equally thin and the JSON the frontend
//! sees is byte-compatible with what it saw before.

use rusqlite::types::{Value as SqlValue, ValueRef};
use rusqlite::{Connection, params_from_iter};
use serde_json::{Map, Number, Value};

use crate::error::{Result, SfError};

/// Convert JSON values into SQLite parameters.
///
/// `null` → NULL, booleans → 0/1, integers → INTEGER, floats → REAL,
/// strings → TEXT. Arrays and objects are stored as their JSON text, which is
/// what the JavaScript code did implicitly for column values it passed through.
pub fn to_sql(value: &Value) -> SqlValue {
    match value {
        Value::Null => SqlValue::Null,
        Value::Bool(b) => SqlValue::Integer(i64::from(*b)),
        Value::Number(n) => {
            if let Some(i) = n.as_i64() {
                SqlValue::Integer(i)
            } else if let Some(f) = n.as_f64() {
                SqlValue::Real(f)
            } else {
                SqlValue::Null
            }
        }
        Value::String(s) => SqlValue::Text(s.clone()),
        other => SqlValue::Text(other.to_string()),
    }
}

fn param_list(params: &[Value]) -> Vec<SqlValue> {
    params.iter().map(to_sql).collect()
}

/// `SELECT` returning every row as a JSON object keyed by column name.
pub fn query(conn: &Connection, sql: &str, params: &[Value]) -> Result<Vec<Value>> {
    let mut stmt = conn.prepare(sql)?;
    let names: Vec<String> = stmt
        .column_names()
        .iter()
        .map(|name| (*name).to_string())
        .collect();
    let values = param_list(params);
    let mut rows = stmt.query(params_from_iter(values.iter()))?;
    let mut out = Vec::new();
    while let Some(row) = rows.next()? {
        out.push(row_to_json(row, &names)?);
    }
    Ok(out)
}

/// `SELECT` returning at most one row.
pub fn query_one(conn: &Connection, sql: &str, params: &[Value]) -> Result<Option<Value>> {
    Ok(query(conn, sql, params)?.into_iter().next())
}

/// A scalar `SELECT`, e.g. `SELECT COUNT(*)`.
pub fn scalar_i64(conn: &Connection, sql: &str, params: &[Value]) -> Result<i64> {
    let values = param_list(params);
    let mut stmt = conn.prepare(sql)?;
    let value = stmt.query_row(params_from_iter(values.iter()), |row| row.get::<_, i64>(0))?;
    Ok(value)
}

/// A scalar `SELECT` that may be NULL.
pub fn scalar_opt_i64(conn: &Connection, sql: &str, params: &[Value]) -> Result<Option<i64>> {
    let values = param_list(params);
    let mut stmt = conn.prepare(sql)?;
    let value = stmt.query_row(params_from_iter(values.iter()), |row| {
        row.get::<_, Option<i64>>(0)
    })?;
    Ok(value)
}

/// `INSERT`/`UPDATE`/`DELETE`, returning the affected row count.
pub fn exec(conn: &Connection, sql: &str, params: &[Value]) -> Result<usize> {
    let values = param_list(params);
    Ok(conn.execute(sql, params_from_iter(values.iter()))?)
}

/// Insert and return the new `rowid`.
pub fn insert(conn: &Connection, sql: &str, params: &[Value]) -> Result<i64> {
    let values = param_list(params);
    conn.execute(sql, params_from_iter(values.iter()))?;
    Ok(conn.last_insert_rowid())
}

/// Run several statements in one implicit transaction.
pub fn exec_batch(conn: &Connection, sql: &str) -> Result<()> {
    conn.execute_batch(sql)?;
    Ok(())
}

pub fn row_to_json(row: &rusqlite::Row<'_>, names: &[String]) -> Result<Value> {
    let mut map = Map::with_capacity(names.len());
    for (index, name) in names.iter().enumerate() {
        let value = match row.get_ref(index)? {
            ValueRef::Null => Value::Null,
            ValueRef::Integer(i) => Value::Number(Number::from(i)),
            ValueRef::Real(f) => Number::from_f64(f)
                .map(Value::Number)
                .unwrap_or(Value::Null),
            ValueRef::Text(bytes) => Value::String(String::from_utf8_lossy(bytes).into_owned()),
            ValueRef::Blob(bytes) => Value::String(base64_encode(bytes)),
        };
        map.insert(name.clone(), value);
    }
    Ok(Value::Object(map))
}

fn base64_encode(bytes: &[u8]) -> String {
    use base64::Engine;
    base64::engine::general_purpose::STANDARD.encode(bytes)
}

/// Require an object body field as an `i64`, accepting numeric strings the way
/// JavaScript's `Number()` coerced them.
pub fn num(value: &Value) -> Option<i64> {
    match value {
        Value::Number(n) => n.as_i64().or_else(|| n.as_f64().map(|f| f as i64)),
        Value::String(s) => s.trim().parse::<i64>().ok(),
        Value::Bool(b) => Some(i64::from(*b)),
        _ => None,
    }
}

/// JavaScript truthiness for the small set of values that reach it.
pub fn truthy(value: &Value) -> bool {
    match value {
        Value::Null => false,
        Value::Bool(b) => *b,
        Value::Number(n) => n.as_f64().map(|f| f != 0.0).unwrap_or(false),
        Value::String(s) => !s.is_empty(),
        Value::Array(a) => !a.is_empty(),
        Value::Object(_) => true,
    }
}

/// `String(value ?? '')` for the field accessors.
pub fn text(value: &Value) -> String {
    match value {
        Value::Null => String::new(),
        Value::String(s) => s.clone(),
        other => other.to_string(),
    }
}

/// Missing field → NULL parameter.
pub fn field(body: &Value, key: &str) -> Value {
    body.get(key).cloned().unwrap_or(Value::Null)
}

pub fn require_i64(body: &Value, key: &str) -> Result<i64> {
    num(&field(body, key))
        .ok_or_else(|| SfError::Invalid(format!("Некорректное поле «{key}»")))
}

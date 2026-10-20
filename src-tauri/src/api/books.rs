//! Books: list and detail summary (R11–R12).

use std::sync::Arc;

use axum::Json;
use axum::extract::{Path, State};
use axum::routing::get;
use serde_json::{Value, json};

use storyforge_core::json_sql::{query, query_one, scalar_i64};

use super::{ApiError, ApiResult, AppRouter, path_id};
use crate::server::AppState;

pub fn routes() -> AppRouter {
    axum::Router::new()
        .route("/api/books", get(list_books))
        .route("/api/books/{id}", get(get_book))
}

async fn list_books(State(state): State<Arc<AppState>>) -> ApiResult {
    let rows = state
        .db
        .with_conn(|conn| query(conn, "SELECT * FROM book ORDER BY title", &[]))?;
    Ok(Json(Value::Array(rows)))
}

async fn get_book(State(state): State<Arc<AppState>>, Path(id): Path<String>) -> ApiResult {
    let id = path_id(&id)?;
    state.db.with_conn(|conn| {
        let Some(book) = query_one(conn, "SELECT * FROM book WHERE id=?", &[json!(id)])? else {
            return Err(ApiError::not_found("Книга не найдена"));
        };

        let count = |table: &str| -> Result<i64, ApiError> {
            let sql = format!("SELECT COUNT(*) FROM {table} WHERE book_id=?");
            Ok(scalar_i64(conn, &sql, &[json!(id)])?)
        };

        let written = query_one(
            conn,
            "SELECT COUNT(*) AS n, COALESCE(SUM(chars_no_space),0) AS chars
               FROM chapter WHERE book_id=? AND status='написано'",
            &[json!(id)],
        )?
        .unwrap_or_else(|| json!({"n": 0, "chars": 0}));

        let mut object = book;
        if let Some(map) = object.as_object_mut() {
            map.insert(
                "counts".into(),
                json!({
                    "characters": count("character")?,
                    "chapters": count("chapter")?,
                    "events": count("event")?,
                    "relationships": count("relationship")?,
                    "secrets": count("secret")?,
                    "promises": count("promise")?,
                    "locations": count("location")?,
                    "timeline": count("timeline_entry")?,
                }),
            );
            map.insert("written".into(), written);
        }
        Ok(Json(object))
    })
}

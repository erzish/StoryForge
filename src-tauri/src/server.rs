//! Local HTTP server.
//!
//! The window loads the UI from this server, exactly like the JavaScript
//! edition did, so the ported frontend keeps using relative `/api/...` URLs.
//! MCP (`/mcp`) lives here too; a stdio transport lives in `crates/mcp`.

use std::net::TcpListener;
use std::sync::Arc;

use axum::Json;
use axum::Router;
use axum::extract::State;
use axum::http::{StatusCode, Uri, header};
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use rust_embed::RustEmbed;
use serde_json::{Value, json};

use storyforge_core::Db;

#[derive(RustEmbed)]
#[folder = "ui/"]
struct Assets;

pub struct AppState {
    pub db: Arc<Db>,
}

/// Start the server on its own runtime thread; returns immediately.
pub fn spawn(listener: TcpListener, db: Arc<Db>) {
    std::thread::Builder::new()
        .name("storyforge-http".into())
        .spawn(move || {
            let runtime = match tokio::runtime::Builder::new_multi_thread()
                .enable_all()
                .build()
            {
                Ok(runtime) => runtime,
                Err(err) => {
                    tracing::error!(%err, "cannot build tokio runtime");
                    return;
                }
            };
            runtime.block_on(async move {
                let app = router(db);
                if let Err(err) = listener.set_nonblocking(true) {
                    tracing::error!(%err, "cannot set listener non-blocking");
                    return;
                }
                let listener = match tokio::net::TcpListener::from_std(listener) {
                    Ok(listener) => listener,
                    Err(err) => {
                        tracing::error!(%err, "cannot adopt listener");
                        return;
                    }
                };
                if let Err(err) = axum::serve(listener, app).await {
                    tracing::error!(%err, "http server stopped");
                }
            });
        })
        .expect("spawn http thread");
}

pub fn router(db: Arc<Db>) -> Router {
    Router::new()
        .merge(crate::api::router())
        .route("/api/health", get(health))
        .route("/api/about", get(about))
        .fallback(static_asset)
        .with_state(Arc::new(AppState { db }))
}

async fn health(State(state): State<Arc<AppState>>) -> Json<Value> {
    let db_ok = state.db.with_conn(|conn| storyforge_core::db::ping(conn)).is_ok();
    let books = state.db.book_count().unwrap_or(-1);
    Json(json!({
        "status": if db_ok { "ok" } else { "degraded" },
        "books": books,
        "version": env!("CARGO_PKG_VERSION"),
    }))
}

async fn about(State(state): State<Arc<AppState>>) -> Json<Value> {
    let db_path = state.db.path().display().to_string();
    let version = env!("CARGO_PKG_VERSION");
    Json(json!({
        "name": "StoryForge",
        "version": version,
        "database": db_path,
        "runtime": format!("rust {}", env!("CARGO_PKG_VERSION")),
    }))
}

async fn static_asset(uri: Uri) -> Response {
    let path = uri.path().trim_start_matches('/');
    let path = if path.is_empty() { "index.html" } else { path };
    let (bytes, mime) = match Assets::get(path) {
        Some(file) => (file.data.into_owned(), mime_for(path)),
        None => match Assets::get("index.html") {
            Some(file) => (file.data.into_owned(), "text/html; charset=utf-8"),
            None => return (StatusCode::NOT_FOUND, "UI assets missing").into_response(),
        },
    };
    let mut response = bytes.into_response();
    response.headers_mut().insert(
        header::CONTENT_TYPE,
        header::HeaderValue::from_static(mime),
    );
    // The UI ships inside the executable; a stale WebView cache would serve an
    // older shell after an upgrade, so asset responses are never cached.
    response.headers_mut().insert(
        header::CACHE_CONTROL,
        header::HeaderValue::from_static("no-store"),
    );
    response
}

fn mime_for(path: &str) -> &'static str {
    match path.rsplit('.').next().unwrap_or_default() {
        "html" => "text/html; charset=utf-8",
        "css" => "text/css; charset=utf-8",
        "js" => "text/javascript; charset=utf-8",
        "json" => "application/json",
        "svg" => "image/svg+xml",
        "png" => "image/png",
        "ico" => "image/x-icon",
        "woff2" => "font/woff2",
        _ => "application/octet-stream",
    }
}


//! StoryForge core: domain model, SQLite storage, markdown import/export,
//! AI clients and the MCP tool surface.
//!
//! The crate is synchronous by design: SQLite calls are blocking, and the
//! desktop shell wraps them in `spawn_blocking` where needed.

pub mod ai;
pub mod anchor;
pub mod chapter_text;
pub mod db;
pub mod error;
pub mod export;
pub mod import;
pub mod json_sql;
pub mod mcp;
pub mod paths;
pub mod search;
pub mod state;

pub use db::Db;
pub use error::{Result, SfError};

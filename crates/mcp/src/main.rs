//! stdio MCP transport — the port of `server/mcp.js`.
//!
//! Newline-delimited JSON-RPC 2.0 on stdin, replies on stdout, everything a
//! human might read on stderr. The tool surface itself lives in
//! `storyforge_core::mcp`, so the HTTP and stdio transports cannot drift apart.

use std::io::{self, BufRead, Write};

use serde_json::Value;

use storyforge_core::{Db, mcp};

fn main() {
    tracing_subscriber::fmt()
        .with_writer(io::stderr)
        .with_target(false)
        .init();

    let db = match Db::open_default() {
        Ok(db) => db,
        Err(err) => {
            eprintln!("storyforge-mcp: не удалось открыть базу данных: {err}");
            std::process::exit(1);
        }
    };

    let stdin = io::stdin();
    let mut reader = stdin.lock();
    let mut stdout = io::stdout().lock();
    let mut line: Vec<u8> = Vec::new();

    loop {
        line.clear();
        match reader.read_until(b'\n', &mut line) {
            Ok(0) => break,
            Ok(_) => {}
            Err(err) => {
                eprintln!("storyforge-mcp: ошибка чтения stdin: {err}");
                break;
            }
        }
        let line = String::from_utf8_lossy(&line);
        if line.trim().is_empty() {
            continue;
        }
        let Ok(message) = serde_json::from_str::<Value>(line.trim()) else {
            continue;
        };
        let result = mcp::dispatch(&db, &message);
        if !result.has_reply {
            continue;
        }
        let Some(payload) = result.payload else {
            continue;
        };
        let Ok(text) = serde_json::to_string(&payload) else {
            continue;
        };
        if writeln!(stdout, "{text}").is_err() || stdout.flush().is_err() {
            break;
        }
    }
}

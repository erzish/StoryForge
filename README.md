# StoryForge (Rust)

Desktop writing environment for long-form fiction: plot graph, chapter editor with
anchored notes, a temporal world-state database, and an MCP server so a model can read and
write the book directly.

This is the Rust rewrite of the Node.js edition in `../StoryForge`. The interface is the
same, the database is the same SQLite schema, and the markdown import/export layout is
unchanged — an existing `story.db` opens as-is.

## Build

Requirements: Rust stable (edition 2024), MSVC build tools, WebView2 (present on Windows
10/11). No Node.js is needed at build or run time.

```powershell
cargo build --release              # storyforge.exe
cargo tauri build                  # window + NSIS installer
pwsh scripts/package.ps1           # installer/out/*.exe + portable zip
```

## Run

```powershell
cargo run -p storyforge            # desktop window
cargo run -p storyforge -- --serve # backend only, prints http://127.0.0.1:<port>/
```

The window loads the interface from the embedded server on `127.0.0.1`, so the same
process serves the UI, `/api/...`, and the MCP endpoint at `/mcp`.

## Database location

1. `STORYFORGE_DB` environment variable (absolute path to a `story.db`);
2. `db_path` in `%APPDATA%\StoryForge\config.json`;
3. `%APPDATA%\StoryForge\data\story.db`, created with the bundled schema on first launch.

`%APPDATA%\StoryForge\ai-config.json` holds the DeepSeek / OpenRouter provider settings;
API keys never leave that file and are never returned by the API.

## Development layout

| Path | Contents |
|---|---|
| `crates/core` | domain, SQLite storage, markdown import/export, anchoring, AI clients, MCP tool surface |
| `crates/mcp` | stdio MCP transport |
| `src-tauri` | Tauri window, HTTP API, bundled UI (`src-tauri/ui`) |
| `docs/analysis` | complete inventory of the JavaScript edition (routes, schema, UI, subsystems) |
| `scripts/difftest.ps1` | differential test: Rust vs Node on identical databases |
| `PLAN.md` | port plan, decisions, milestones, known defects carried over |

## Verification

The JavaScript edition remains the specification. `scripts/difftest.ps1` starts both
backends against the same data and compares every probed endpoint as parsed JSON; route
parity is the acceptance criterion, not "it looks right in the window".

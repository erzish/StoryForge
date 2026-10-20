# StoryForge

**A writing environment for books.** A plot graph instead of a flat chapter list, an editor
with anchored notes, a "world" database that answers questions about any point in the story,
and an MCP server so a model can read and write the book directly.

Rewritten from Node.js to Rust: one window, one process, embedded SQLite. Node.js is not
required to run it — or to build it.

![Plot graph](docs/screenshot-graph.png)

![Chapter editor](docs/screenshot-editor.png)

---

## Install

Download `StoryForge_0.1.0_x64-setup.exe` from [Releases](../../releases) and run it.

- Per-user install; no administrator rights needed.
- Windows 10/11; WebView2 is already part of the system.
- The installer is unsigned, so SmartScreen will say "Unknown publisher" —
  **More info** → **Run anyway**.
- The "extract only" option (portable) drops `StoryForge.exe` in a folder without installing.

## First run

The app opens the database at `%APPDATA%\StoryForge\data\story.db`, created empty on first
launch. With no books yet, press **Import** on the empty screen and it will parse the
markdown bible from sibling folders (any folder containing `book.md`).

To open an **existing** database (for example one left by the Node edition):

```jsonc
// %APPDATA%\StoryForge\config.json
{ "db_path": "D:\\Books\\StoryForge\\data\\story.db" }
```

or set the `STORYFORGE_DB` environment variable to the path of `story.db`.

Import and export operate on the book folder: `book.md`, `chapters/chNNN.md`, `bible/`,
`plot/`, `timeline/`, `state/`. Re-importing does not destroy what markdown never held —
notes, facts, graph layout and version history are preserved.

## Screens

| Screen | What it does |
|---|---|
| **Graph** | canvas of chapter, event and free-plan nodes. Pan, zoom, marquee select, typed edges, group actions |
| **Text** | chapter editor: formatting, character counter, versions, revision-checked saving |
| **Notes** | cards beside the prose; the anchored fragment is highlighted in the text and survives edits above it |
| **State** | the world at a chosen moment: who is where, what each character knows, what they carry, which guns are still hanging |
| **Characters** | dossiers: personality, voice, evolution per chapter, relationships, appearances |
| **Activity** | what the model wrote into the book through MCP |
| **AI** | chat about the book and the chapter, scene context, fact extraction, plan and draft |
| **Help** | promises, secrets, timeline, world rules, glossary, events |

### Graph

One finger selects or drags; dragging empty space draws a selection box; two fingers scroll;
pinch zooms; right click opens the menu. `Ctrl+A` / `C` / `V` / `D`, `Delete`, `Esc`,
`Ctrl +/−/0`. Drag a node's right-hand dot onto another node to connect them.

### Text and notes

`Ctrl+B`, `Ctrl+I`, `Ctrl+Z`, `Ctrl+S`; `Ctrl+Alt+M` turns the selection into a note.
An empty note deletes itself. The file on disk stays plain markdown: saving without edits
changes not a single character.

Working with a model: **Build for AI** copies a prompt containing the notes and their
paragraphs; **Apply edits** accepts a JSON list of replacements and validates each one — an
ambiguous or missing fragment is rejected with a reason and the text there is left untouched.

## AI

**API settings** → DeepSeek or OpenRouter, model and key → **Test connection**. Keys live in
`%APPDATA%\StoryForge\ai-config.json` and are never returned to the interface or written to logs.

Two modes in the AI panel: **Chat · answer only** (discussion; the book is not modified) and
**Agent · edits the book** (tool calls: read state, write a chapter, record facts). The
transcript is stored locally per book and per chapter.

## MCP

While the app is running, MCP is available at `http://127.0.0.1:5123/mcp`.

```bash
claude mcp add --transport http storyforge http://127.0.0.1:5123/mcp
```

There is also a stdio transport (`storyforge-mcp.exe` reads JSON-RPC on stdin). Both speak the
same surface: 22 tools (state, facts, chapters, characters, events, relationships), 9 book
resources (`storyforge://<slug>/<key>`) and the `write_chapter` prompt.

## Build from source

Rust stable and MSVC build tools are enough; Node.js is not required.

```powershell
cargo build --release                 # storyforge.exe
cargo tauri build                     # window + NSIS installer
pwsh scripts/package.ps1              # installer/out: setup.exe + portable zip
cargo run -p storyforge               # window
cargo run -p storyforge -- --serve    # backend only, prints the URL
cargo test -p storyforge-core         # 46 tests
```

## Layout

```
crates/core      SQLite (schema copied verbatim), markdown import/export, note anchoring,
                 temporal world slices, DeepSeek/OpenRouter clients, the MCP tool surface
crates/mcp       stdio MCP transport over the same dispatch
src-tauri        window, HTTP API, embedded frontend, NSIS packaging
docs/analysis    full inventory of the JavaScript edition the port followed
scripts          differential tests against the Node backend
```

## What is verified — and what is not

Verified: 29 of 29 read endpoints answer byte-identically to the Node edition; writes
(chapter, versions, notes, edits, facts) show no differences; importing a real book matched
the counters; export was exercised on a copy of the book folder; MCP lists 22 tools;
46 tests pass.

Not verified: live AI provider responses (no key in the build environment), and the
**State**, **Characters**, **Activity** and **Help** screens have not been walked through by
hand. Known deviation: when the model refuses, the agent answers "the agent did not perform
the action" instead of the original four-attempt message.

## License

See [LICENSE](LICENSE).

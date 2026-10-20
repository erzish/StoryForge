# StoryForge → Rust: port plan

Rewrite of `../StoryForge` (Node 22 + `node:sqlite` + hand-written web UI) as a single
Rust desktop application. No Node runtime ships with the result.

## Decisions

| Question | Decision |
|---|---|
| Shell / window | Tauri v2, own WebView2 window, one `storyforge.exe` |
| UI | the existing `web/` assets, served by the embedded Rust HTTP server (same relative `/api` URLs), so behaviour and CSS survive |
| Process model | one process: Tauri window + `axum` server on `127.0.0.1:<port>` (default 5123) + SQLite |
| Database | the existing `story.db`, schema copied verbatim (`crates/core/assets/schema.sql`), `rusqlite` bundled SQLite |
| Installer | NSIS `setup.exe` (per-user) + portable zip, built by `cargo tauri build` |
| MCP | same tool surface over both transports: HTTP `POST /mcp` (embedded) and stdio (`storyforge-mcp.exe`) |
| Concurrency | one SQLite connection behind a mutex; synchronous core, async only at the HTTP edge |

## Workspace

```
crates/core        domain + storage + import/export + AI + MCP tool surface (sync)
crates/mcp         stdio MCP transport (thin wrapper over core)
src-tauri          window, HTTP API, bundled UI assets, packaging
  src/api/*.rs     one module per route family, mirroring server/index.js
  ui/              the ported web frontend
docs/analysis      full inventory of the JS edition (routes, schema, UI, subsystems)
scripts/           differential test harness (Rust vs Node on the same DB)
```

## Fidelity strategy

The JavaScript edition is the specification. Two independent sources are used for every
route: `docs/analysis/server-api.md` (route table, schema, 68 hazards) and the original
source in `../StoryForge/server`. Correctness is then checked by
`scripts/difftest.ps1`, which runs the Node server and the Rust server against identical
database copies and compares every probed endpoint as parsed JSON.

## Milestones

1. **Skeleton** — workspace, Tauri window, embedded server, database opens the real
   `story.db`, `/api/health`, `/api/about`. *(done)*
2. **Read surface** — books, chapters, characters, events, timeline, graph, state, facts,
   knowledge, dictionaries, moments, diff, search, annotations, versions. *(in progress)*
3. **Write surface** — chapter save + versions, annotations, CRUD, facts/knowledge writes,
   graph mutations, apply-edits.
4. **Import / export** — markdown bible import with preservation, generated-file export.
5. **AI** — settings + keys, chat, agent with tools, extract/plan/draft/audit/suggest.
6. **MCP** — HTTP + stdio, 22 tools, 9 resources, `write_chapter` prompt.
7. **Packaging** — icons, NSIS installer, portable zip, first-run database picker.
8. **Parity proof** — difftest green on the whole probed route set, plus the JS test
   suite's assertions ported to Rust tests.

## Known JS defects

These are reproduced deliberately until the user decides otherwise; each is noted where
it is ported.

* `preserve.js` captures `relationship_state` but never restores it — hand-authored
  relationship states are lost on every re-import.
* Imported facts whose kind is not `событие` disappear on an export→import round trip.
* `DELETE /api/nodes/:id?withEntity=1` interpolates the user-writable `node.ref_table`
  column as a SQL identifier (the port whitelists table names instead).
* `GET /api/books/:id/graph` mutates the database by seeding nodes on first read.

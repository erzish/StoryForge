# Story Forge — Server-Side Engineering Inventory

Reference for a Rust reimplementation. Describes **only what exists today** in
`<js-edition>\server`. No target design is proposed.

Everything below was read from source. Exact identifiers, route strings, column names,
SQL fragments and Russian user-facing error strings are quoted verbatim, because the
web client (`web/*.js`, out of scope here) depends on them.

---

## 0. Runtime, process model, file inventory

| Fact | Value |
|---|---|
| Language / module system | ES modules (`"type": "module"` in root `package.json`), Node.js **>= 22.5** (hard-checked twice: `db/index.js:27`, `start.js:19`) |
| HTTP server | `node:http` `createServer`, no framework, **zero npm dependencies** |
| Database | `node:sqlite` **`DatabaseSync`** (synchronous, built into Node), file `data/story.db` |
| Bind address | `127.0.0.1` only (`lib/http.js:121`) |
| Port | `process.env.SF_PORT` or **5123** (`index.js:24`, `start.js:11`) |
| DB path | `process.env.SF_DB` or `<APP_ROOT>/data/story.db` (`db/index.js:13`) |
| AI config path | `process.env.SF_AI_CONFIG` or `<APP_ROOT>/data/ai-config.json` (`lib/ai.js:9`) |
| `APP_ROOT` | `server/db/../../` = repository root (`db/index.js:10`) |
| Static root | `APP_ROOT/web` (`index.js:26`), serves `index.html`, `app.js`, `style.css`, … |
| Auth | **none**. No cookies, no sessions, no CORS headers, no CSRF, loopback bind is the only control |
| Logging | `console.log`/`console.error` to stdout/stderr only; no log levels |

npm scripts (`package.json`), all with `--disable-warning=ExperimentalWarning`:

```
start     node server/start.js
serve     node server/index.js
import    node server/import/cli.js
export    node server/tools/export.js
report    node server/import/report.js
evaluate  node server/tools/evaluate.js
scaletest node server/tools/scaletest.js
mcp       node server/mcp.js
test      node --test server/test/*.test.js
```

### 0.1 The 36 files

| File | Bytes | Role |
|---|---:|---|
| `server/index.js` | 49674 | HTTP entry point: all 85 routes, plus `/mcp` transport |
| `server/start.js` | 2778 | launcher: Node version check, first-run import, spawn browser |
| `server/mcp.js` | 2547 | MCP **stdio** transport (JSON-RPC per line) |
| `server/db/index.js` | 3500 | `getDb()` singleton, schema application, column migration |
| `server/db/schema.sql` | 36455 | full DDL, seeds, indexes, triggers, FTS5 table |
| `server/lib/http.js` | 5159 | router, body parsing, static files, `fail()` / `raw()` |
| `server/lib/crud.js` | 2954 | whitelisted `patch` / `insert` / `remove`, `EDITABLE` field lists |
| `server/lib/md.js` | 5054 | markdown bible parser (sections, tables, bullets, metrics) |
| `server/lib/anchor.js` | 8317 | note→text anchoring, edit planning/applying |
| `server/lib/chapterText.js` | 4201 | chapter file I/O, sha256 revisions, versions, FTS sync |
| `server/lib/state.js` | 15335 | time-slice queries: `resolveMoment`, `worldState`, `diff`, … |
| `server/lib/facts.js` | 8741 | slotted fact/knowledge writes, kind tree expansion |
| `server/lib/scenePacket.js` | 3741 | per-chapter "scene packet" context bundle |
| `server/lib/aiContext.js` | 7232 | flat text prompt-context assembly |
| `server/lib/ai.js` | 7162 | provider config, HTTP calls to DeepSeek/OpenRouter |
| `server/lib/agent.js` | 18196 | built-in tool-calling agent, Russian intent heuristics |
| `server/lib/mcpCore.js` | 32538 | 22 MCP tools, 9 resources, 1 prompt, JSON-RPC `dispatch` |
| `server/lib/entityTools.js` | 11664 | generic CRUD tools over 17 entity types |
| `server/lib/proposals.js` | 6805 | extract→validate→commit pipeline for AI-extracted facts |
| `server/lib/graph.js` | 3942 | canvas graph auto-seed and load |
| `server/lib/search.js` | 1035 | FTS5 chapter search + offscreen-beat LIKE search |
| `server/lib/activity.js` | 1165 | `mcp_activity` log write/read |
| `server/lib/chatHistory.js` | 1678 | reconstruct chat transcript from `ai_run` |
| `server/import/importBook.js` | 26123 | 18-stage destructive markdown→DB rebuild |
| `server/import/preserve.js` | 8379 | capture/restore user data around re-import |
| `server/import/timeline.js` | 9120 | moment axis construction, legacy→fact migration |
| `server/import/importAll.js` | 1344 | find book folders (`book.md`) and import each |
| `server/import/cli.js` | 1471 | `npm run import` |
| `server/import/report.js` | 6233 | `npm run report` integrity report |
| `server/tools/export.js` | 7928 | DB→markdown snapshot writer |
| `server/tools/evaluate.js` | 2571 | eval harness over `buildAiContext` |
| `server/tools/scaletest.js` | 7297 | 1000-chapter load/scale test |
| `server/test/reliability.test.js` | 3313 | revision gate, overlapping edits, import rollback |
| `server/test/memory.test.js` | 4048 | scene-packet separation, proposal validation |
| `server/test/ai.test.js` | 12300 | chat history, provider config, context, key non-leak |
| `server/test/agent.test.js` | 4347 | agent writes chapter, book isolation, no-write on question |

---

## 1. HTTP layer contract (`server/lib/http.js`)

### 1.1 Routing

```js
createApp({ staticDir }) -> {
  get(p,h), post(p,h), put(p,h), patch(p,h), delete(p,h),
  listen(port) -> Promise, server
}
```

* A route pattern is split on `/`, empty segments dropped, so `'/api/books/:id'` becomes
  `['api','books',':id']`.
* Matching requires **equal segment count** and per-segment equality; a `:name` segment
  matches anything and captures `decodeURIComponent(segment)`.
* Routes are tested **in registration order**; the **first** match for the request method
  wins. Methods are compared exactly (`req.method`), so `HEAD` matches nothing.
* `:` params are percent-decoded; a malformed escape throws `URIError` **outside** the
  per-route `try` block (`index.js`/`http.js:80`), i.e. an invalid `%` sequence in a path
  segment rejects the async request handler → unhandled rejection → process termination.
* Query string: `Object.fromEntries(url.searchParams)` — every value is a **string**, and a
  repeated parameter keeps the **last** value.

`listen()` resolves when bound: `server.listen(port, '127.0.0.1', ok)`.

### 1.2 Request body

* Only for non-`GET` methods (`http.js:84`). Empty body → `undefined`.
* Body must be JSON; accumulated fully in memory, **no size limit**. Parse failure throws
  `Error('Тело запроса — не JSON')` with `status: 400`.
* `Content-Type` is never inspected.

### 1.3 Response rules

| Handler returns | Response |
|---|---|
| `{__raw:true, status, body, headers}` | exactly those; body `null`/`undefined` → empty body (`res.end()`) |
| `{__status:n, data:{error}}` (from `fail()`) | `n`, JSON `{"error": "..."}` |
| any other value | `200`, JSON of value; `undefined` serializes to `null` |
| thrown `Error` | `e.status ?? 500`, JSON `{"error": e.message}`; `>=500` also `console.error(e)` |
| no route matched, GET | attempt static file; on success `200` with MIME + `Cache-Control: no-cache` |
| otherwise | `404`, JSON `{"error":"Не найдено"}` |

```js
fail(status, message) -> ({ __status: status, data: { error: message } })
raw(status, body, headers = {}) -> ({ __raw: true, status, body, headers })
```

Note the asymmetry: `fail()` produces `{error: message}` **inside** `data`; a thrown error
produces the same shape. `raw()` bypasses `JSON.stringify`, which is why `/mcp` can emit
`202` with a truly empty body.

Status codes actually emitted by the server: **200, 202, 400, 403, 404, 405, 409, 500,
502**. (`201`, `204`, `304`, `401` never appear.)

### 1.4 Static files

* Only for `GET`, only after all routes fail.
* `/` → `index.html`; otherwise `normalize(pathname)` with leading `[/\.]+` stripped.
* Traversal guard: `full.startsWith(staticDir)` after `join`.
* MIME map: `.html`, `.js`, `.css`, `.json`, `.svg`, `.png`, `.jpg`, `.woff2`, `.ico`;
  anything else → `application/octet-stream`.
* Directory paths are not indexed (`stat().isFile()` check) → 404.

---

## 2. Full HTTP route table (85 routes, registration order)

`index.js` registers handlers in the order below. All paths are absolute. Body shapes use
`field: type` where `?` marks optional. "Row" means the raw SQLite row(s), snake_case,
including `id`.

### 2.1 AI settings and chat (R01–R10)

| # | M | Path | Purpose | Request body | Response | Status |
|---|---|---|---|---|---|---|
| R01 | GET | `/api/ai/settings` | AI provider config, secrets redacted | — | `{provider, model, has_key, profiles:{deepseek:{model,has_key}, openrouter:{model,has_key}}}` | 200 |
| R02 | PUT | `/api/ai/settings` | Save provider/model/key | `{provider:'deepseek'\|'openrouter', model?, api_key?, clear_key?}` | same as R01 | 200, 400 |
| R03 | POST | `/api/ai/test` | Ping the model | — | `{ok:true, model, answer}` (`answer` = first 200 chars) | 200, 502 |
| R04 | GET | `/api/books/:id/ai/history` | Chat transcript from `ai_run` | — (query `chapter_id?`) | `{messages:[{role, content, actions?, retry?, run_id}], has_more}` | 200, 400, 404 |
| R05 | POST | `/api/books/:id/ai/chat` | One-shot chat turn (persists nothing to the book) | `{message:1..12000, chapter_id?, history?:[{role:'user'\|'assistant', content}]}` | `{text, tool_calls, model, finish_reason, tokens_in, tokens_out}` | 200, 400, 404(→500 path), 502 |
| R06 | POST | `/api/books/:id/ai/agent` | Tool-calling agent turn | `{message:1..12000, chapter_id?, history?}` | `{text, actions[], steps[], model, finish_reason}` **or on failure `{error, actions, steps}`** | 200 (even on failure), 400, 404 |
| R07 | POST | `/api/chapters/:id/ai/extract` | Ask the model for facts/knowledge/reader-knowledge proposals, validate them | none (chapter from path) | `{accepted[], rejected[], revision, model}` | 200, 400, 404, 502 |
| R08 | POST | `/api/chapters/:id/ai/commit-facts` | Persist accepted proposals | `{expected_revision, proposals[]}` | `{inserted:number}` | 200, 400, 404, 409 |
| R09 | POST | `/api/chapters/:id/ai/task` | `plan` / `draft` / `audit` / `style_review` | `{kind, input?, scene_type?}` | `{text, tool_calls, model, finish_reason, tokens_in, tokens_out, revision}` | 200, 400, 404, 502 |
| R10 | POST | `/api/chapters/:id/ai/suggest-edits` | Propose exact replacements for open annotations (dry run only) | none | `{revision, preview, applied:[{annotation_id, old, new}], rejected[]}` | 200, 400, 404, 502 |

Notes:

* **R02** errors are plain `Error` thrown by `saveAiConfig` and caught locally →
  `400` with e.g. `'Выберите DeepSeek или OpenRouter'`, `'Некорректное имя модели'`,
  `'Некорректный API-ключ'`.
* **R04** validation: `bookId` must be a safe integer, `chapter_id` too (`''` means null).
  `loadChatHistory` throws 404 `'Книга не найдена'` and
  `'Глава не принадлежит книге'`.
* **R05** validation: `message` trimmed, 1..12000 → else `400`
  `'Сообщение должно содержать от 1 до 12000 символов'`; `chapter_id` must be a safe
  integer → `400 'Некорректная глава'`; `history` is truncated to the **last 8** entries,
  each `content` truncated to its **last 6000** chars; invalid entries → `400`
  `'Некорректная история диалога'`. Errors from `buildAiContext` surface as
  `fail(e.status || 500)`.
* **R06** returns **200 with an `error` field** when the agent fails (no `fail()`), unlike
  every other AI route. `actions_json` is persisted even on failure.
* **R07** uses only `chapter.body.slice(0, 60000)` (no options), inserts an `ai_run` row with
  `kind='extract_facts'`, `prompt='Извлечение фактов из главы N'`. The spread
  `{...checked, chapter: undefined, model}` drops `chapter` during JSON serialization.
* **R09** `kind` must be one of `plan`, `draft`, `audit`, `style_review` → else
  `400 'Неизвестная задача ИИ'`; `input` clipped to 60000 chars; `scene_type` trimmed to 60;
  `audit`/`style_review` require non-empty `input` → `400 'Нет черновика для проверки'`.
  maxTokens 8000/timeout 180 s for `draft`, else 3000/120 s.
* **R10** requires at least one `annotation` with `status='открыта'` → else
  `400 'Нет открытых заметок'`. Model JSON is parsed after stripping a leading/trailing
  code fence `^```(?:json)?\s*|\s*```$` (global regex, so only the outermost fence);
  `502` for `'Модель вернула некорректный JSON правок'` / `'В ответе нет списка правок'`.

### 2.2 Books, search, style examples (R11–R17)

| # | M | Path | Purpose | Request body | Response | Status |
|---|---|---|---|---|---|---|
| R11 | GET | `/api/books` | All books | — | `book[]` ordered by `title` | 200 |
| R12 | GET | `/api/books/:id` | Book + counters | — | `book` + `{counts:{characters, chapters, events, relationships, secrets, promises, locations, timeline}, written:{n, chars}}` | 200, 404 |
| R13 | GET | `/api/books/:id/scenes/:chapter/context` | Scene packet for a chapter id | — | `buildScenePacket` result (see §6.6) | 200, 404 |
| R14 | GET | `/api/books/:id/text-search` | Full-text search over chapter prose | — (query `q`, `limit`) | `{chapters:[{chapter_id, number, title, snippet}], offscreen:[{id, chapter_no, line, offscreen, visible_effect, planned}]}` | 200, 400 |
| R15 | GET | `/api/books/:id/style-examples` | Approved style samples | — | `style_example[]` ordered by `id DESC` | 200 |
| R16 | POST | `/api/books/:id/style-examples` | Add a sample | `{scene_type:1..60, body:40..6000, title?≤120, source_chapter_id?}` | created `style_example` row | 200, 400, 404 |
| R17 | DELETE | `/api/style-examples/:id` | Delete a sample | — | `{changed:0\|1}` | 200 |

Notes: **R12** `written` = `{n, chars}` where `chars` is `SUM(chars_no_space)` over
`status='написано'`. **R13**'s `:chapter` is a **chapter id**, not a chapter number.
**R14** delegates bounds to `searchBookText` (2..150 chars, limit clamped 1..100).

### 2.3 Characters, relationships (R18–R20)

| # | M | Path | Purpose | Request body | Response | Status |
|---|---|---|---|---|---|---|
| R18 | GET | `/api/books/:id/characters` | Character list + counters | — | `character.*` + `state_count`, `scene_count`, ordered `is_main DESC, name` | 200 |
| R19 | GET | `/api/characters/:id` | Dossier | — | `character` + `states[]`, `relationships[]`, `incoming[]`, `events[]`, `secrets[]` | 200, 404 |
| R20 | GET | `/api/books/:id/relationships` | Flat edge list | — | `{id, kind, current_state, history, from_id, from_name, to_id, to_name}[]` ordered by `from_name` | 200 |

Notes: **R19** `states` ordered `COALESCE(chapter_no,-1), ord`; `relationships` are outgoing
only (`r.from_char=?`) with `COALESCE(t.name, r.to_label) AS to_name`; `incoming` joins
`to_char` only (so label-only targets never appear); `events` ordered by `ch.number`
(NULL chapter numbers sort first); `secrets` from `secret_knowledge`.

### 2.4 Chapters, text, versions (R21–R26)

| # | M | Path | Purpose | Request body | Response | Status |
|---|---|---|---|---|---|---|
| R21 | GET | `/api/books/:id/chapters` | Chapter list with arc + counts | — | `chapter.*` + `arc_name`, `arc_color`, `event_count`, `open_notes`, ordered by `number` | 200 |
| R22 | GET | `/api/chapters/:id/text` | Chapter metadata + prose | — | `chapter.*` + `{body, revision, missing?}` | 200, 404 |
| R23 | PUT | `/api/chapters/:id/text` | Save prose | `{body, expected_revision?, reason?}` | `{ok:true, revision, chars_no_space, words}` or `{ok:true, unchanged:true, revision, chars_no_space, words}` | 200, 400, 404, 409 |
| R24 | GET | `/api/chapters/:id/versions` | Version list | — | `{id, reason, created_at, chars}[]` ordered `id DESC` LIMIT 100 | 200 |
| R25 | GET | `/api/chapters/:id/versions/:version` | One version, full body | — | `chapter_version` row or `404 'Версия не найдена'` | 200, 404 |
| R26 | POST | `/api/chapters/:id/versions/:version/restore` | Restore a version | `{expected_revision?}` | as R23, `reason='восстановление версии'` | 200, 400, 404, 409 |

Notes: **R22** `missing` is present only when `file_path` is set but the file is absent.
**R23** `reason` defaults to `'ручная правка'`; `body` must be a string → `400 'Нет текста'`.
`expected_revision` is optional here; when provided and stale → `409`. **R26** reads the
version body first (404 if missing) and then saves with the version's text.

### 2.5 Events, timeline, promises, secrets, world (R27–R32)

| # | M | Path | Purpose | Request body | Response | Status |
|---|---|---|---|---|---|---|
| R27 | GET | `/api/books/:id/events` | Event list | — | `event.*` + `chapter_no`, `participants` (comma-joined names) ordered `ch.number, e.id` | 200 |
| R28 | GET | `/api/books/:id/timeline` | Timeline entries | — | `timeline_entry[]` ordered by `ord` | 200 |
| R29 | GET | `/api/books/:id/promises` | "Chekhov guns" | — | `promise[]` ordered by `id` | 200 |
| R30 | GET | `/api/books/:id/secrets` | Secrets + who knows | — | `secret.*` + `knowledge:[{who, stance, character_id}]` | 200 |
| R31 | GET | `/api/books/:id/world` | World bible | — | `{rules, locations, glossary, hidden}` | 200 |
| R32 | POST | `/api/import` | Import one book folder | `{path}` | `{bookId, title, stats, warnings}` | 200, 400, 500 |

Notes: **R31** `glossary` ordered `section, term`; `hidden` = `hidden_line[]` (no beats/hints
included). **R32** `path` is `resolve()`d; missing `path` → `400 'Нужен путь к папке книги'`.
The route has **no try/catch**, so an import failure becomes `500 {error}` (and the import's
own `ROLLBACK` has already run).

### 2.6 Import-all and MCP transport (R33–R37)

| # | M | Path | Purpose | Request body | Response | Status |
|---|---|---|---|---|---|---|
| R33 | POST | `/api/import-all` | Import every sibling book folder | — | `{books:[{title, stats, warnings}]}` | 200, 404 |
| R34 | POST | `/mcp` | MCP Streamable HTTP, JSON-RPC 2.0 | any JSON-RPC message | JSON-RPC response, or empty body | 200, 202, 403 |
| R35 | GET | `/mcp` | Not supported | — | `{error:'GET не поддерживается — сервер не отдаёт SSE-поток'}` | 405 |
| R36 | DELETE | `/mcp` | Not supported | — | `{error:'DELETE не поддерживается — управление сессиями не реализовано'}` | 405 |
| R37 | GET | `/api/books/:id/activity` | AI action feed | — (query `limit`, default 50) | `mcp_activity[]` ordered `id DESC` with `detail` JSON-parsed | 200 |

Notes for **R34**: scans `APP_ROOT/..` for directories containing `book.md`; empty result →
`404 'Книги не найдены — папка с book.md рядом со StoryForge не обнаружена'`.

**MCP transport semantics:**

* `Origin` header, when present, must parse as a URL whose `hostname` is exactly
  `localhost` or `127.0.0.1`; otherwise `403 {"error":"Недопустимый Origin"}` (raw JSON).
  Absent `Origin` is allowed.
* Body absent (`undefined`) → `202` with **no body** (notification/response from client).
* `dispatch()` returns `{hasReply:false}` → `202`, no body.
* `{hasReply:true}` → `200` with `JSON.stringify(payload)` and
  `Content-Type: application/json`.
* No SSE, no `Mcp-Session-Id`, no GET stream, no DELETE session teardown.

### 2.7 Moments and time slices (R38–R43)

| # | M | Path | Purpose | Request body | Response | Status |
|---|---|---|---|---|---|---|
| R38 | GET | `/api/books/:id/moments` | Time axis with coverage | — | `moment.*` + `entries` count + `chapters` (comma-joined chapter numbers whose `[from_moment.seq, to_moment.seq]` bracket this `seq`), ordered `seq` | 200 |
| R39 | GET | `/api/books/:id/state` | World slice at a moment | — (query `chapter`, `moment`, `characters`, `kinds`, `events`, `minWeight`, `full`) | `worldState` result (§6.4) | 200 |
| R40 | GET | `/api/characters/:id/state` | One character's slice | — (query `chapter`, `moment`, `full`, `events`) | `characterState` result | 200, 404 |
| R41 | GET | `/api/books/:id/facts/search` | Fact search | — (query `q`, `chapter`, `moment`, `characters`, `kinds`, `limit`) | `fact.*` + `subject`, `from_label` | 200 |
| R42 | GET | `/api/dictionaries` | Dictionaries | — | `{tree, fact_kinds, stances}` | 200 |
| R43 | GET | `/api/books/:id/diff` | Delta between two moments | — (query `from_moment`/`from`, `to_moment`/`to`) | `{from, to, appeared[], ended[], learned[]}` | 200 |

Query parsing helpers (`index.js:507`):

```js
const idList  = (s) => String(s ?? '').split(',').map(Number).filter(Boolean);
const strList = (s) => String(s ?? '').split(',').map(x => x.trim()).filter(Boolean);
```

`idList` **drops `0` and `NaN`** (`filter(Boolean)`). `strList` drops empty strings but keeps
any Unicode text.

Notes: **R39** `full === '1'` only (the string `'1'`); `events`/`minWeight` are `Number()`ed
when present (`?events=0` is meaningful: no events). **R41** computes `at` only when
`chapter` or `moment` is present, i.e. no time filter otherwise.

### 2.8 Facts (R44–R48)

| # | M | Path | Purpose | Request body | Response | Status |
|---|---|---|---|---|---|---|
| R44 | GET | `/api/books/:id/facts` | List facts | — (query `subject_type`, `subject_id`, `kind`) | `fact.*` + `from_label`, `from_seq` (from joined `moment`), ordered `COALESCE(m.seq,0), f.id` | 200 |
| R45 | POST | `/api/books/:id/facts` | Add a fact (slot-closing) | arbitrary fact fields, see §6.3 | `fact` row + `closed:[{id,value}]` | 200 |
| R46 | PATCH | `/api/facts/:id` | Patch whitelisted fields | subset of `EDITABLE.fact` | `fact` row or `null` | 200 |
| R47 | DELETE | `/api/facts/:id` | Hard delete | — | `{changed:0\|1}` | 200 |
| R48 | POST | `/api/facts/:id/close` | Close a fact from a chapter/moment | `{chapter?, moment?}` | `fact` row | 200, 404 |

Notes: **R44** selects `m.seq AS from_seq` **after** `f.*`, so the JSON `from_seq` is the
joined moment's `seq` (`null` when the moment is missing), shadowing the denormalized
column. **R45** never validates `subject_*` or `kind`; an unknown `kind` is silently coerced
to `'событие'`. **R48** requires the fact to exist; `to_moment` is `body.moment ?? momentOfChapter(book_id, chapter)`
and `to_chapter` is `body.chapter ?? null`.

### 2.9 Knowledge (R49–R52)

| # | M | Path | Purpose | Request body | Response | Status |
|---|---|---|---|---|---|---|
| R49 | GET | `/api/books/:id/knowledge` | List knowledge | — | `knowledge.*` + `who`, `secret_title`, `from_label`, `from_seq` (joined), ordered `COALESCE(m.seq,0), c.name` | 200 |
| R50 | POST | `/api/books/:id/knowledge` | Add knowledge (slot-closing) | see §6.3 | `knowledge` row + `closed:[{id,stance}]` | 200 |
| R51 | PATCH | `/api/knowledge/:id` | Patch whitelisted fields | subset of `EDITABLE.knowledge` | row or `null` | 200 |
| R52 | DELETE | `/api/knowledge/:id` | Hard delete | — | `{changed:0\|1}` | 200 |

### 2.10 Promises (R53–R55)

| # | M | Path | Purpose | Request body | Response | Status |
|---|---|---|---|---|---|---|
| R53 | POST | `/api/books/:id/promises` | Create | `{ext_no?, title?='', planted_ch?, expected_payoff?, status?='ожидает', fired_ch?}` | created row | 200 |
| R54 | PATCH | `/api/promises/:id` | Patch | `title, planted_ch, expected_payoff, status, fired_ch` | row or `null` | 200 |
| R55 | DELETE | `/api/promises/:id` | Delete | — | `{changed}` | 200 |

### 2.11 Entity CRUD (R56–R66)

| # | M | Path | Purpose | Request body | Response | Status |
|---|---|---|---|---|---|---|
| R56 | PATCH | `/api/characters/:id` | Patch character | `EDITABLE.character` subset | row or `null` | 200 |
| R57 | POST | `/api/books/:id/characters` | Create character | `{name?='Новый персонаж', is_main?}` | row | 200 |
| R58 | DELETE | `/api/characters/:id` | Delete (cascades) | — | `{changed}` | 200 |
| R59 | PATCH | `/api/chapters/:id` | Patch chapter | `EDITABLE.chapter` subset | row or `null` | 200 |
| R60 | POST | `/api/books/:id/chapters` | Create chapter | `{number?, title?}` | row (number = `MAX(number)+1`, title = `Глава N`, status `план`) | 200 |
| R61 | DELETE | `/api/chapters/:id` | Delete chapter (file left on disk) | — | `{changed}` | 200 |
| R62 | PATCH | `/api/events/:id` | Patch event | `EDITABLE.event` subset | row or `null` | 200 |
| R63 | POST | `/api/books/:id/events` | Create event | `{chapter_id?, title?='Новое событие', summary?, status?='план', hidden?}` | row | 200 |
| R64 | DELETE | `/api/events/:id` | Delete event | — | `{changed}` | 200 |
| R65 | POST | `/api/events/:id/characters` | Upsert participant | `{character_id (required), state_snapshot?}` | `{ok:true}` | 200, 400 |
| R66 | DELETE | `/api/events/:id/characters/:cid` | Remove participant | — | `{ok:true}` | 200 |

Notes: **R57** inserts only `book_id`, `name`, `is_main`; duplicate name → UNIQUE violation →
`500`. **R60** duplicate `number` → `500`. **R65** uses
`INSERT OR REPLACE INTO event_character (event_id, character_id, state_snapshot)`;
`400 'Нужен character_id'` when falsy.

### 2.12 Graph canvas (R67–R73)

| # | M | Path | Purpose | Request body | Response | Status |
|---|---|---|---|---|---|---|
| R67 | GET | `/api/books/:id/graph` | Whole canvas (may auto-seed) | — | `{nodes[], edges[]}` | 200 |
| R68 | POST | `/api/books/:id/nodes` | Create node (and entity) | `{type?='comment', ref_table?, ref_id?, x?, y?, data?, chapter_id?}` | node from `loadGraph` (with `entity`) or raw row | 200 |
| R69 | PATCH | `/api/nodes/:id` | Move/resize/edit node | `EDITABLE.node` subset; `data` object is stringified | row or `null` | 200 |
| R70 | POST | `/api/nodes/move` | Bulk move | `{items:[{id, x, y}]}` | `{ok:true, moved:N}` | 200 |
| R71 | DELETE | `/api/nodes/:id` | Delete node, optionally entity | — (query `withEntity=1`) | `{changed}` | 200, 404 |
| R72 | POST | `/api/books/:id/edges` | Create edge (+ event_link mirror) | `{from_node, to_node, kind?='flow', label?}` | created `edge` row | 200, 400, 409 |
| R73 | DELETE | `/api/edges/:id` | Delete edge (+ mirror cleanup) | — | `{changed}` | 200 |

Notes:

* **R68** — when `type==='chapter' && !ref_id`, a `chapter` is created
  (`number = MAX+1`, `title = 'Глава N'`, `status='план'`); when
  `type==='event' && !ref_id`, an `event` is created (`title='Новое событие'`,
  `status='план'`, `chapter_id` from body). `data` is `JSON.stringify`d when truthy.
* **R71** — 404 `'Нода не найдена'`; `withEntity==='1'` and a `ref_table`/`ref_id` pair
  triggers `crud.remove(node.ref_table, node.ref_id)` — the table name is interpolated from
  stored data (see §9, hazard H7).
* **R72** — `400 'Нужны from_node и to_node'`; `400 'Нода не может ссылаться на себя'`;
  duplicate `(book_id, from_node, to_node)` → `409 'Такая связь уже есть'`; when both nodes
  reference events, `INSERT OR IGNORE INTO event_link (from_event, to_event)` (kind defaults
  to `'causal'`).
* **R73** — resolves both endpoint nodes; if both are event nodes, deletes the matching
  `event_link` row first.

### 2.13 Annotations and edit application (R74–R79)

| # | M | Path | Purpose | Request body | Response | Status |
|---|---|---|---|---|---|---|
| R74 | GET | `/api/chapters/:id/annotations` | Notes for a chapter | — | `annotation[]` ordered `start_pos` | 200 |
| R75 | POST | `/api/chapters/:id/annotations` | Create note (empty allowed) | `{quote?='', prefix?, suffix?, start_pos?, end_pos?, note?=''}` | created row | 200 |
| R76 | PATCH | `/api/annotations/:id` | Patch note / change status | `EDITABLE.annotation` subset | row or `null` | 200 |
| R77 | DELETE | `/api/annotations/:id` | Delete note | — | `{changed}` | 200 |
| R78 | GET | `/api/chapters/:id/ai-context` | Anchored notes + prose for the model | — | `{chapter:{id, number, title, pov, summary, chars_no_space, min_chars}, book:{title, style}, text, notes:[…]}` | 200, 404 |
| R79 | POST | `/api/chapters/:id/apply-edits` | Validate + apply exact replacements | `{edits:[{annotation_id?, old, new}], dry_run?, expected_revision?}` | applied: `{applied[], rejected[], chars_no_space, revision}`; dry run adds `dry_run:true` and `preview` | 200, 400, 404, 409 |

Notes:

* **R75** deliberately accepts empty `quote`/`note`: the UI creates a card on selection and
  the author types afterwards; empty notes are deleted client-side.
* **R76** sets `resolved_at=datetime('now')` whenever `status` is present and `!== 'открыта'`,
  then patches.
* **R78** reads the file directly (`readFileSync(join(root_path, file_path))`, `''` if
  absent) instead of going through `readChapterText`; each note is enriched by `locateNote`
  with `found`, `matched_by` (`how`), `unique`, `occurrences`, `paragraph_index`,
  `paragraph_count`, `paragraph`, `paragraph_before`, `paragraph_after`.
* **R79** — `400 'Нет правок'` when `edits` is not a non-empty array; `404 'Глава не найдена'`;
  `400 'У главы ещё нет текста'` when `file_path` is null; `400 'Файл главы не найден'`.
  Then:
  `if (!body?.dry_run && body?.expected_revision !== revision) → 409 'Глава изменилась после просмотра правок. Получите предпросмотр заново.'`
  — **note that omitting `expected_revision` on a non-dry-run call always yields 409.**
  `notesById` is built from **all** annotations of the chapter (any status). When nothing is
  planned the response is `{applied:[], rejected, chars_no_space: ch.chars_no_space}` and no
  write happens. Successful edits set
  `annotation.status='применена'`, `resolved_at=datetime('now')`, `ai_result=<new text>`.

### 2.14 Character states and relationships (R80–R85)

| # | M | Path | Purpose | Request body | Response | Status |
|---|---|---|---|---|---|---|
| R80 | POST | `/api/characters/:id/states` | Add evolution fact | `{chapter_no?, kind?='event', text?='', hidden?}` | created `character_state` row | 200 |
| R81 | PATCH | `/api/character-states/:id` | Patch | `chapter_no, kind, text, hidden, ord` | row or `null` | 200 |
| R82 | DELETE | `/api/character-states/:id` | Delete | — | `{changed}` | 200 |
| R83 | POST | `/api/books/:id/relationships` | Create relation | `{from_char, to_char?, to_label?, kind?, current_state?='', history?}` | row | 200 |
| R84 | PATCH | `/api/relationships/:id` | Patch | `kind, current_state, history, is_mutual, to_char, to_label` | row or `null` | 200 |
| R85 | DELETE | `/api/relationships/:id` | Delete | — | `{changed}` | 200 |

Notes: **R83** does `Number(body?.from_char)` and does not validate it (may be `NaN`);
`current_state` defaults to `''`; a duplicate `(book_id, from_char, to_char, to_label)`
raises a UNIQUE violation → `500`.

---

## 3. SQLite schema (`server/db/schema.sql`, 700 lines)

Applied on **every** process start by `db/index.js:40` (`_db.exec(schema)`), before
`migrate()` and before the FTS bootstrap. The file begins with:

```sql
PRAGMA journal_mode = WAL;
PRAGMA foreign_keys = ON;
```

`foreign_keys=ON` is per-connection and must be re-issued for any new connection.

36 tables, 15 explicit indexes, 8 triggers, 1 FTS5 virtual table, 2 seeded dictionaries.

### 3.1 Book and free-form docs

#### `book`
> One row per imported book; `slug` is the folder name and the identity key for re-import.

```sql
id            INTEGER PRIMARY KEY
slug          TEXT UNIQUE NOT NULL          -- имя папки
title         TEXT NOT NULL
root_path     TEXT NOT NULL                 -- absolute path to the book folder
genre         TEXT
pov           TEXT
tone          TEXT
language      TEXT DEFAULT 'ru'
active_style  TEXT                          -- styles/<name>.md stem
min_chars     INTEGER DEFAULT 10000         -- platform threshold, chars w/o spaces
target_chars  INTEGER DEFAULT 12000
premise       TEXT
created_at    TEXT DEFAULT (datetime('now'))
updated_at    TEXT DEFAULT (datetime('now'))
```

#### `doc`
> Every markdown file that did not map to a structured table; guarantees import loses nothing.

```sql
id         INTEGER PRIMARY KEY
book_id    INTEGER NOT NULL REFERENCES book(id) ON DELETE CASCADE
kind       TEXT NOT NULL          -- book | direction | arc_plan | style | factions | other
rel_path   TEXT NOT NULL
title      TEXT
body       TEXT NOT NULL
updated_at TEXT DEFAULT (datetime('now'))
UNIQUE(book_id, rel_path)
```

#### `moment`
> The single time axis; `seq` is a **REAL** so points can be inserted between existing ones.

```sql
id         INTEGER PRIMARY KEY
book_id    INTEGER NOT NULL REFERENCES book(id) ON DELETE CASCADE
seq        REAL NOT NULL       -- порядок на оси; дробный
label      TEXT NOT NULL       -- «Гринтаун, день 3, вечер»
world      TEXT                -- реальный мир | мир книги
story_date TEXT
note       TEXT
-- INDEX idx_moment_book ON moment(book_id, seq)
```

#### `arc`
> Story arc, ordered by `ord`, keyed externally by `ext_id` (e.g. `a_vino`).

```sql
id       INTEGER PRIMARY KEY
book_id  INTEGER NOT NULL REFERENCES book(id) ON DELETE CASCADE
ext_id   TEXT
name     TEXT NOT NULL
color    TEXT
ord      INTEGER DEFAULT 0
UNIQUE(book_id, ext_id)
```

### 3.2 Characters and relations

#### `character`
> Full character card; prose fields are markdown fragments extracted from the bible.

```sql
id             INTEGER PRIMARY KEY
book_id        INTEGER NOT NULL REFERENCES book(id) ON DELETE CASCADE
ext_id         TEXT                      -- c_miya
name           TEXT NOT NULL
aliases        TEXT
is_main        INTEGER DEFAULT 0
status         TEXT                      -- жив / мёртв / ...
age            TEXT
gender         TEXT
group_name     TEXT
summary        TEXT                      -- «Характер: текущий»
character_history TEXT                   -- «История изменений»
voice          TEXT                      -- манера речи
voice_rules    TEXT                      -- чего никогда не скажет
atmosphere_desc  TEXT
atmosphere_level INTEGER DEFAULT 0       -- 0..5
appearance     TEXT
goals          TEXT
beliefs        TEXT
knows          TEXT                      -- «Знает / Не знает»
notes          TEXT
arc_from       TEXT
arc_to         TEXT
arc_stage      TEXT
source_path    TEXT
UNIQUE(book_id, name)
-- INDEX idx_character_book ON character(book_id)
```

#### `character_state`
> Linear, chapter-indexed evolution lines; `chapter_no IS NULL` means backstory / pre-book.

```sql
id            INTEGER PRIMARY KEY
character_id  INTEGER NOT NULL REFERENCES character(id) ON DELETE CASCADE
chapter_no    INTEGER
kind          TEXT DEFAULT 'event'      -- event | trait | appearance | knowledge
text          TEXT NOT NULL
hidden        INTEGER DEFAULT 0         -- 🔒 offscreen
ord           INTEGER DEFAULT 0
-- INDEX idx_charstate_char ON character_state(character_id, chapter_no)
```

#### `relationship`
> Directed relation; `to_char` NULL means the target is a label (group/family).

```sql
id           INTEGER PRIMARY KEY
book_id      INTEGER NOT NULL REFERENCES book(id) ON DELETE CASCADE
from_char    INTEGER NOT NULL REFERENCES character(id) ON DELETE CASCADE
to_char      INTEGER REFERENCES character(id) ON DELETE CASCADE
to_label     TEXT
kind         TEXT                        -- дружба / вражда / романтика / группа
current_state TEXT NOT NULL
history      TEXT
is_mutual    INTEGER DEFAULT 1
UNIQUE(book_id, from_char, to_char, to_label)
-- INDEX idx_rel_book ON relationship(book_id)
```

Because SQLite treats `NULL` as distinct in UNIQUE, `(book, from, NULL, NULL)` rows can
repeat; `importBook` relies on `INSERT OR IGNORE` here.

#### `relationship_state`
> Interval history of a relationship; resolved by the latest `from_moment` at or before `at`.

```sql
id              INTEGER PRIMARY KEY
relationship_id INTEGER NOT NULL REFERENCES relationship(id) ON DELETE CASCADE
chapter_no      INTEGER
to_chapter      INTEGER
from_moment     INTEGER REFERENCES moment(id) ON DELETE SET NULL
to_moment       INTEGER REFERENCES moment(id) ON DELETE SET NULL
state           TEXT NOT NULL
note            TEXT
event_id        INTEGER REFERENCES event(id) ON DELETE SET NULL
```

`event` is declared **later** in the file than this FK — legal in SQLite (FKs resolve at DML
time), a hazard for eager schema validators.

### 3.3 Chapters, events

#### `chapter`
> One prose file; `number` is unique per book; `status` drives most reports.

```sql
id             INTEGER PRIMARY KEY
book_id        INTEGER NOT NULL REFERENCES book(id) ON DELETE CASCADE
ext_id         TEXT                      -- ch7
number         INTEGER NOT NULL
title          TEXT
file_path      TEXT                      -- relative path to prose (forward slashes)
words          INTEGER DEFAULT 0
chars_no_space INTEGER DEFAULT 0
status         TEXT DEFAULT 'план'       -- план | черновик | написано
arc_id         INTEGER REFERENCES arc(id) ON DELETE SET NULL
pov            TEXT
summary        TEXT                      -- from summaries/chNNN.md
plan           TEXT
from_moment    INTEGER REFERENCES moment(id) ON DELETE SET NULL
to_moment      INTEGER REFERENCES moment(id) ON DELETE SET NULL
updated_at     TEXT DEFAULT (datetime('now'))
UNIQUE(book_id, number)
```

No index besides `UNIQUE(book_id, number)` (which SQLite backs with an autoindex).

#### `event`
> A plot event; doubles as the "scene" concept and as a canvas node target.

```sql
id           INTEGER PRIMARY KEY
book_id      INTEGER NOT NULL REFERENCES book(id) ON DELETE CASCADE
ext_id       TEXT
chapter_id   INTEGER REFERENCES chapter(id) ON DELETE SET NULL
chapter_end_id INTEGER REFERENCES chapter(id) ON DELETE SET NULL
title        TEXT NOT NULL
summary      TEXT
status       TEXT DEFAULT 'план'         -- план | написано
hidden       INTEGER DEFAULT 0
weight       INTEGER DEFAULT 0           -- объём в словах
arc_id       INTEGER REFERENCES arc(id) ON DELETE SET NULL
story_date   TEXT
moment_id    INTEGER REFERENCES moment(id) ON DELETE SET NULL
x            REAL
y            REAL
UNIQUE(book_id, ext_id)
-- INDEX idx_event_book ON event(book_id)
-- INDEX idx_event_chapter ON event(chapter_id)
```

#### `event_character`
> Join table with a state snapshot of the character at that event.

```sql
event_id     INTEGER NOT NULL REFERENCES event(id) ON DELETE CASCADE
character_id INTEGER NOT NULL REFERENCES character(id) ON DELETE CASCADE
state_snapshot TEXT                     -- каким персонаж был в этот момент
PRIMARY KEY (event_id, character_id)
```

#### `event_link`
> Directed causal edge between events (default kind `'causal'`).

```sql
from_event INTEGER NOT NULL REFERENCES event(id) ON DELETE CASCADE
to_event   INTEGER NOT NULL REFERENCES event(id) ON DELETE CASCADE
kind       TEXT DEFAULT 'causal'
PRIMARY KEY (from_event, to_event)
```

### 3.4 Hidden lines, secrets, promises

#### `hidden_line`
> A concealed storyline with its truth and who knows it.

```sql
id         INTEGER PRIMARY KEY
book_id    INTEGER NOT NULL REFERENCES book(id) ON DELETE CASCADE
code       TEXT                          -- H1
name       TEXT NOT NULL
truth      TEXT                          -- ПРАВДА
who_knows  TEXT
reader_learns TEXT
status     TEXT
UNIQUE(book_id, code)
```

#### `hidden_beat`
> Off-screen chronology of a hidden line: what happened but was not shown.

```sql
id             INTEGER PRIMARY KEY
hidden_line_id INTEGER NOT NULL REFERENCES hidden_line(id) ON DELETE CASCADE
chapter_no     INTEGER
offscreen      TEXT NOT NULL
visible_effect TEXT
planned        INTEGER DEFAULT 0         -- 1 = not implemented yet
```

#### `hint`
> A seeded hint, possibly not yet attached to a hidden line (`hidden_line_id` nullable).

```sql
id             INTEGER PRIMARY KEY
hidden_line_id INTEGER REFERENCES hidden_line(id) ON DELETE CASCADE
chapter_no     INTEGER
text           TEXT NOT NULL
carrier        TEXT                      -- через что / через кого
level          INTEGER DEFAULT 0         -- 0..5
```

#### `secret`
> A secret with reveal plan and current reader knowledge.

```sql
id           INTEGER PRIMARY KEY
book_id      INTEGER NOT NULL REFERENCES book(id) ON DELETE CASCADE
ext_no       TEXT
title        TEXT NOT NULL
truth        TEXT
reader_knows TEXT
reveal_plan  TEXT
```

#### `secret_knowledge`
> Who holds which stance toward a secret. **No primary key and no UNIQUE** —
> duplicates are possible and are only prevented by import-time logic.

```sql
secret_id    INTEGER NOT NULL REFERENCES secret(id) ON DELETE CASCADE
character_id INTEGER REFERENCES character(id) ON DELETE CASCADE
label        TEXT                        -- «весь клуб» if not a character
stance       TEXT NOT NULL               -- знает | заблуждается | не знает
```

#### `promise`
> A "Chekhov gun": what was promised to the reader and where it should fire.

```sql
id             INTEGER PRIMARY KEY
book_id        INTEGER NOT NULL REFERENCES book(id) ON DELETE CASCADE
ext_no         TEXT
title          TEXT NOT NULL
planted_ch     INTEGER
expected_payoff TEXT
status         TEXT DEFAULT 'ожидает'
fired_ch       INTEGER
```

### 3.5 Activity, world, "now" snapshot

#### `mcp_activity`
> Append-only log of what the AI actually did to the book (one row per write tool call).

```sql
id         INTEGER PRIMARY KEY
book_id    INTEGER REFERENCES book(id) ON DELETE CASCADE
tool       TEXT NOT NULL
summary    TEXT NOT NULL
detail     TEXT                          -- JSON args, for debugging
created_at TEXT DEFAULT (datetime('now'))
```

#### `world_rule`
> Bullet-level world rules; `locked=1` (🔒) means inviolable.

```sql
id       INTEGER PRIMARY KEY
book_id  INTEGER NOT NULL REFERENCES book(id) ON DELETE CASCADE
category TEXT
text     TEXT NOT NULL
locked   INTEGER DEFAULT 0
ord      INTEGER DEFAULT 0
```

#### `location`
> A place card extracted from `bible/locations/*.md`.

```sql
id          INTEGER PRIMARY KEY
book_id     INTEGER NOT NULL REFERENCES book(id) ON DELETE CASCADE
name        TEXT NOT NULL
kind        TEXT
where_desc  TEXT
description TEXT
who_here    TEXT
events_here TEXT
source_path TEXT
UNIQUE(book_id, name)
```

#### `glossary`
> Canonical spellings; uniqueness is per section.

```sql
id       INTEGER PRIMARY KEY
book_id  INTEGER NOT NULL REFERENCES book(id) ON DELETE CASCADE
term     TEXT NOT NULL
meaning  TEXT
section  TEXT                            -- имена / персонажи книги / термины
UNIQUE(book_id, term, section)
```

#### `timeline_entry`
> Raw imported chronology rows, before `moment` construction.

```sql
id          INTEGER PRIMARY KEY
book_id     INTEGER NOT NULL REFERENCES book(id) ON DELETE CASCADE
chapter_no  INTEGER
story_date  TEXT
moment_id   INTEGER REFERENCES moment(id) ON DELETE SET NULL
description TEXT NOT NULL
who         TEXT
hidden      INTEGER DEFAULT 0
ord         INTEGER DEFAULT 0
```

#### `current_state`
> The "right now" snapshot document; write-once per import, read by export.

```sql
id         INTEGER PRIMARY KEY
book_id    INTEGER NOT NULL REFERENCES book(id) ON DELETE CASCADE
last_chapter INTEGER
story_date TEXT
positions  TEXT                          -- markdown table «где кто»
conflicts  TEXT
open_questions TEXT
updated_at TEXT DEFAULT (datetime('now'))
```

### 3.6 Facts and knowledge (the temporal core)

#### `fact_kind`
> Controlled vocabulary for fact kinds, as a **tree**; `slotted=1` means one current value
> per (subject, kind, key), `slotted=0` means it accumulates forever.

```sql
code     TEXT PRIMARY KEY
title    TEXT NOT NULL
parent   TEXT REFERENCES fact_kind(code)
slotted  INTEGER NOT NULL DEFAULT 1
key_hint TEXT
ord      INTEGER DEFAULT 0
```

Seeded with `INSERT OR IGNORE` (48 rows) — roots first, then leaves:

| root | ord | children (code ord `key_hint`) |
|---|---|---|
| `внешность` Облик | 10 | `телосложение` 11 `рост · комплекция`, `лицо` 12 `глаза · черты · кожа`, `причёска` 13, `одежда` 14 `верх · низ · обувь · верхняя · головной убор`, `аксессуар` 15 `украшение · очки · часы · сумка`, `примета` 16 `шрам · татуировка · родинка · протез` |
| `предмет` При себе | 20 | `оружие` 21 `нож · пистолет · меч`, `документ` 22 `паспорт · письмо · карта`, `ценность` 23 `кошелёк · украшение`, `артефакт` 24, `расходник` 25 `еда · зелье · патроны`, `транспорт` 26 |
| `тело` Тело | 30 | `рана` 31 `где именно`, `болезнь` 32, `самочувствие` 33 `усталость · голод · опьянение`, `возраст` 34 |
| `нутро` Внутреннее | 40 | `черта` 41, `эмоция` 42, `травма` 43, `страх` 44, `убеждение` 45, `привычка` 46, `цель` 47 `ближняя · дальняя` |
| `умения` Умения | 50 | `навык` 51 `боевой · ремесло · общение`, `эрудиция` 52 `лор · наука · язык`, `способность` 53, `ограничение` 54 `к какой способности`, `уровень` 55 |
| `социальное` Социальное | 60 | `имя` 61 `настоящее · псевдоним · прозвище`, `статус` 62 `титул · должность`, `принадлежность` 63 `группа · фракция`, `репутация` 64 `у кого именно`, `долг` 65 `перед кем`, `состав` 66 |
| `положение` Положение | 70 | `место` 71, `дом` 72, `маршрут` 73 |
| `сюжет` Сюжет | 80 | `обязательство` 81, `правило` 82, `событие` 83 — **slotted=0** |

Followed by 10 idempotent repair statements that attach pre-tree kinds:

```sql
UPDATE fact_kind SET parent='нутро', ord=41 WHERE code='черта' AND parent IS NULL;
-- … 'эмоция', 'цель', 'имя', 'статус', 'состав', 'место', 'обязательство', 'правило'
UPDATE fact_kind SET parent='сюжет', ord=83, slotted=0 WHERE code='событие' AND parent IS NULL;
```

The `AND parent IS NULL` guards are load-bearing: a re-run must not re-parent a kind the
user (or a future migration) already moved.

#### `fact`
> The core primitive: one assertion with a validity **interval** on the moment axis.

```sql
id           INTEGER PRIMARY KEY
book_id      INTEGER NOT NULL REFERENCES book(id) ON DELETE CASCADE
subject_type TEXT NOT NULL      -- character | location | group | world | plot (also: item)
subject_id   INTEGER
subject_name TEXT               -- when the subject has no row
kind         TEXT NOT NULL REFERENCES fact_kind(code)
key          TEXT               -- slot name: «оружие», «одежда», «дом»
value        TEXT NOT NULL
weight       INTEGER DEFAULT 1  -- 0 background · 1 normal · 2 key
from_moment  INTEGER REFERENCES moment(id) ON DELETE SET NULL
to_moment    INTEGER REFERENCES moment(id) ON DELETE SET NULL
from_chapter INTEGER            -- duplicated for filtering
to_chapter   INTEGER
from_seq     REAL               -- denormalized copy of moment.seq
to_seq       REAL
event_id     INTEGER REFERENCES event(id) ON DELETE SET NULL
status       TEXT DEFAULT 'канон'    -- канон | план
hidden       INTEGER DEFAULT 0       -- 🔒 unseen by reader
source       TEXT DEFAULT 'вручную'  -- импорт | вручную | ИИ | ИИ (MCP) | ИИ: принято автором
note         TEXT
source_chapter_id INTEGER REFERENCES chapter(id) ON DELETE SET NULL
source_quote TEXT
created_at   TEXT DEFAULT (datetime('now'))
-- INDEX idx_fact_subject ON fact(book_id, subject_type, subject_id)
-- INDEX idx_fact_slice   ON fact(book_id, from_seq, to_seq)
-- INDEX idx_fact_slot    ON fact(book_id, subject_type, subject_id, kind, key, to_seq)
-- INDEX idx_fact_kind    ON fact(book_id, kind, from_seq)
```

Interval semantics: a fact is true at `seq` when
`COALESCE(from_seq, -1e9) <= seq AND (to_seq IS NULL OR to_seq > seq) AND status='канон'`.

#### `knowledge_stance`
> Three-valued (really four-valued) stance dictionary.

```sql
code  TEXT PRIMARY KEY
title TEXT NOT NULL
ord   INTEGER DEFAULT 0
```
Seeded: `знает` 1 `Знает правду`, `подозревает` 2, `заблуждается` 3 `Верит в неверное`,
`не знает` 4.

#### `knowledge`
> Who believes/knows what, with the same interval model as `fact`.

```sql
id           INTEGER PRIMARY KEY
book_id      INTEGER NOT NULL REFERENCES book(id) ON DELETE CASCADE
character_id INTEGER REFERENCES character(id) ON DELETE CASCADE
holder_label TEXT               -- collective holder («город», «клуб»)
secret_id    INTEGER REFERENCES secret(id) ON DELETE CASCADE
topic        TEXT               -- free-form topic when not a listed secret
stance       TEXT NOT NULL REFERENCES knowledge_stance(code)
belief       TEXT               -- what exactly they wrongly believe
from_moment  INTEGER REFERENCES moment(id) ON DELETE SET NULL
to_moment    INTEGER REFERENCES moment(id) ON DELETE SET NULL
from_chapter INTEGER
to_chapter   INTEGER
from_seq     REAL
to_seq       REAL
event_id     INTEGER REFERENCES event(id) ON DELETE SET NULL
source       TEXT DEFAULT 'вручную'
note         TEXT
source_chapter_id INTEGER REFERENCES chapter(id) ON DELETE SET NULL
source_quote TEXT
-- INDEX idx_knowledge_char  ON knowledge(book_id, character_id, to_seq)
-- INDEX idx_knowledge_slice ON knowledge(book_id, from_seq, to_seq)
```

#### `reader_knowledge`
> What the **reader** knows, tracked per topic; newer table, single-point (`from_*`) only.

```sql
id INTEGER PRIMARY KEY
book_id INTEGER NOT NULL REFERENCES book(id) ON DELETE CASCADE
topic TEXT NOT NULL
stance TEXT NOT NULL DEFAULT 'знает'
belief TEXT
from_moment INTEGER REFERENCES moment(id) ON DELETE SET NULL
from_seq REAL
source_chapter_id INTEGER REFERENCES chapter(id) ON DELETE SET NULL
source_quote TEXT
source TEXT DEFAULT 'вручную'
status TEXT DEFAULT 'канон'
-- INDEX idx_reader_knowledge_slice ON reader_knowledge(book_id, from_seq, topic)
```

### 3.7 Graph canvas

#### `node`
> Free-form canvas node; `ref_table`/`ref_id` optionally point at a real entity, `data` is
> JSON with type-specific fields (e.g. `{text}` for `type='plan'`).

```sql
id        INTEGER PRIMARY KEY
book_id   INTEGER NOT NULL REFERENCES book(id) ON DELETE CASCADE
type      TEXT NOT NULL        -- chapter | event | character | plan | branch | promise | comment
ref_table TEXT
ref_id    INTEGER
x         REAL DEFAULT 0
y         REAL DEFAULT 0
w         REAL
h         REAL
data      TEXT
collapsed INTEGER DEFAULT 0
-- INDEX idx_node_book ON node(book_id)
```

#### `edge`
> Canvas edge; `kind='flow'` is chapter order, `'data'` is causal/participation.

```sql
id        INTEGER PRIMARY KEY
book_id   INTEGER NOT NULL REFERENCES book(id) ON DELETE CASCADE
from_node INTEGER NOT NULL REFERENCES node(id) ON DELETE CASCADE
from_port TEXT
to_node   INTEGER NOT NULL REFERENCES node(id) ON DELETE CASCADE
to_port   TEXT
kind      TEXT DEFAULT 'flow'
label     TEXT
```

No UNIQUE on `(book_id, from_node, to_node)` — duplicate prevention is route-level (409).

### 3.8 Annotations, AI runs, versions, style, FTS

#### `annotation`
> A note attached to a text fragment; quote + ±~30 chars of context make it re-locatable.

```sql
id         INTEGER PRIMARY KEY
chapter_id INTEGER NOT NULL REFERENCES chapter(id) ON DELETE CASCADE
quote      TEXT NOT NULL
prefix     TEXT          -- ~30 chars before
suffix     TEXT          -- ~30 chars after
start_pos  INTEGER
end_pos    INTEGER
note       TEXT NOT NULL
status     TEXT DEFAULT 'открыта'   -- открыта | применена | отклонена | потеряна
ai_result  TEXT
created_at TEXT DEFAULT (datetime('now'))
resolved_at TEXT
-- INDEX idx_annotation_chapter ON annotation(chapter_id, status)
```

#### `ai_run`
> One row per AI call; doubles as the chat transcript source and the token ledger.

```sql
id          INTEGER PRIMARY KEY
book_id     INTEGER NOT NULL REFERENCES book(id) ON DELETE CASCADE
chapter_id  INTEGER REFERENCES chapter(id) ON DELETE SET NULL
kind        TEXT            -- chat | agent | extract_facts | plan | draft | audit | style_review | suggest_edits
engine      TEXT            -- cli | api
model       TEXT
prompt      TEXT
result      TEXT
actions_json TEXT           -- agent actions, without the full chapter text
status      TEXT DEFAULT 'running'   -- running | ok | error | cancelled
error       TEXT
tokens_in   INTEGER
tokens_out  INTEGER
created_at  TEXT DEFAULT (datetime('now'))
finished_at TEXT
```

#### `chapter_version`
> Snapshot of the **previous** body, written before overwriting the file.

```sql
id         INTEGER PRIMARY KEY
chapter_id INTEGER NOT NULL REFERENCES chapter(id) ON DELETE CASCADE
body       TEXT NOT NULL
reason     TEXT            -- ручная правка | правки по заметкам | восстановление версии | правка ИИ через MCP
ai_run_id  INTEGER REFERENCES ai_run(id) ON DELETE SET NULL
created_at TEXT DEFAULT (datetime('now'))
```

#### `style_example`
> Author-approved prose samples, selected by scene type at prompt time.

```sql
id INTEGER PRIMARY KEY
book_id INTEGER NOT NULL REFERENCES book(id) ON DELETE CASCADE
scene_type TEXT NOT NULL
title TEXT
body TEXT NOT NULL
source_chapter_id INTEGER REFERENCES chapter(id) ON DELETE SET NULL
created_at TEXT DEFAULT (datetime('now'))
```

#### `chapter_search` (FTS5)
> Full-text index over chapter bodies; a plain (content-bearing) FTS5 table with two
> UNINDEXED payload columns.

```sql
CREATE VIRTUAL TABLE chapter_search USING fts5(
  book_id UNINDEXED, chapter_id UNINDEXED, body, tokenize='unicode61'
);
```

`book_id`/`chapter_id` are **UNINDEXED**, so `WHERE cs.book_id=?` is a post-filter over
matched rows. The hidden column whose name equals the table (`chapter_search MATCH ?`) is
what `lib/search.js` uses.

### 3.9 Triggers (8 total)

`from_seq`/`to_seq` are copies of `moment.seq`, maintained by triggers so no query needs a
JOIN to `moment`:

| Trigger | Event | Body |
|---|---|---|
| `fact_seq_ins` | `AFTER INSERT ON fact` | sets `from_seq`/`to_seq` from `NEW.from_moment`/`NEW.to_moment` |
| `fact_seq_upd` | `AFTER UPDATE OF from_moment, to_moment ON fact` | same recomputation for `NEW.id` |
| `knowledge_seq_ins` | `AFTER INSERT ON knowledge` | same, knowledge |
| `knowledge_seq_upd` | `AFTER UPDATE OF from_moment, to_moment ON knowledge` | same, knowledge |
| `moment_seq_upd` | `AFTER UPDATE OF seq ON moment` | rewrites `fact.from_seq`, `fact.to_seq`, `knowledge.from_seq`, `knowledge.to_seq` for all rows referencing `NEW.id` |
| `reader_knowledge_seq_ins` | `AFTER INSERT ON reader_knowledge` | `from_seq = (SELECT seq FROM moment WHERE id=NEW.from_moment)` |
| `reader_knowledge_seq_upd` | `AFTER UPDATE OF from_moment ON reader_knowledge` | same |
| `reader_moment_seq_upd` | `AFTER UPDATE OF seq ON moment` | `reader_knowledge.from_seq = NEW.seq WHERE from_moment=NEW.id` |

Consequences that must be preserved:

1. Inserting a `fact` with a `from_moment` produces a **second write** (the trigger's
   UPDATE), which fires `fact_seq_upd` only if `from_moment`/`to_moment` were in the SET
   list — it isn't, so no recursion.
2. Moving a moment's `seq` silently rewrites every dependent fact/knowledge/reader row;
   there is no application-level code doing this.
3. `to_seq` for an open interval stays `NULL` (the subquery returns `NULL` when
   `to_moment IS NULL`).

### 3.10 Migration logic in code (`db/index.js:55` `migrate`)

`CREATE TABLE IF NOT EXISTS` never alters existing tables, so `migrate()` runs
`PRAGMA table_info` and conditionally `ALTER TABLE … ADD COLUMN`, each wrapped in
`try/catch` (a missing table is tolerated and will be created by the schema on the same
boot). Additions, in order:

```
fact_kind.parent TEXT
fact_kind.key_hint TEXT
fact.source_chapter_id INTEGER
fact: weight INTEGER DEFAULT 1, from_seq REAL, to_seq REAL
knowledge: from_seq REAL, to_seq REAL
fact.source_quote TEXT
knowledge.source_chapter_id INTEGER
knowledge.source_quote TEXT
ai_run.actions_json TEXT
chapter.from_moment INTEGER
chapter.to_moment INTEGER
event.moment_id INTEGER
timeline_entry.moment_id INTEGER
relationship_state.to_chapter INTEGER
relationship_state.from_moment INTEGER
relationship_state.to_moment INTEGER
relationship_state.event_id INTEGER
```

(Exact call order: `fact_kind.parent`, `fact_kind.key_hint`, `fact.weight`, `fact.from_seq`,
`fact.to_seq`, `knowledge.from_seq`, `knowledge.to_seq`, `fact.source_chapter_id`,
`fact.source_quote`, `knowledge.source_chapter_id`, `knowledge.source_quote`,
`ai_run.actions_json`, `chapter.from_moment`, `chapter.to_moment`, `event.moment_id`,
`timeline_entry.moment_id`, `relationship_state.{to_chapter,from_moment,to_moment,event_id}`.)

After migration, `getDb()` bootstraps FTS:

```js
const indexed = _db.prepare('SELECT COUNT(*) n FROM chapter_search').get().n;
if (!indexed) for (const { id } of _db.prepare('SELECT id FROM book').all()) indexBookChapters(_db, id);
```

The gate is **global** (not per book): if any row exists, no book is reindexed.

`backfillSeq(db, bookId)` in `lib/facts.js` is the application-level equivalent, used after
bulk import.

---

## 4. Domain model

```
book ─┬─ doc (free markdown)
      ├─ arc ────────────────┐
      ├─ character ──┬─ character_state
      │              ├─ relationship ── relationship_state
      │              ├─ event_character ── event
      │              ├─ knowledge ── secret ── secret_knowledge
      │              └─ fact (subject_type='character')
      ├─ chapter ────┬─ annotation
      │              ├─ chapter_version
      │              └─ chapter_search (FTS)
      ├─ event ──┬─ event_character
      │          ├─ event_link
      │          └─ node(type='event')
      ├─ moment ──── fact / knowledge / reader_knowledge / chapter range / event
      ├─ timeline_entry → moment
      ├─ promise, secret, hint, hidden_line → hidden_beat
      ├─ world_rule, location, glossary, current_state
      ├─ node ── edge
      ├─ style_example
      ├─ ai_run ── chapter_version
      ├─ mcp_activity
      └─ reader_knowledge
```

### 4.1 book
The unit of import and of every query. Identity = `slug` = folder basename.
`root_path` is absolute and is the only link to the filesystem; **prose lives in files,
structural facts live in SQLite** (schema header comment). One book can be re-imported
destructively; see §7.1.

### 4.2 chapter
A numbered unit of prose (`UNIQUE(book_id, number)`), optionally tied to an `arc`, a `pov`,
and a `[from_moment, to_moment]` interval on the time axis. `file_path` is the authored
prose location; when `NULL` the convention is `chapters/chNNN.md` (`ch` + zero-padded
3-digit `number`). `words`, `chars_no_space` and `status='написано'` are **derived on save**
and are not the source of truth (the file is).

### 4.3 scene
There is **no `scene` table and no `scene` route segment meaning "scene"**. A scene is:

* in prose/graph terms — an `event` (`node.type='event'`, `ref_table='event'`), and
* in prompt terms — the **scene packet** built by `buildScenePacket` for a chapter, and
* in graph-layout terms — the vertical column of event nodes under a chapter node.

`GET /api/books/:id/scenes/:chapter/context` returns that packet for a **chapter id**.

### 4.4 event
A plot beat: `title`, `summary`, `status` (`план`/`написано`), `hidden`, `weight` (word
volume, used for node sizing), `arc_id`, `story_date`, `moment_id`, canvas `x`/`y`,
`chapter_id` (start) and `chapter_end_id` (end, for multi-chapter events). Participants are
`event_character` rows carrying a `state_snapshot` string. Causal order is `event_link`.

### 4.5 entity
Two distinct meanings, both in use:

1. **MCP entity** (`lib/entityTools.js`) — a whitelisted table in the `ENTITY` registry:
   `chapter, character, event, promise, relationship, moment, location, secret,
   hidden_line, timeline_entry, node, edge, character_state, relationship_state,
   hidden_beat, annotation` (16 types; `arc` is handled specially in `assertBook` only, not
   in `ENTITY`, so `arc` cannot be created through the generic tools). Each type declares
   `fields` (patchable), optional `create` (insertable subset), `required`, and optional
   `parent: [table, fkColumn]` for ownership resolution.
2. **Canvas entity** — `node.ref_table` + `node.ref_id`. `loadGraph` attaches the live row
   as `node.entity` for `ref_table ∈ {'chapter','event'}` only.

### 4.6 fact
One assertion about a subject, valid over an interval. The tuple
**(subject_type, subject_id, kind, key)** is a **slot** when `fact_kind.slotted=1`:
writing a new value closes the previous one instead of adding a sibling. `weight` (0/1/2)
drives prompt selection; `status` separates written canon (`канон`) from planned
(`план`); `hidden` marks reader-invisible truth; `source_quote` + `source_chapter_id`
retain provenance.

### 4.7 knowledge
Same interval model, but with a **stance** instead of a value: `знает`, `подозревает`,
`заблуждается`, `не знает`. Holder is either `character_id` or `holder_label` (collective).
`belief` stores the exact false belief when `stance='заблуждается'` — the schema comment is
explicit that misconception ≠ ignorance and that it drives plot.

### 4.8 reader_knowledge
Separate table for what the **reader** knows (not any character). Only `from_*` (a point,
not an interval); the "current" value per topic is the row with the greatest
`COALESCE(from_seq,-1e9)` (`ROW_NUMBER() OVER (PARTITION BY topic …)` in `scenePacket.js`).
`status='канон'` filters planned-only rows.

### 4.9 annotation
A note anchored to a text fragment of one chapter: `quote` (the selected text), `prefix` and
`suffix` (~30 chars each) for re-anchoring, optional `start_pos`/`end_pos`, the author's
`note`, a lifecycle `status` (`открыта` → `применена` | `отклонена` | `потеряна`) and
`ai_result` (the replacement text that was applied). Annotations are the unit of
"edit by instruction": R10 proposes, R79 applies.

### 4.10 version
`chapter_version` rows: the **previous** body, written immediately before a file overwrite,
with a `reason` and optional `ai_run_id`. The current text is never stored in this table;
`GET /versions` exposes only `LENGTH(body)` as `chars`, and restoring is a normal save with
`reason='восстановление версии'` (which itself creates a new version of the pre-restore
text).

### 4.11 proposal
Not a table. A JSON object shaped
`{type:'fact'|'knowledge'|'reader', …}` produced by the model, validated by
`validateProposals`, and committed by `commitProposals` in one transaction. Fact proposals
carry `subject_type`, `subject_id`, `kind`, `key`, `value`, `hidden`, `source_quote`;
knowledge proposals carry `character_id`, `topic`, `stance`, `belief`, `source_quote`;
reader proposals carry `topic`, `stance`, `belief`, `source_quote`. The mandatory
`source_quote` must be a **verbatim substring of the chapter body** (4..1000 chars).

### 4.12 node / edge
`node` is a canvas box: `type` (free string, conventionally one of
`chapter|event|character|plan|branch|promise|comment`), optional reference to a real entity,
position/size, `data` (JSON, e.g. `{"text": "…"}` for plan notes), `collapsed`.
`edge` connects two nodes with optional ports, `kind` (`flow` for chapter order, `data` for
causal/participation) and `label`. The graph is seeded from chapters+events on first read
(§6.9) and is the only place where layout is stored.

### 4.13 moment
The ordering primitive. `seq` is REAL and stepped by 100 during import, so new points can be
inserted between two existing ones without renumbering. `world` splits parallel timelines
(`реальный мир` vs `мир книги`). "The state at time T" is always a query over
`from_seq`/`to_seq`, never a re-read of prose.

---

## 5. `server/lib/*` module reference

### 5.1 `lib/http.js`

| Export | Signature | Semantics |
|---|---|---|
| `createApp` | `({staticDir}) => app` | Builds router + `node:http` server; `app.listen(port)` binds `127.0.0.1` |
| `fail` | `(status, message) => {__status, data:{error}}` | Non-200 JSON error envelope |
| `raw` | `(status, body, headers={}) => {__raw, status, body, headers}` | Bypasses JSON serialization; `body==null` → empty response |
| internal `match` | `(route, segments) => params\|null` | Exact-count, in-order segment match; `decodeURIComponent` on `:` parts |
| internal `readBody` | `(req) => parsed\|undefined` | Buffers the whole body; empty → `undefined`; non-JSON → 400 |
| internal `serveStatic` | `(pathname, res) => boolean` | Traversal-guarded file serving, `Cache-Control: no-cache` |

### 5.2 `lib/crud.js`

```js
makeCrud(db) -> {
  patch(table, id, allowed, body) -> {changed},
  insert(table, data)             -> row,
  remove(table, id)               -> {changed}
}
```

* `patch` filters `Object.keys(body)` through the `allowed` array, then builds
  `UPDATE <table> SET k1=?,k2=? WHERE id=?` — **identifiers from the whitelist, values
  parameterized**. Zero allowed keys → `{changed:0}` with no statement executed.
* `insert` uses the object's own keys as the column list (so callers must pre-filter);
  returns the row by `lastInsertRowid`.
* `remove` is a hard `DELETE`.

`EDITABLE` (the whitelists):

| table | fields |
|---|---|
| `character` | `name, aliases, is_main, status, age, gender, group_name, summary, character_history, voice, voice_rules, atmosphere_desc, atmosphere_level, appearance, goals, beliefs, knows, notes, arc_from, arc_to, arc_stage` |
| `chapter` | `number, title, status, arc_id, pov, summary, plan` |
| `event` | `chapter_id, chapter_end_id, title, summary, status, hidden, weight, arc_id, story_date, x, y` |
| `node` | `x, y, w, h, data, collapsed, type, ref_table, ref_id` |
| `annotation` | `note, status, ai_result, quote, prefix, suffix, start_pos, end_pos` |
| `promise` | `title, planted_ch, expected_payoff, status, fired_ch` |
| `character_state` | `chapter_no, kind, text, hidden, ord` |
| `relationship` | `kind, current_state, history, is_mutual, to_char, to_label` |
| `fact` | `subject_type, subject_id, subject_name, kind, key, value, from_moment, to_moment, from_chapter, to_chapter, event_id, status, hidden, note` |
| `knowledge` | `character_id, holder_label, secret_id, topic, stance, belief, from_moment, to_moment, from_chapter, to_chapter, event_id, note` |
| `moment` | `seq, label, world, story_date, note` |

Deliberately **not** patchable: `book_id`, `created_at`, `source`, `source_quote`,
`source_chapter_id`, `chapter.file_path`, `annotation.chapter_id`, `relationship.from_char`.

### 5.3 `lib/md.js` — markdown bible parser

| Export | Signature | Semantics |
|---|---|---|
| `charsNoSpace` | `(text) => number` | Strips `^#{1,6}\s*` per line, all `**`, all `---`; splits on `\s+`, joins, `.length` (**UTF-16 code units**) |
| `wordCount` | `(text) => number` | Same cleanup; counts non-empty `\s+` tokens |
| `h1` | `(text) => string\|null` | First `^#\s+(.+)$` (multiline), trimmed |
| `frontmatter` | `(text) => Record<string,string>` | `^---\n([\s\S]*?)\n---` with `m`; splits each line at the **first** `:`; not YAML |
| `sections` | `(text) => Record<title, body>` | Marks at `^(#{2,4})\s+(.+)$`; a section ends at the next heading with level ≤ its own |
| `bullets` | `(block) => string[]` | Top-level `-`/`*` items; continuation lines joined with a space; blank line closes the item; a line starting `\|` or `#` is neither appended nor closes the open item |
| `table` | `(block) => Record<col,val>[]` | Pipe lines only; needs ≥2 lines; first is the header; separator rows (`^\|?[\s:\|-]+\|?$`) and all-empty rows skipped; missing cells become `''` |
| `chapterNo` | `(text) => number\|null` | `/гл\.?\s*(\d+)/i` |
| `isHidden` | `(text) => boolean` | `/🔒\|закадров\|за кадром\|скрыт/i` |
| `hintLevel` | `(text) => number\|null` | `/ур\.?\s*(\d)/i` |
| `levelOutOf5` | `(text) => number` | `/(\d)\s*\/\s*5/`, default `0` |
| `labelled` | `(block, label) => string\|null` | Tries `\*\*<label>[:：]?\*\*[:：]?\s*(.+)` (i), then `^\s*[-*]?\s*<label>[:：]\s*(.+)$` (im); `label` is interpolated as a **regex fragment** (callers escape, e.g. `'Название \\(рабочее\\)'`) |

Known edge: `frontmatter` requires **LF** line endings; a CRLF file silently yields `{}`.

### 5.4 `lib/anchor.js` — text anchoring

```js
paragraphs(text) -> [{start, end, text}]
findFlexible(text, needle) -> {start, end, count, exact}
locateNote(text, note) -> {found, how, start, end, occurrences, unique, paragraphIndex, paragraphCount, paragraph, before, after}
planEdits(text, edits, notesById) -> {planned, rejected}
applyPlanned(text, planned) -> string
```

* `paragraphs` splits on `/\n\s*\n/g`, records offsets, and **drops empty paragraphs**
  (so a text of only whitespace yields `[]`).
* `findFlexible`: exact `indexOf` counting first (`countOccurrences`, non-overlapping,
  advancing by `needle.length`); on failure builds a regex from the **trimmed** needle with
  every regex metacharacter escaped and every whitespace run replaced by `\s+`, global, then
  counts matches. Returns the **first** match position and total count. A regex construction
  failure is swallowed and reported as `{start:-1,end:-1,count:0}`.
* `locateNote` attempt ladder, with `how` values in order:
  1. `'позиция'` — `text.slice(start_pos, end_pos) === quote` (end derived from quote length);
  2. `'контекст'` — `findFlexible(prefix + quote + suffix)`, then the quote located **inside
     that slice** (offsets rebased);
  3. `'префикс'` — `findFlexible(prefix + quote)` then quote inside it (requires non-empty prefix);
  4. `'цитата'` (exact) or `'цитата (с допуском)'` (whitespace-tolerant) — quote alone.
  Not found → `{found:false, occurrences:0, how:null}` (no other keys).
  On success it maps both offsets onto the paragraph list, takes `from..last` (multi-paragraph
  selections include every spanned paragraph joined with `\n\n`), sets
  `occurrences = countOccurrences(text, quote) || 1` and `unique = occurrences === 1`.
* `planEdits` — per edit, in input order:
  rejects non-string `old`/`new`, rejects an `annotation_id` that is not in `notesById`
  (message `'заметка не принадлежит этой главе'`), rejects empty `old`;
  `count===1` → planned at `text.indexOf(old)`; `count===0` → rejected
  `'фрагмент не найден в главе'`; `count>1` → try `note.prefix + old` (exact index), else
  `locateNote` neighbourhood (`text.indexOf(old, anchor.start-400)` accepted only if
  `< anchor.end + 400`); otherwise rejected `` `фрагмент встречается ${count} раз — неоднозначно` ``.
  Finally planned edits are sorted by `start` and any edit starting before the previous
  accepted edit's end is rejected with `'пересекается с другой правкой'`
  (so at most one of `{old:'abc'},{old:'bcd'}` on `'abcd'` survives).
* `applyPlanned` applies **right-to-left** (`sort((a,b)=>b.start-a.start)`) so earlier
  offsets stay valid. Planned entries are `{...edit, start, old, new}`.

### 5.5 `lib/chapterText.js` — file I/O and revisions

```js
revisionOf(body) -> sha256 hex of utf8 body
chapterFile(db, chapterId) -> {ch, rel, full}
readChapterText(db, chapterId) -> {ch, rel, full, body, revision, missing}
saveChapterText(db, chapterId, body, {expectedRevision, reason='ручная правка', aiRunId=null}) -> {...}
indexBookChapters(db, bookId) -> void
```

* `chapterFile`: `rel = ch.file_path || join('chapters', 'ch' + padStart(number,3) + '.md')`;
  `full = join(root_path, rel)`; throws `404 'Глава не найдена'` or
  `400 'Недопустимый путь главы'` when `isAbsolute(rel)`, or when `relative(root, full)`
  is `'..'`/starts with `'..\\'`/`'../'`.
* `readChapterText`: `missing = Boolean(file_path) && !existsSync(full)`; `body=''` when the
  file is absent (a chapter may legitimately have no file yet).
* `saveChapterText` flow:
  1. `typeof body !== 'string'` → `400 'Нет текста'`.
  2. Read current state; if `expectedRevision != null && !== revision` → `409`
     `'Глава изменилась после открытия. Сравните её с черновиком перед сохранением.'`
     (**checked before the transaction opens** — TOCTOU window exists but the process is
     single-threaded and synchronous, so it cannot interleave).
  3. Fast path: `body === previous && existsSync(full)` → returns
     `{ok:true, unchanged:true, revision, chars_no_space, words}` **without** writing a
     version or touching the DB.
  4. `db.exec('BEGIN IMMEDIATE')`.
  5. If the file exists → `INSERT INTO chapter_version (chapter_id, body, reason, ai_run_id)`
     with the **previous** body.
  6. `atomicWrite(full, body)`: `mkdirSync(dirname, {recursive:true})`, write to
     `.<randomUUID()>.tmp` in the same directory, `renameSync` over the target; on failure
     unlink the temp file and rethrow.
  7. `UPDATE chapter SET file_path=<rel with `\`→`/`>, words=?, chars_no_space=?,
     status='написано', updated_at=datetime('now')` — **always forces `написано`**.
  8. `DELETE FROM chapter_search WHERE chapter_id=?` then
     `INSERT INTO chapter_search (book_id, chapter_id, body)`.
  9. `COMMIT`; returns `{ok:true, revision: revisionOf(body), chars_no_space, words}`.
  10. On any error: `ROLLBACK` (its failure is ignored), then best-effort restore of the
      previous file (`atomicWrite(full, previous)` if there was content, else `unlinkSync`),
      and if that also fails appends
      `'; не удалось восстановить файл — проверьте главу на диске'` to the error message.
* `indexBookChapters`: deletes all FTS rows for the book and re-inserts every chapter with
  `file_path IS NOT NULL` whose file exists.

### 5.6 `lib/state.js` — temporal queries

`const FAR_PAST = -1e9;` is the sentinel compared against `from_seq` when it is NULL.

```js
resolveMoment(db, bookId, {moment, chapter}) -> moment | {id:null, seq:0, label:'начало'}
factsAt(db, bookId, seq, opts) -> fact[]
recentEvents(db, bookId, seq, {characters, limit=30, minWeight}) -> fact[]
knowledgeAt(db, bookId, seq, {characters, strict=false}) -> knowledge[] (+who, secret_title)
relationshipsAt(db, bookId, seq, {characters}) -> [...]
worldState(db, bookId, at, opts) -> {...}
groupByBranch(db, facts) -> [{code, title, ord, kinds:[{code, title, items}]}]
characterState(db, characterId, at, opts) -> {...} | null
searchFacts(db, bookId, {q, at, characters, kinds, limit=50}) -> fact[] (+subject, from_label)
diff(db, bookId, from, to) -> {from, to, appeared, ended, learned}
```

**`resolveMoment` precedence** (each step must hold, else fall through):
1. `moment` query → row with that `id` **and** matching `book_id`;
2. `chapter` query (a chapter **number**) → that chapter's `to_moment`;
3. the `to_moment` of the last chapter with `status='написано'`, ordered by `m.seq DESC`;
4. the last `moment` of the book by `seq DESC`;
5. `{id: null, seq: 0, label: 'начало'}`.

**`factsAt`** base predicate:

```sql
f.book_id = ?
AND COALESCE(f.from_seq, -1e9) <= ?
AND (f.to_seq IS NULL OR f.to_seq > ?)
AND f.status = 'канон'
```

plus optional: `characters` →
`(f.subject_type <> 'character' OR f.subject_id IN (…))`; `kinds` → `expandKinds` (branch
expansion); `minWeight` → `f.weight >= ?`; `slottedOnly` →
`f.kind IN (SELECT code FROM fact_kind WHERE slotted = 1)`; `subjectType`; `subjectId`;
`limit` (string-interpolated integer, not a bind).
Order: `f.weight DESC, f.from_seq DESC, f.id DESC`.

**`recentEvents`**: `kind='событие'`, `COALESCE(from_seq,-1e9) <= seq`, `status='канон'`,
optional `subject_id IN (…)` and `weight >=`, order `from_seq DESC, id DESC`, `LIMIT ?`.

**`knowledgeAt`**: interval predicate identical to facts; no `status` filter at all; with
`characters`, `strict=true` → `k.character_id IN (…)`, otherwise
`(k.character_id IS NULL OR k.character_id IN (…))`. Joins `character`/`secret` for
`who`/`secret_title`; order `c.name, k.id`.

**`relationshipsAt`**: picks the newest `relationship_state` whose `from_moment.seq` is
`<= at` via a correlated subquery, `COALESCE(rs.state, r.current_state)`. Filter accepts a
relation if either endpoint is in `characters`. Order `f.name`.

**`worldState`**: the compact slice. `slots = factsAt(..., slottedOnly: !full)`;
`events` are added only when `!full && eventLimit > 0 && (no kinds filter || kinds includes
'событие')`; `eventLimit = opts.events ?? 30`. Facts are bucketed: character facts by
`subject_id`, then `plot`, `location`, everything else into `world`. Knowledge is bucketed
by `character_id` only. Returns:

```js
{
  at,                                       // the resolved moment row
  scope: {characters: ids|null|'все', events, kinds, full},
  totals: {facts_in_slice, facts_total, knowledge_in_slice},
  characters: [{id, name, is_main, status, group_name, summary, voice,
                atmosphere_desc, atmosphere_level,
                facts[], knows[], suspects[], misbeliefs[], unaware[]}],
  relationships: [...],
  plot: fact[], world: fact[], locations: fact[],
  open_promises: promise[]          // WHERE status NOT LIKE 'выстрелило%' ORDER BY id
}
```

**`characterState`**: `fact` slots for one subject (`slottedOnly: !full`), `events`
(default limit 20), both merged into `facts` (grouped by `kind`) and `branches`; knowledge
with `strict:true`; `awareness` split into `knows/suspects/misbeliefs/unaware`;
`relationships`; `history_available` = total count of that character's facts.

**`searchFacts`**: `f.value LIKE %q% OR f.key LIKE %q%` (case-insensitivity comes from
SQLite's ASCII-only `LIKE`, so Cyrillic is effectively case-sensitive); optional `at`
(lower bound on `from_seq`), `characters`, `kinds` (expanded); order `from_seq DESC,
id DESC`, `LIMIT`.

**`diff`**: normalizes arguments so `a.seq <= b.seq`; three independent queries, each
`LIMIT 200`:
`appeared` = `from_seq > a.seq AND from_seq <= b.seq` ordered by `from_seq`;
`ended` = `to_seq > a.seq AND to_seq <= b.seq` ordered by `to_seq`;
`learned` = same window on `knowledge.from_seq` ordered by `from_seq`.
No `status`/`hidden` filtering.

### 5.7 `lib/facts.js` — slotted writes

```js
slottedKinds(db) -> Set<string>
expandKinds(db, kinds) -> string[]
kindTree(db) -> [{...fact_kind, children: [...]}]
rootOf(db, code) -> string
momentOfChapter(db, bookId, chapterNo) -> momentId|null
normalizeKind(db, kind) -> string        // unknown → 'событие'
normalizeStance(db, stance) -> string    // unknown → 'знает'
addFact(db, bookId, input) -> {fact, closed:[{id,value}]}
addKnowledge(db, bookId, input) -> {knowledge, closed:[{id,stance}]}
backfillSeq(db, bookId) -> void
```

* `expandKinds` uses a recursive CTE with `UNION` (dedup):
  `WITH RECURSIVE tree(code) AS (SELECT code FROM fact_kind WHERE code IN (…) UNION SELECT k.code FROM fact_kind k JOIN tree t ON k.parent = t.code) SELECT code FROM tree`.
* `rootOf` walks **up** with `UNION ALL`, returning the ancestor whose `parent IS NULL`;
  falls back to the input code.
* `normalizeKind`: `String(kind || 'событие').trim().toLowerCase()`, validated against
  `fact_kind`; unknown → `'событие'`. So `'Оружие'` → `'оружие'`, `'foo'` → `'событие'`.
  Same shape for stances with default `'знает'`.
* `addFact` algorithm:
  1. normalize `kind`; `key = input.key ?? null`; `subject_type` default `'character'`.
  2. `fromMoment = input.from_moment ?? momentOfChapter(bookId, input.from_chapter)`;
     `fromSeq` = that moment's `seq` or `null`.
  3. If `slotted` **and** `fromSeq !== null`, select previous rows:
     ```sql
     WHERE book_id=? AND subject_type=? AND kind=?
       AND (subject_id IS ? OR subject_id = ?)
       AND (key IS ? OR key = ?)
       AND from_seq < ?
       AND (to_seq IS NULL OR to_seq > ?)
       AND status='канон'
     ```
     and for each: `UPDATE fact SET to_moment=?, to_chapter=? WHERE id=?` with
     `to_moment = fromMoment`, `to_chapter = input.from_chapter ?? null`.
     The `IS ?/OR =` pairs are the NULL-safe comparison idiom.
  4. Insert with columns
     `(book_id, subject_type, subject_id, subject_name, kind, key, value, weight,
     from_moment, from_chapter, to_moment, to_chapter, event_id, status, hidden, source,
     note, source_chapter_id, source_quote)`; defaults `weight=1`, `status='канон'`,
     `hidden = input.hidden ? 1 : 0`, `source='вручную'`.
  5. Re-read the row (which now has trigger-populated `from_seq`/`to_seq`).
  **No transaction** wraps close+insert.
* `addKnowledge` mirrors it; slot identity is
  `(character_id, holder_label, secret_id, topic)` and closure happens only when
  `fromSeq !== null && (input.character_id || input.holder_label)`; there is **no**
  `status` filter on the previous rows.
* `backfillSeq` sets `from_seq`/`to_seq` from `moment` for every row of a book, for both
  `fact` and `knowledge` (table names interpolated from a fixed literal list).

### 5.8 `lib/graph.js`

```js
ensureGraph(db, bookId) -> void
loadGraph(db, bookId) -> {nodes, edges}
```

Constants: `COL_W = 320`, `ROW_H = 130`, `TOP = 60`.

* `ensureGraph` is a no-op when the book already has ≥1 `node`. Otherwise, inside no
  explicit transaction: one node per chapter (`x = i*320`, `y = 60`, ordered by `number`),
  a `flow` edge chaining consecutive chapter nodes, one node per event
  (`x = chapterIndex*320 + 20`, `y = 60 + 150 + k*130` where `k` is the event's index within
  its chapter, events ordered by `chapter_id, id`; chapter index `-1` maps to column 0), and
  a `data` edge for every `event_link` whose both endpoints have node ids.
* `loadGraph` calls `ensureGraph` (**a GET mutates the DB**), loads nodes/edges, then for each
  node: `ref_table='chapter'` → `n.entity = chapter row`; `ref_table='event'` → event row
  plus `n.entity.participants = [{id, name}]` from `event_character`; `n.data` is
  `JSON.parse`d when possible, otherwise left as the raw string.

### 5.9 `lib/search.js`

```js
searchBookText(db, bookId, query, limit = 30) -> {chapters, offscreen}
```

* `q = String(query ?? '').trim()`; `q.length` outside `2..150` → `400 'Запрос: от 2 до 150 символов'`.
* `n = max(1, min(100, Number(limit) || 30))`.
* `phrase = '"' + q.replaceAll('"','""') + '"'` — the query is always a **quoted FTS phrase**,
  so FTS operators (`*`, `-`, `OR`, `NEAR`) are inert; internal `"` is doubled.
* `chapters`: `snippet(chapter_search, 2, '[', ']', '…', 22)`, `WHERE cs.book_id=? AND
  chapter_search MATCH ? ORDER BY rank LIMIT ?`. Because `book_id` is UNINDEXED, the book
  filter is applied to matched rows only.
* `offscreen`: `hidden_beat JOIN hidden_line` with
  `hb.offscreen LIKE %q% OR hb.visible_effect LIKE %q%`, ordered by `chapter_no`, same limit.
  Returns `{id, chapter_no, line, offscreen, visible_effect, planned}`.

### 5.10 `lib/activity.js`

```js
logActivity(db, {book_id, tool, summary, detail}) -> void   // detail JSON.stringify'd
listActivity(db, {book_id, limit=50}) -> rows[]             // detail JSON.parse'd, ordered id DESC
```

`book_id` may be `null`. Parsing is done on read, so malformed `detail` throws **on read**
(this is not guarded).

### 5.11 `lib/chatHistory.js`

```js
loadChatHistory(db, bookId, chapterId, limit = 200) -> {messages, has_more}
```

* 404 `'Книга не найдена'`; if `chapterId != null`, 404
  `'Глава не принадлежит книге'`.
* Query: `ai_run WHERE book_id=? AND chapter_id IS ? AND kind IN ('chat','agent')
  ORDER BY id DESC LIMIT limit+1` — `IS ?` gives NULL-safe equality, so a book-wide
  transcript (`chapter_id NULL`) is a distinct conversation from per-chapter ones.
* `has_more` = more than `limit` rows existed. Rows are reversed to ascending order and each
  becomes 1–2 messages: always `{role:'user', content: prompt, run_id}`; then
  `ok` → `{role:'assistant', content: result, actions}`, `error` →
  `{role:'error', content: error||'Запрос завершился с ошибкой', retry: prompt, actions}`,
  `running` → `{role:'error', content:'Ответ ещё формируется или запрос был прерван.
  Обновите переписку позже.', retry: prompt}`. `actions` defaults to `[]` and is parsed
  from `actions_json` with a `try/catch` (legacy rows tolerated).

### 5.12 `lib/scenePacket.js`

```js
buildScenePacket(db, bookId, chapterId) -> {
  book:{id,title}, chapter:{id,number,title,pov,plan,summary}, at,
  participants:[{id,name}], events:[...], world_truth:[...],
  pov_knowledge:{character, knows, suspects, misbeliefs, unaware} | null,
  reader_knowledge:[...], reader_knowledge_tracked: boolean,
  offscreen_for_author:[...], planned_facts:[...], open_promises:[...]
}
```

* 404s: `'Книга не найдена'`, `'Глава не найдена в книге'`.
* `sceneMoment` (private): the moment **immediately before** the chapter's `from_moment`
  (`seq < start.seq ORDER BY seq DESC LIMIT 1`); no `from_moment` →
  `{seq:-1e9, label:'начало книги'}`; no earlier moment → `{seq:start.seq-1, label:'до
  начала главы'}` (**this fallback object has no `id`**).
* participants: distinct `event_character.character_id` over the chapter's events; POV is
  inferred by **case-insensitive substring containment** between `chapter.pov || book.pov`
  and character names (`toLocaleLowerCase('ru-RU')`), and the matched character is appended
  to the participant list.
* `worldState(..., {characters: ids || undefined, events: 15})`.
* `reader_knowledge`: latest row per `topic` at `moment.seq`, via
  `ROW_NUMBER() OVER (PARTITION BY topic ORDER BY COALESCE(from_seq,-1000000000) DESC, id DESC)`,
  filtered to `status='канон'` and `from_seq <= seq`.
* `offscreen_for_author`: `hidden_beat` joined to `hidden_line` for chapter numbers in
  `[max(1, number-2), number]`, ordered `chapter_no, id`, including `truth`.
* `planned_facts`: `fact WHERE status != 'канон' AND from_chapter = chapter.number`.
* `world_truth`: all character facts flattened with a `subject` name, plus `plot`, `world`,
  `locations` buckets.

### 5.13 `lib/aiContext.js`

```js
buildAiContext(db, bookId, chapterId, options = {}) -> string
```

Produces **one string** consumed as a prompt section. 404s as in scenePacket
(`'Глава не принадлежит этой книге'`).

Assembly order (all clips are `String(v ?? '').slice(0, max)`):

1. Header lines: book title, genre, tone (`'не задан'` fallbacks); slice label; the
   instruction not to invent canon.
2. Chapter line: number, title, POV (`chapter.pov || book.pov || 'не задан'`);
   `Синопсис:` (≤2500); `План:` (≤3500).
3. Related graph `plan` nodes: up to 12 nodes adjacent (either direction) to the chapter
   node with `type='plan'`; each contributes `JSON.parse(node.data).text` (≤800) or the raw
   `data` string.
4. Scene packet: POV knowledge line (knows/suspects/misbeliefs, first 18 items) and the
   reader-knowledge line, or the explicit warning that reader awareness is untracked.
5. `Персонажи и их состояние:` — up to 30 characters; per character: `• name: summary`
   (≤280), up to 18 facts as `  [kind/key] value` (≤230 each), up to 10 knowledge entries.
6. `Отношения:` — up to 20 as `from → to: state` (≤150).
7. `Общее [kind]: value` for `plot` + `world` facts, up to 25 (≤250).
8. `Незакрытое обещание: title` up to 15 (≤200).
9. Hidden beats `[max(1, chapter.number-2) .. chapter.number]`, up to 15, with the explicit
   "do not reveal in prose" instruction.
10. Current chapter text fragment, if `file_path` and the file exists: first 10000 chars.
11. Files under `book.root_path`, if present:
    `bible/world_rules.md` ≤3500, `bible/voices.md` ≤3500,
    `styles/<active_style>.md` ≤4500.
12. Up to 3 `style_example` rows: filtered by `scene_type == options.sceneType` **or**
    `'общий'`, falling back to all examples when nothing matches, then ranked
    `sceneType match (3) > 'общий' (2) > other (1)`, tie-broken by `id DESC`; each is
    rendered as `Одобренный автором образец (scene_type; title):` + ≤1600 chars.
13. Final composition:
    `lines.join('\n').slice(0, 22000) + '\n' + docLines.join('\n').slice(0, 10000)`.

The chapter moment used is the one **before** `from_moment`, like the scene packet; for a
book without a chapter, `resolveMoment(db, bookId)`.

### 5.14 `lib/ai.js`

```js
readAiConfig() -> {provider, model, apiKey, profiles}
publicAiConfig() -> {provider, model, has_key, profiles:{name:{model, has_key}}}
saveAiConfig(input) -> publicAiConfig()
completeAiTurn(messages, {timeoutMs=90000, maxTokens=2500, fetcher=fetch, tools, toolChoice='auto'}) -> {text, tool_calls, model, finish_reason, tokens_in, tokens_out}
completeAi(messages, options) -> same, but throws when text is empty
testAiConnection(options) -> completeAi([{role:'user', content:'Ответь одним словом: готово'}], {maxTokens:50, timeoutMs:30000})
```

`PROVIDERS`:

| key | url | defaultModel |
|---|---|---|
| `deepseek` | `https://api.deepseek.com/chat/completions` | `deepseek-flash` |
| `openrouter` | `https://openrouter.ai/api/v1/chat/completions` | `openrouter/free` |

* `readAiConfig` is called on **every** AI operation (no cache). It tolerates a missing file
  (`ENOENT` → defaults) but rethrows other errors. It migrates the legacy single-provider
  shape (`{provider, model, apiKey}`) into `profiles[provider]` without losing the key.
  An unknown saved `provider` falls back to `'deepseek'`.
* `saveAiConfig` validation: provider must be a known key (else
  `'Выберите DeepSeek или OpenRouter'`); `model` non-empty, ≤120 chars, no CR/LF
  (`'Некорректное имя модели'`); `apiKey` ≤500 chars, no CR/LF (`'Некорректный API-ключ'`);
  `clear_key:true` blanks the key; otherwise an omitted `api_key` keeps the previous key
  **for that provider only**. Written atomically to `<path>.tmp` with mode `0o600`
  (+ best-effort `chmodSync`), then `renameSync`. Returns the public view.
  Note `saveAiConfig` throws **plain** `Error` objects (no `status`), so callers must map.
* `completeAiTurn` request body: `{model, messages, max_tokens, stream:false}`; when tools:
  `tools`, `tool_choice`, `parallel_tool_calls:false`; **deepseek only**:
  `thinking:{type:'disabled'}` (comment: reasoning can consume a small `max_tokens` and leave
  `content` empty); openrouter adds header `X-OpenRouter-Title: Story Forge`.
  `Authorization: Bearer <key>`; `AbortSignal.timeout(timeoutMs)`.
* Transport errors map to `'Модель не ответила вовремя'` (TimeoutError) or
  `` `Не удалось подключиться к ${provider}: ${e.message}` ``.
* HTTP errors: detail is `data.error.message || data.message || 'HTTP <status>'`, **with the
  API key replaced by `[скрыто]`** and sliced to 300 chars, prefixed `${provider}: `.
* `content` may be a string or an array of parts (only `type:'text'` parts joined).
* Empty content **and** no tool calls → an error chosen from `finish_reason`:
  `length` → `Лимит ответа исчерпан (N токенов)…`, `content_filter` →
  `Провайдер отфильтровал ответ.`, `insufficient_system_resource` →
  `У провайдера не хватило ресурсов…`, `reasoning_content` present →
  `Модель вернула только рассуждение…`, otherwise
  `Провайдер ${provider} не вернул текст ответа (причина: …)`.
* `tokens_in`/`tokens_out` come from `usage.prompt_tokens`/`usage.completion_tokens`,
  `null` when absent; `model` falls back to the configured model.

### 5.15 `lib/agent.js`

Constants:

```js
HIDDEN_TOOLS = new Set(['list_books', 'run_import'])          // removed from the agent's toolset
MUTATIONS    = new Set(['add_fact','close_fact','add_knowledge','write_chapter_text',
                        'add_promise','update_promise','create_entity','update_entity','delete_entity'])
ACTION_WORDS = /(?:создай|добавь|измени|обнови|удали|запиши|сохрани|отредактируй|напиши|пиши|перепиши|исправь|поправь|замени|заведи|дополни|закрой)/iu
DELETE_WORDS = /(?:удали|сотри|стереть)/iu
CHAPTER_NUMBER = /(?:глав[аыуоеой]*\s*№?\s*(\d+)|(\d+)\s*глав[аыуоеой]*)/iu
WRITE_CHAPTER  = /(?:напиши|пиши|перепиши|допиши|продолжи|сочини)/iu
```

```js
export function inferChapterId(db, bookId, chapterId, message) -> chapterId | null
export async function runAgent(db, {bookId, chapterId, message, history = [], completeTurn = completeAiTurn, maxRounds = 12})
  -> {text, actions, steps, model, finish_reason}          // throws with e.actions / e.steps
```

* `inferChapterId`: if the message names a chapter number (either pattern), look up that
  chapter in the book — found → its id, **not found → `null`** (deliberately not falling
  back). Otherwise validate the supplied `chapterId` belongs to the book (404
  `'Глава не принадлежит книге'`), else `null`.
* `wantsChapterWriting(message)`: `WRITE_CHAPTER` **and** `/глав[аыуоеой]*/i` **and** not
  negated (`не/пока не + write verb`) **and** not `/\bи\s+(?:добавь|обнови|измени|создай|удали)\b/`
  (i.e. "write chapter 5 and add a fact" is *not* treated as pure chapter writing).
  When true, `runAgent` takes the separate `writeRequestedChapter` path.
* `runAgent` general path:
  - 404 `'Книга не найдена'`; `400 'Некорректное задание агенту'` for a bad/oversized message.
  - `tools = makeTools(db).filter(t => !HIDDEN_TOOLS.has(t.name))` (20 tools), definitions in
    OpenAI function format.
  - `wantsAction = ACTION_WORDS.test(message)`.
  - System prompt is a fixed Russian instruction block ending with the book id and the
    `buildAiContext` text; history is the last 8 messages.
  - Loop `round < maxRounds` (12). `writeNow = wantsAction && !actions.length && round >= 6`;
    when `writeNow`, only `MUTATIONS` definitions are offered.
    `toolChoice = wantsAction && (round === 0 || writeNow) ? 'required' : 'auto'`;
    `maxTokens: 8000`, `timeoutMs: 180000`.
  - No tool calls: if `wantsAction && !actions.length` → throw
    `'Агент не выполнил действие. Попробуйте указать конкретную главу или запись и повторить задание.'`;
    otherwise return the text (or `` `Выполнено действий: ${actions.length}` ``).
  - Per tool call, in order: reject calls at index `>= 8` in one response
    (`'Слишком много вызовов инструментов за один шаг'`), unknown tool, JSON parse,
    `validateArgs`, `assertToolScope`, `await handler(args)`. Writes append to `actions`
    (`{tool, label, entity_type, id}`) and log activity entries with `tool.activity`; any
    error becomes `{error}` fed back as a `tool` message.
  - Tool output serialization is truncated to 30000 chars + `'\n[ответ сокращён]'`.
  - Exhausting rounds throws `` `Агент остановлен после ${maxRounds} шагов. Выполнено действий: ${actions.length}.` ``.
* `assertToolScope` (security-relevant, all Russian messages):
  - a `MUTATIONS` tool is only allowed when `ACTION_WORDS` matches the user message
    (`'Автор не поручал изменять книгу в этом сообщении'`);
  - `book_id` in args, if present, must equal `bookId` (`'Инструмент запросил другую книгу'`);
  - `delete_entity` additionally requires `DELETE_WORDS`
    (`'Удаление разрешено только по прямой просьбе автора'`);
  - ownership checks via `owned(table, id)`: `get_character_state` → `character`;
    `get_chapter_text`/`write_chapter_text` → `chapter`; `close_fact` → `fact`;
    `update_promise` → `promise`; `add_knowledge` → `character`;
    `add_fact` with `subject_type==='character'` → `character`;
  - `write_chapter_text` extra: if the chapter already has text, `expected_revision` must
    match, else `'Глава уже содержит текст. Сначала прочитай её через get_chapter_text и
    передай текущую revision в expected_revision.'`; body 1..160000 chars.
* `validateArgs(tool, args)`: hand-rolled subset of JSON Schema — `required` present and
  non-null; unknown keys rejected only when `additionalProperties === false`; type checks for
  `integer` (`Number.isSafeInteger`), `string`, `object` (not array), `array`; `enum`
  membership. `number`/`boolean` are not type-checked.
* `actionLabel`: per-tool Russian label, e.g.
  `` `Глава ${number}: записано ${chars_no_space} знаков` ``,
  `` `Создана глава ${number}` ``, `` `Добавлен факт #${id}` ``.
* `writeRequestedChapter` (the "write this chapter" fast path):
  - resolves/creates the target chapter (`create_entity` with
    `{number, title:'Глава N'}` when the message names a number that does not exist);
    if no number can be derived → `400 'Укажите номер главы или выберите её в списке'`;
  - builds the system prompt including the **current revision** and the last 5000 chars of
    the previous chapter for continuity;
  - offers **only** `write_chapter_text`, `toolChoice:'required'`, `maxTokens:8000`,
    timeout 180 s, up to **4 attempts**;
  - forces `args.chapter_id = target` and `args.expected_revision = current.revision`,
    strips `book_id`, then `validateArgs` + `assertToolScope` + handler;
  - on success returns text `` `Глава N сохранена в книге (X знаков).` ``;
  - on exhaustion throws
    `'Модель не смогла записать главу после четырёх попыток. Последняя ошибка: …'`.

### 5.16 `lib/entityTools.js`

`ENTITY` registry (this is the complete, exact field policy):

| type | `fields` (patchable) | `create` | `required` | `parent` |
|---|---|---|---|---|
| `chapter` | `EDITABLE.chapter` | `number, title, pov, summary, plan` | — | — |
| `character` | `EDITABLE.character` | `EDITABLE.character` | `name` | — |
| `event` | `EDITABLE.event` | `EDITABLE.event` | `title` | — |
| `promise` | `EDITABLE.promise` | `EDITABLE.promise` | `title` | — |
| `relationship` | `from_char` + `EDITABLE.relationship` | same | `from_char, current_state` | — |
| `moment` | `EDITABLE.moment` | `EDITABLE.moment` | `seq, label` | — |
| `location` | `name, kind, where_desc, description, who_here, events_here` | (same) | `name` | — |
| `secret` | `ext_no, title, truth, reader_knows, reveal_plan` | (same) | `title` | — |
| `hidden_line` | `code, name, truth, who_knows, reader_learns, status` | (same) | `name` | — |
| `timeline_entry` | `chapter_no, story_date, description, who, hidden, ord, moment_id` | (same) | `description` | — |
| `node` | `EDITABLE.node` | (same) | `type` | — |
| `edge` | `from_node, from_port, to_node, to_port, kind, label` | (same) | `from_node, to_node` | — |
| `character_state` | `EDITABLE.character_state` | `character_id` + fields | `character_id, text` | `['character','character_id']` |
| `relationship_state` | `chapter_no, to_chapter, from_moment, to_moment, state, note, event_id` | `relationship_id` + fields | `relationship_id, state` | `['relationship','relationship_id']` |
| `hidden_beat` | `chapter_no, offscreen, visible_effect, planned` | `hidden_line_id` + fields | `hidden_line_id, chapter_no` | `['hidden_line','hidden_line_id']` |
| `annotation` | `EDITABLE.annotation` | `chapter_id` + fields | `chapter_id, note` | `['chapter','chapter_id']` |

Note `TYPES` (the enum exposed to the model) is `Object.keys(ENTITY)` — **16 types**;
`arc` is absent from the registry but handled by name in `assertBook`.

```js
entityBook(db, type, id) -> {row, bookId}       // resolves ownership via row.book_id or the parent row
makeEntityTools(db) -> [list_entities, get_entity, get_entity_schema, create_entity, update_entity, delete_entity]
```

* `cleanData(db, bookId, type, data, creation)` rejects unknown fields
  (`` `Поле ${key} нельзя изменять у ${type}` ``), non-scalar values, strings > 120000
  chars, then validates every foreign key named in the `foreign` map
  (`chapter_id→chapter, character_id→character, from_char→character, to_char→character,
  from_node→node, to_node→node, moment_id→moment, from_moment→moment, to_moment→moment,
  hidden_line_id→hidden_line, relationship_id→relationship, arc_id→arc, event_id→event,
  to_char_id→character`) via `assertBook`; booleans are coerced with `Number(value)`.
  On creation, `required` fields must be non-null and non-empty
  (`` `Для ${type} нужно поле ${key}` ``); an empty patch set is rejected
  (`'Нет полей для изменения'`) except for chapter creation.
* `create`: for `chapter`, `number ??= MAX(number)+1`, `title ??= 'Глава N'`; child types get
  their parent FK from `data` and no `book_id`; all others get `{book_id, ...fields}`.
* `update`: resolves ownership (400 `'Запись не принадлежит выбранной книге'`), cleans the
  patch, applies `crud.patch`, re-reads the row.
* `list_entities`: root types → `WHERE book_id=? ORDER BY id LIMIT 100`; child types → join
  through the parent to filter by book.
* `delete_entity`: **refuses chapters** with
  `'Главу удаляйте через редактор после сохранения копии; инструмент агента не удаляет главы'`.
* Every mutating tool carries an `activity(args, result)` function producing
  `{book_id, summary, detail}` for `mcp_activity`.

### 5.17 `lib/mcpCore.js`

```js
makeTools(db) -> tool[]                              // 22 tools
listResources(db) -> resource[]
readResource(db, uri) -> {uri, mimeType:'text/markdown', text}
PROMPTS                                          // [{name:'write_chapter', description, arguments:[]}]
getPrompt(name) -> {description, messages:[{role:'user', content:{type:'text', text}}]}
dispatch(msg, db) -> {hasReply:false} | {hasReply:true, payload}
```

Tool catalog (name → required args → behaviour):

| Tool | Required | Notes |
|---|---|---|
| `list_books` | — | `id, slug, title, root_path, active_style` |
| `list_characters` | `book_id` | `id, name, is_main, status` |
| `get_state` | `book_id` | optional `chapter, characters[], kinds[], events, min_weight` → `worldState` |
| `get_character_state` | `character_id` | optional `chapter`; resolves the book from the character |
| `search_facts` | `book_id, q` | optional `characters, kinds, limit` |
| `diff_state` | `book_id, from_chapter, to_chapter` | → `diff` |
| `get_dictionaries` | — | `{fact_kinds: kindTree, stances}` |
| `add_fact` | `book_id, kind, value` | optional `subject_type, subject_id, subject_name, key, weight, from_chapter, note`; `source:'ИИ (MCP)'` |
| `close_fact` | `fact_id, chapter` | sets `to_moment`/`to_chapter` |
| `add_knowledge` | `book_id, topic, stance` | optional `character_id, holder_label, belief, from_chapter, note` |
| `get_chapter_text` | — | `chapter_id` or `book_id+number`; returns `{id, number, title, status, chars_no_space, revision, body}` |
| `write_chapter_text` | `chapter_id, body` | optional `reason` (default `'правка ИИ через MCP'`), `expected_revision`; adds `min_chars_note` when below `book.min_chars` |
| `list_promises` | `book_id` | |
| `add_promise` | `book_id, title` | optional `planted_ch, expected_payoff`; status `'ожидает'` |
| `update_promise` | `promise_id` | optional `status` (enum `ожидает/выстрелило/отменено`), `fired_ch` |
| `run_import` | — | `importAll(join(APP_ROOT,'..'))`, returns `[{book_id, title, stats, warnings}]` |
| + 6 from `makeEntityTools` | | `list_entities, get_entity, get_entity_schema, create_entity, update_entity, delete_entity` |

All tool errors are thrown `Error` objects, sometimes tagged `isToolError = true` by the local
`err()` helper (the flag is only used for `err()`-produced messages such as
`'Глава не найдена'`).

**Resources** — `DOC_FILES` (only listed when the file exists; URI
`storyforge://<slug>/<key>`):

| key | relative path | title (Russian, as served) |
|---|---|---|
| `book` | `book.md` | Паспорт книги — жанр, тон, POV, жёсткий минимум знаков, запреты |
| `style` | `styles/<book.active_style>.md` | Профиль стиля — метрики, приёмы-маркеры (тропы стиля), запреты, калибровочный образец |
| `voices` | `bible/voices.md` | Живые голоса персонажей — манера речи, форумные привычки |
| `world_rules` | `bible/world_rules.md` | Правила мира — 🔒 неизменяемое, нарушать нельзя |
| `glossary` | `bible/glossary.md` | Глоссарий — единственно верные написания имён и терминов |
| `direction` | `plot/direction.md` | Якорные точки сюжета — куда движется история |
| `hidden` | `plot/hidden.md` | Скрытые линии — что готовится за кадром, расписание намёков |
| `arcs` | `plot/arcs.md` | Арки персонажей — на какой они сейчас стадии |
| `promises` | `plot/promises.md` | Ружья текстом — то же, что list_promises, но с пояснениями |

`readResource` URI regex: `^storyforge:\/\/([^/]+)\/([^/]+)$`; errors
`'Некорректный URI ресурса, ожидается storyforge://<slug>/<ключ>'`,
`'Книга не найдена: <slug>'`, `'Неизвестный ресурс: <key>'`, `'Файл не найден: <rel>'`.

**Prompt** `write_chapter`: a single `user` message with a 5-step methodology (context → plan
→ text → check → DB update) and an explicit warning not to hand-edit generated markdown.

**`dispatch` (JSON-RPC 2.0)**:

| Method | Result |
|---|---|
| `initialize` | `{protocolVersion: params.protocolVersion \|\| '2025-11-25', capabilities:{tools:{},resources:{},prompts:{}}, serverInfo:{name:'storyforge', version:'1.1.0'}, instructions: '…'}` |
| `notifications/initialized`, `notifications/cancelled`, `method === undefined` | `{hasReply:false}` |
| `ping` | `{}` |
| `tools/list` | `{tools:[{name, description, inputSchema}]}` |
| `tools/call` | `{content:[{type:'text', text: JSON.stringify(result, null, 2)}]}`; unknown tool → JSON-RPC error `-32602` `Неизвестный инструмент: X`; **tool throw** → success result `{content:[{type:'text', text:'Ошибка: '+msg}], isError:true}` |
| `resources/list` | `{resources}`; internal throw → `-32603` |
| `resources/read` | `{contents:[{uri, mimeType, text}]}`; throw → `-32602` |
| `prompts/list` | `{prompts: PROMPTS}` |
| `prompts/get` | `getPrompt(name)`; throw → `-32602` |
| any other method **with** `id` | `-32601` `Неизвестный метод: X` |
| any other method **without** `id` | `{hasReply:false}` |

`hasReply` is simply `id !== undefined`, so a request without `id` is treated as a
notification even when it names a real method.

Activity logging happens inside `tools/call` **after** the handler returns, wrapped in its own
`try/catch` (a logging failure never changes the tool result).

### 5.18 `lib/proposals.js`

```js
parseProposals(text) -> proposal[]                 // max 100, tags type
validateProposals(db, bookId, chapterId, proposals) -> {accepted, rejected, revision, chapter}
commitProposals(db, bookId, chapterId, expectedRevision, proposals) -> {inserted}
```

* `parseProposals` strips an outer ```` ```json ````/```` ``` ```` fence, `JSON.parse`s
  (`502 'Модель вернула некорректный JSON'`), requires an object
  (`502 'Модель не вернула список фактов'`), then concatenates
  `data.facts` → `type:'fact'`, `data.knowledge` → `'knowledge'`, `data.reader` → `'reader'`,
  and slices to 100.
* `validateProposals` rules, in order (first failure wins, one reason per proposal):
  - `TYPES` must contain `p.type` → `'неизвестный тип'`;
  - `source_quote` must be a string of length 4..1000 **and** `body.includes(source_quote)`
    → `'цитата-источник не найдена дословно в главе'`;
  - `fact`: `kind` in `fact_kind` (`'неизвестный вид факта'`); `value` a non-empty string
    ≤500 (`'некорректный факт'`); a `character` subject must exist in **this** book
    (`'персонаж не из этой книги'`); `subject_type` in
    `['character','plot','world','location','item']` (`'неизвестный субъект'`); for slotted
    kinds, two different values for the same `[subject_type, subject_id, kind, key]` in one
    batch → the second gets `'два разных значения одного слота в одном моменте'`;
  - `knowledge`: `character_id` in book (`'персонаж не из этой книги'`), `stance` in
    `{знает, подозревает, заблуждается, не знает}` (`'неизвестная позиция знания'`), `topic`
    non-empty ≤300 (`'некорректная тема'`);
  - `reader`: `stance`, `topic` as above with `'неизвестная позиция читателя'`.
  Also `404 'Глава не принадлежит книге'` and `400 'Некорректный список фактов'` when the
  array is bad or >100.
* `commitProposals`: validates again, requires `checked.revision === expectedRevision`
  (else `409 'Текст главы изменился после извлечения фактов. Повторите извлечение.'`),
  refuses if **any** proposal was rejected (`400 'Часть выбранных фактов не прошла проверку'`),
  then in `BEGIN IMMEDIATE`:
  - `moment = ch.to_moment || ch.from_moment || null`, `from_chapter = ch.number`;
  - dedupe: an existing `fact` with the same `source_chapter_id, subject_type, subject_id,
    kind, value` is skipped; same for `knowledge` on `character_id, topic, stance` and
    `reader_knowledge` on `topic, stance`;
  - `fact`/`knowledge` go through `addFact`/`addKnowledge` with
    `source:'ИИ: принято автором'`, `source_chapter_id`, `source_quote`;
  - `reader` is inserted directly into `reader_knowledge`
    `(book_id, topic, stance, belief, from_moment, source_chapter_id, source_quote, source)`;
  - returns `{inserted: n}` (count of rows actually created, not of proposals).
  Any throw → `ROLLBACK` and rethrow.

---

## 6. Non-`lib` server modules

### 6.1 `server/db/index.js`

| Export | Semantics |
|---|---|
| `APP_ROOT` | `join(__dirname,'..','..')` |
| `dbPath()` | `SF_DB` or `<APP_ROOT>/data/story.db` |
| `getDb()` | Lazy singleton `DatabaseSync`; checks Node ≥ 22.5; `mkdirSync(dirname,{recursive:true})`; executes `schema.sql`; runs `migrate()`; FTS bootstrap |
| `closeDb()` | `close()` and clear the singleton (used by CLI tools and tests) |

No `PRAGMA busy_timeout`, no `synchronous` tuning, no connection pool. WAL mode is set by the
schema file itself.

### 6.2 `server/start.js`

1. Node version check → exit 1 with a Russian message.
2. If `dbPath()` does **not exist** → `importAll(resolve(APP_ROOT,'..'))`; zero results →
   exit 1 with `'Рядом со StoryForge не найдено ни одной папки книги…'`.
3. `await import('./index.js')` — the server module has top-level `await app.listen(PORT)`.
4. `EADDRINUSE` → a specific Russian hint; other errors → `'Сервер не запустился: …'`.
5. Unless `SF_NO_BROWSER` is set, spawns `cmd /c start '' <url>` (Windows) / `open` / `xdg-open`,
   detached and unref'd; failures ignored.

### 6.3 `server/mcp.js` (stdio transport)

* `getDb()` once; `process.stdin` set to utf8; a rolling `buf` split on `'\n'`.
* Blank lines skipped; unparseable JSON lines **silently skipped**.
* `handle(msg)` is called **without `await`** — messages are processed concurrently as their
  promise chains interleave, so stdout responses may be emitted **out of order**. In practice
  tool handlers are synchronous aside from `run_import`, so ordering is usually preserved, but
  nothing guarantees it.
* `send` writes `JSON.stringify(msg) + '\n'`; nothing else may go to stdout (all human output
  must use stderr).
* `stdin` `end` → `exit(0)`; `SIGTERM` → `exit(0)`.

### 6.4 `server/import/importBook.js` — 18-stage rebuild

`importBook(bookDir, opts = {}) -> {bookId, title, stats, warnings}`

Whole body runs inside `db.exec('BEGIN IMMEDIATE')` … `COMMIT`, with `ROLLBACK` on any throw
(this is what the "failed reimport rolls back" test asserts).

1. **Book (source stage 1)** — read `app/data.json` (`meta.meta.title`, `meta.arcs`,
   `meta.chars`, `meta.chapters`, `meta.events`); parse `book.md` via
   `md.sections`/`md.labelled` for genre, POV, tone, style profile, premise and `min_chars`
   from `≥ N знак`. A malformed `data.json` produces a warning and is treated as `{}`.
   Then `captureUserData(db, slug)` (before any destructive step), then
   `DELETE FROM chapter_search WHERE book_id=?` (only if the book already existed), then
   `DELETE FROM book WHERE slug=?` (cascades everything), then
   `INSERT INTO book (slug,title,root_path,genre,pov,tone,active_style,min_chars,
   target_chars,premise)`.
2. **Arcs** — from `meta.arcs` (`{id,name,color}`), `ord` = array index; map `ext_id → id`.
3. **Characters (registry)** — from `meta.chars` (`{n, main, atm, atmLvl}`); `name` is
   `cleanName(c.n)` (strips a trailing parenthetical), `aliases` holds the raw name when it
   differs; builds a name index that answers both raw and cleaned names.
4. **Character cards** — `bible/characters/*.md` (skipping files starting with `_`):
   match by `h1` or `frontmatter['имя']`; create the character when missing (warning
   `'Персонаж «X» есть в карточке, но отсутствует в data.json'`). Extracts `Характер`
   (`Текущий`, `История изменений`, `Прозвище`), `Атмосфера` (`Описание`, `Влияние` via
   `levelOutOf5`), `Голос (речь)` bullets **split** into `voice` vs `voice_rules` by
   `IS_RULE = /никогда не скажет|⚠️|запрещ|нельзя/i`, `Арка`, and stores `Внешность`,
   `Цели`, `Взгляды и убеждения`, `Знает / Не знает`, plus a concatenation of `Прочее`,
   `Прозвище: …`, and `Арка`. The `UPDATE` uses `COALESCE` so `data.json` values survive.
   `Ключевые события` bullets → `character_state` rows (`kind='event'`, `chapter_no` from
   `md.chapterNo`, `hidden` from `md.isHidden`). `Отношения` tables are deferred.
5. **Relationships** — for each deferred row, `С кем` is split on `/,\s*/` (one row may name
   several targets); a resolved character becomes `to_char`, otherwise the raw name lands in
   `to_label`. `INSERT OR IGNORE`, `is_mutual=1`.
6. **Chapters** — from `meta.chapters` (`id, n, title, arc`), reading
   `chapters/chNNN.md` when present: `words`/`chars_no_space` recomputed, `status='написано'`,
   `title` taken from the H1 with a leading `Глава N. ` stripped. Then any `chNNN.md` not in
   `data.json` is added (warning). `summaries/chNNN.md` → `chapter.summary`.
7. **Events** — from `meta.events` (`id, ch, chEnd, t, s, status, hidden, w, arc`), then
   participants (`chars` with `cstate` snapshots), `links` (→ `event_link`), and `hints`
   (→ `hint` rows, initially `hidden_line_id NULL`). Unknown references produce warnings.
8. **Hidden lines** — `plot/hidden.md`: sections whose title matches `^H\d+\s*—`; the code
   is before the em dash, the name after. Fields grabbed with `^LABEL:\s*(.+)$` (`ПРАВДА`,
   `КТО ЗАМЕШАН`, `ЧИТАТЕЛЬ УЗНАЁТ`, `СТАТУС`). Sub-tables
   `Хронология за кадром` → `hidden_beat` (`planned` when `Когда` matches
   `/план|не финализ/i`), `Посеянные намёки` → `hint`.
9. **Secrets** — `state/secrets.md`, table `Тайны и заблуждения`; columns `#`, `Что`,
   `Читатель знает?`, `Раскроется`, `Кто знает правду` (stance `знает`),
   `Кто заблуждается / не знает` (stance `не знает`). Name lists are stripped of
   parentheticals **before** splitting on `/[,;]|\sи\s/` and leading
   `только|все|весь`, then resolved to characters or kept as `label`.
10. **Promises** — `plot/promises.md` table; `Что заявлено` is mandatory; `fired_ch` parsed
    from the status cell when it contains `выстрелило`.
11. **World rules** — `bible/world_rules.md` sections → bullets → `world_rule`
    (`locked` when the bullet contains 🔒, `ord` = bullet index).
12. **Glossary** — `bible/glossary.md` sections → tables; the **first column** is the term,
    the remaining columns are joined with `' · '` into `meaning`.
13. **Locations** — `bible/locations/*.md`: `frontmatter.название` (else H1, else filename),
    `тип`, `где`, and sections `Описание`, `Кто здесь`, `События здесь`.
14. **Timeline** — `timeline/events.md` sections → tables; `Глава` is either a bare number or
    `гл.N`; `Когда`/`Когда (дата мира)` → `story_date`; `hidden` from `md.isHidden(desc)`.
15. **Current state** — `state/current.md`: `Последняя глава`, `Дата истории`, and sections
    `Где кто находится`, `Активные конфликты`, `Открытые вопросы сцены`.
16. **Docs** — `book.md` (`kind='book'`), `plot/direction.md`, `plot/arcs.md`,
    `bible/factions.md`, any `plot/*arc\d*` file (`kind='arc_plan'`), every `styles/*.md`
    (`kind='style'`); stored with `rel_path` backslashes normalized to `/`.
17. **Timeline & facts** — `buildTimeline`, then `Object.assign(stats, migrateFacts(...))`.
18. **Restore** — `restoreUserData`, `indexBookChapters`, warning when some rows could not be
    reattached.

`stats` keys (exact, in production order): `arcs, characters, character_states,
relationships, chapters, summaries, events, event_characters, event_links, hidden_lines,
hidden_beats, hints, secrets, secret_knowledge, promises, world_rules, glossary, locations,
timeline, docs, moments, facts, knowledge, relationships` — **`relationships` is overwritten**
by `migrateFacts` (which counts `relationship_state` rows), and a Russian key
`восстановлено` may be added.

### 6.5 `server/import/preserve.js`

`captureUserData(db, slug) -> snapshot | null` (null when the book does not exist yet) keeps:

* `facts` joined to `character` for `subject_name_ref`, filtered `f.source <> 'импорт'`;
* `knowledge` joined for `holder_name_ref` and `secret_title_ref`, same filter;
* all `annotations` and `chapter_version` rows of the book, each with `chapter_no`;
* `nodes` with `chapter_no` (via `ref_table='chapter'`) and `event_ext` (via `ref_table='event'`);
* all `edges`;
* `relationship_state` rows **with `rs.note IS NULL`** plus `from_name`/`to_name`;
* all `mcp_activity` rows.

`restoreUserData(db, bookId, snap) -> {restored, skipped}` remaps by stable business keys:

| captured | remapped via |
|---|---|
| fact subject | `character.name` map (falls back to the raw `subject_id`) |
| knowledge holder | `character.name` |
| knowledge secret | `secret.title` |
| annotation/version chapter | `chapter.number` |
| node ref (chapter) | `chapter.number` |
| node ref (event) | `event.ext_id` |
| edge endpoints | the old→new node id map |
| activity | copied verbatim under the new `book_id` |

Anything unmappable is counted in `skipped`. **Captured `relStates` are never restored** — an
asymmetry in the current implementation.

### 6.6 `server/import/timeline.js`

`buildTimeline(db, bookId) -> {moments}`:

* Iterates `timeline_entry` ordered by `ord`; one `moment` per **unique label**
  (`story_date` trimmed, else `` `гл.${chapter_no ?? '?'}` ``), assigning `seq += 100`
  each time and `world = worldOf(label)` (`'мир книги'` when the label matches
  `/гринтаун|мир книги/i`, else `'реальный мир'`); each entry gets its `moment_id`.
* If there were **no** timeline entries: one moment per chapter (`Глава N`, `world NULL`,
  sequential `seq`), and each chapter's `from_moment = to_moment = that moment`;
  returns immediately.
* Otherwise each chapter's span is `MIN(moment.seq) … MAX(moment.seq)` over its timeline
  entries; chapters with no entries get an appended `Глава N (план)` moment at the end.
* Finally every event with a `chapter_id` gets `moment_id = chapter.from_moment`.

`migrateFacts(db, bookId) -> {facts, knowledge, relationships}`:

* `character_state` → `fact` (`subject_type='character'`, kind guessed by `guessKind`:
  `/одет|футболк|наряд|внешност|волос/`→`внешность`; `/погиб|умер|ранен|шок|пропал|жив/`→`статус`;
  `/поселил|живёт|находится|приходит в себя|оказыва/`→`место`;
  `/характер|цинизм|трещина|меняется/`→`черта`; else `событие`), `source='импорт'`,
  `status='канон'`; a leading `гл.N: ` is stripped from the value.
* `secret_knowledge` → `knowledge` with `chapter_no` from `reveal_plan` (default 1).
* Each `relationship` → one `relationship_state` row (chapter from `history`, default 1).
* `current_state.open_questions` bullets → `fact` rows with `kind='обязательство'`,
  `subject_type='plot'`, `note='из среза «сейчас»'`.
* `backfillSeq(db, bookId)` at the end.

### 6.7 `server/import/importAll.js`

`findBooks(root) -> string[]` — immediate subdirectories containing `book.md`
(stat errors swallowed). `importAll(root)` — sequential, `console.log` per book, and
**rethrows** on the first failure after printing `✗`. Despite the `async` signature the work
is synchronous.

### 6.8 `server/import/cli.js`, `report.js`, `tools/*`

* `cli.js`: `getDb()`; one argument → import that folder (must contain `book.md`, else exit 1);
  no argument → `importAll(resolve(APP_ROOT,'..'))`; prints stats and up to 20 warnings;
  `closeDb()` in `finally`.
* `report.js`: read-only integrity report per book (chapters below `min_chars`, missing
  summaries, characters without summary/voice, orphan events, events without participants,
  relationships with `to_char IS NULL`, `character_state` rows pointing at non-existent
  chapter numbers, pending promises, hidden-line beat/hint counts).
* `tools/export.js`: writes **files** (see §8.3).
* `tools/evaluate.js`: reads a JSON array of `{id, book_slug, chapter_no, question, scene_type?}`,
  builds `buildAiContext` per case, optionally calls the model (`--run`), writes
  `<input>.check.json` or `<input>.results.json` next to the input.
* `tools/scaletest.js`: creates a throwaway book (`slug='scale-<timestamp>'`, `root_path='/tmp/scale'`),
  `CHAPTERS` (default 1000) chapters/moments, `CHARACTERS` (default 200) characters, and
  ~3 slotted + 1 accumulating fact per chapter, then times `worldState`/`characterState`/
  `searchFacts`/`diff` and deletes the book at the end. Env: `SF_TEST_CHAPTERS`,
  `SF_TEST_CHARS`.

---

## 7. Concurrency, state, side effects

### 7.1 Process and database state

* One Node process, one event loop, **one `DatabaseSync`** (`db/index.js:17` `_db`), shared by
  every route, both MCP transports and all CLI tools in that process.
* All SQLite calls are **synchronous** and block the event loop. Consequence: request
  handlers are effectively serialized at the DB boundary, and there are **no racing writes**
  — which is why revision checks are implemented as plain read-then-write without locks.
* No cache layer anywhere: every query hits SQLite; `readAiConfig()` re-reads the JSON file on
  every call; `kindTree`/`expandedKinds` always query.
* No `busy_timeout`; a second process opening the same DB (e.g. `npm run mcp` while the
  server runs) can hit `SQLITE_BUSY` immediately on writes. In practice the WAL file
  (`data/story.db-wal`) is present and shared.
* Transactions: `BEGIN IMMEDIATE` in `saveChapterText`, `importBook`, `commitProposals`;
  `BEGIN` (deferred) only in `scaletest.js`. **No nested transactions** are supported by this
  code; `importBook`'s transaction must therefore not call `saveChapterText` (it currently
  doesn't).
* Multi-statement routes that are *not* transactional: `addFact` (close previous + insert),
  `POST /api/books/:id/nodes` (create entity + node),
  `POST /api/books/:id/edges` (insert edge + `event_link`),
  `DELETE /api/edges/:id` (delete `event_link` + edge),
  `POST /api/chapters/:id/apply-edits` (save + mark annotations),
  `POST /api/facts/:id/close`. A crash between statements leaves partial state.
* `ai_run` rows are inserted with status `'running'` before an `await` on the network and
  updated after it. While awaiting, the event loop is free, so other requests can run; no
  transaction is open across the await.

### 7.2 In-memory state

Only module-level singletons: `_db` (db), `app`/`routes` (http), `crud`. Nothing per-user,
no sessions, no request context, no memoization.

### 7.3 File side effects

| Path | Written by | When |
|---|---|---|
| `<APP_ROOT>/data/story.db` (+ `-wal`, `-shm`) | `getDb` / all writes | continuously |
| `<APP_ROOT>/data/ai-config.json` | `saveAiConfig` | `PUT /api/ai/settings` (atomic, mode 0600) |
| `<book.root_path>/<chapter.file_path>` or `…/chapters/chNNN.md` | `saveChapterText` → `atomicWrite` | `PUT /api/chapters/:id/text`, restore, `apply-edits`, `write_chapter_text` |
| `<book.root_path>/bible/characters/<name>.md` | `tools/export.js` | `npm run export` (overwrites) |
| `<book.root_path>/state/current.md` | `tools/export.js` | `npm run export` |
| `<book.root_path>/timeline/events.md` | `tools/export.js` | `npm run export` |
| `<input>.check.json` / `.results.json` | `tools/evaluate.js` | `npm run evaluate` |
| `.data/.fuse_hidden*` artifacts | OS/editor, not the app | — |

Read side: `importBook` (all bible markdown, `app/data.json`, `summaries/`, `timeline/`,
`state/`, `styles/`), `readChapterText`/`R78`/`apply-edits` (chapter prose),
`buildAiContext` (`bible/world_rules.md`, `bible/voices.md`, `styles/<style>.md`, chapter
file), `mcpCore` resources (9 doc paths).

The chapter file is the **source of truth for prose**; `chapter.words`,
`chars_no_space`, `status`, `file_path` and the FTS row are derived caches updated on save.
Deleting a chapter (`R61`) leaves the markdown file on disk; `delete_entity` refuses chapters
for the same reason.

### 7.4 Derived data that must stay in sync

1. `chapter_search` — updated on `saveChapterText`, rebuilt by `indexBookChapters`, purged by
   `DELETE FROM chapter_search WHERE book_id=?` before book deletion.
2. `chapter.words/chars_no_space/status`.
3. `fact.from_seq/to_seq`, `knowledge.from_seq/to_seq`, `reader_knowledge.from_seq` — triggers.
4. `event.moment_id` — only written by `buildTimeline` (and `EDITABLE.event` omits it).
5. `event_link` ⇄ `edge` — mirrored by R72/R73 only; direct table edits desynchronize them.
6. `node.data` — stored as text, parsed on read; PATCH stringifies objects, POST does too.
7. `mcp_activity` — appended by MCP write tools and agent write tools.

---

## 8. What a Rust port must not get wrong

### 8.1 HTTP contract

* **H1.** All 85 routes, exact strings, exact methods. Matching is by equal segment count and
  first-registration-wins; `:param`s are percent-decoded. Query params are all strings and
  duplicate keys keep the **last** value.
* **H2.** Response envelope rules: `fail()` → `{__status, data:{error}}`;
  `raw()` → status/body/headers verbatim and **empty body** when `body` is null; other values
  → `200` + `JSON.stringify(value ?? null)`. A handler returning `undefined` must produce the
  four bytes `null`.
* **H3.** Status code set is exactly `{200, 202, 400, 403, 404, 405, 409, 500, 502}`. Do not
  invent `401/422/429`.
* **H4.** Error bodies are `{"error": "<Russian message>"}`; several messages are asserted by
  tests and displayed by the UI. Preserve them verbatim (see the per-route notes).
* **H5.** Any thrown error becomes `e.status || 500`; `>=500` is also logged. Routes that
  catch locally and remap (R02 → 400, R05/R07/R09/R10 → 502/`e.status`, R06 → 200 with an
  `error` field) must keep those mappings.
* **H6.** PATCH/GET-single routes return `null` with status 200 for a missing row; only
  explicitly coded routes return 404. Do not "improve" this.
* **H7.** `DELETE /api/nodes/:id?withEntity=1` interpolates `node.ref_table` (a
  **user-writable** column, via `POST /api/books/:id/nodes` `ref_table` and `EDITABLE.node`)
  as a SQL table name. That is an identifier-injection surface in the current code. A Rust
  port should whitelist it; if it must be bug-compatible, document the exposure.
* **H8.** A malformed percent escape in a path segment throws in `decodeURIComponent` outside
  the per-route `try` and can terminate the process. A Rust port should return 400 instead,
  but must not silently change route matching for well-formed paths.
* **H9.** Bodies are JSON-only, unlimited in size, and `GET` never reads a body.
  `Content-Type` is ignored.
* **H10.** Static file serving: GET-only, after all routes, `/` → `index.html`, traversal
  guard on the resolved path, `Cache-Control: no-cache`, fixed MIME map, 404 for directories.
* **H11.** `/mcp`: `Origin` allowlist (absent allowed; only `localhost`/`127.0.0.1`
  hostnames), `202` + empty body for notifications and missing bodies, `200` + JSON-RPC
  payload otherwise, `405` for GET/DELETE with those exact messages.

### 8.2 SQLite and schema

* **H12.** `PRAGMA journal_mode=WAL` and `PRAGMA foreign_keys=ON` — the latter is
  per-connection; FK enforcement silently disappears without it. `relationship_state.event_id`
  references a table declared later in the file, so schema validation must be deferred to DML
  time (or reordered).
* **H13.** The 8 triggers are the only mechanism maintaining `from_seq`/`to_seq`
  (including the `moment_seq_upd` cascade across `fact`, `knowledge`,
  `reader_knowledge`). Any replacement must reproduce them exactly, including the `NULL`
  result when `to_moment` is NULL.
* **H14.** NULL semantics in slot matching use `(col IS ? OR col = ?)`; plain `= ?` breaks
  every NULL-keyed slot.
* **H15.** `INSERT OR IGNORE`/`INSERT OR REPLACE` usage is deliberate: relationships and
  glossary/location/hint seeds rely on it; `event_character` relies on `REPLACE` (R65);
  `event_link` relies on `IGNORE` (R72). Switching to plain `INSERT` turns idempotent
  re-imports into constraint errors.
* **H16.** UNIQUE constraints involving NULL are not enforced by SQLite
  (`relationship(book_id,from_char,to_char,to_label)`, `event(book_id,ext_id)`,
  `arc(book_id,ext_id)`, `hidden_line(book_id,code)`, `secret_knowledge` has no key at all).
  A Rust port that models these as strict unique keys will reject data the JS accepts.
* **H17.** FTS5 specifics: `tokenize='unicode61'`, two UNINDEXED payload columns, the hidden
  MATCH column named after the table, `snippet(chapter_search, 2, '[', ']', '…', 22)`, and
  `ORDER BY rank`. Query text is always wrapped as a quoted phrase with `"` doubled.
* **H18.** `LIKE` in `searchFacts` and in the offscreen-beat search is ASCII-case-insensitive
  only; Cyrillic matching is effectively case-sensitive. Preserve that, or the result sets
  change.
* **H19.** `migrate()`'s conditional `ALTER TABLE` list must be reproduced (or the DB
  pre-upgraded) so existing `data/story.db` files keep working; `CREATE TABLE IF NOT EXISTS`
  alone will not add the columns.
* **H20.** The FTS bootstrap gate is a **global** `COUNT(*)` over `chapter_search`, not
  per-book.

### 8.3 Text, files, revisions

* **H21.** `revisionOf` = lowercase hex SHA-256 of the UTF-8 body. The revision is a public
  API value (returned by R22/R23/R26/R78/R79/R07 and required by R08/R23/R79 and
  `write_chapter_text`) — the algorithm is part of the contract.
* **H22.** `charsNoSpace` counts **UTF-16 code units** of the concatenated non-whitespace
  text after removing heading markers, `**`, and `---`. For astral characters (emoji) this
  differs from a Unicode scalar count; `chars_no_space` feeds `min_chars` checks and reports,
  so parity requires UTF-16 semantics.
* **H23.** `wordCount` = number of non-empty `\s+`-separated tokens after the same cleanup.
* **H24.** Ordering inside `saveChapterText`: read → revision check → (unchanged shortcut) →
  `BEGIN IMMEDIATE` → insert **previous** body as a version (only if the file exists) →
  atomic write → update row (forcing `status='написано'`) → replace FTS row → `COMMIT`;
  on failure roll back and restore the file. The `unchanged:true` shortcut writes neither a
  version nor FTS and must stay.
* **H25.** Atomic write is temp-file + rename in the **same directory**, creating parent
  directories, with temp cleanup on failure.
* **H26.** Path handling: `chapter.file_path` is stored with `/` separators (backslashes
  normalized on save), the default name is `chapters/chNNN.md` with a 3-digit zero pad, and
  absolute or escaping paths are rejected with 400.
* **H27.** `missing:true` means "`file_path` is set but the file is gone"; a chapter with no
  `file_path` at all is *not* missing and has an empty body.
* **H28.** `apply-edits` requires `expected_revision` for non-dry-run calls (omitting it
  yields 409), builds `notesById` from **all** annotations (not only open ones), rejects
  ambiguous fragments rather than guessing, rejects overlapping edits, applies right-to-left,
  and only then marks notes `применена`.
* **H29.** `planEdits` message strings and the exact rejection conditions matter to the UI;
  the ambiguity-resolution ladder (exact → `note.prefix + old` → `locateNote` ±400 chars) is
  behaviour, not an implementation detail.
* **H30.** `locateNote`'s attempt ladder and `how` labels (`позиция`, `контекст`, `префикс`,
  `цитата`, `цитата (с допуском)`) are surfaced to the client as `matched_by`.
* **H31.** Markdown parsing quirks: `frontmatter` requires LF; `bullets` merges continuation
  lines and closes on blank lines; `sections` bounds a section by the next heading of equal or
  shallower level; `table` skips separator rows; `labelled` accepts `**Label:**`,
  `**Label**:` and bare `- Label:` and treats `label` as a regex. `isHidden`/`chapterNo`/
  `levelOutOf5` are regex heuristics whose exact patterns are observable in imported data.

### 8.4 Domain semantics

* **H32.** Slots: closing requires `slotted(kind) && from_seq !== null`; it only affects rows
  with `status='канон'`, `from_seq < new from_seq`, and an open-or-later `to_seq`; it happens
  **outside** a transaction together with the insert. Knowledge closing has **no** `status`
  filter and additionally requires a holder (`character_id` or `holder_label`).
* **H33.** An unknown `kind` silently becomes `'событие'` and an unknown `stance` becomes
  `'знает'` (R45/R50 and MCP tools). Validation happens only in the proposal pipeline and in
  `EDITABLE`-based patches (where an unknown kind would hit the FK and fail).
* **H34.** Slice queries always filter `status='канон'` for facts; `knowledgeAt` does not.
  Events additionally require `kind='событие'`. `worldState` only includes events when the
  kinds filter is absent or mentions `'событие'`, and never when `full=true`.
* **H35.** `worldState` default `events` limit is 30; `characterState` 20; scene packet 15;
  `buildAiContext` 12. `full=1` means "all non-event facts, no events".
* **H36.** `resolveMoment` fallback chain (id → chapter.to_moment → last written chapter's
  to_moment → last moment → `{id:null, seq:0, label:'начало'}`) is the definition of "now";
  getting this order wrong changes every default slice.
* **H37.** Interval comparison is half-open: `from_seq <= seq < to_seq` (via
  `to_seq IS NULL OR to_seq > seq`), with `from_seq` coalesced to `-1e9`. A fact written at
  the same moment as another **closes** it (the new `from_seq` equals the old `to_seq`).
* **H38.** `diff` normalizes direction (`a.seq <= b.seq`), uses strict `>` on the lower bound
  and `<=` on the upper, applies `LIMIT 200` per bucket and no status/hidden filtering.
* **H39.** `reader_knowledge` "current" per topic is the greatest `COALESCE(from_seq,-1e9)`
  then greatest `id` — not "the merely latest row".
* **H40.** POV resolution is `chapter.pov || book.pov` matched by **substring containment**
  against character names in `ru-RU` lowercase; no exact-name match, no ambiguity handling.

### 8.5 AI, agent, MCP

* **H41.** Prompt/context budgets: `buildAiContext` = 22000 chars of narrative lines +
  `'\n'` + 10000 chars of document lines; per-field clips (2500 summary, 3500 plan, 800 plan
  node, 280 summary, 230 fact, 150 relationship, 250 plot/world, 200 promise, 180 belief,
  10000 chapter fragment, 3500/3500/4500 docs, 1600 style example). Chapter text slices in
  extract/suggest are 60000 chars.
* **H42.** Every AI route persists `ai_run` rows (`kind`, `engine='api'`, `model`,
  `prompt`, `result`, `actions_json`, `status`, `error`, tokens, `finished_at`) — the chat
  transcript is *derived* from this table, so skipping it breaks R04.
* **H43.** `chapter_id IS ?` (NULL-safe) separates book-wide from per-chapter conversations,
  and rows are returned oldest-first after being read newest-first.
* **H44.** Provider plumbing: `thinking:{type:'disabled'}` **only** for deepseek;
  `parallel_tool_calls:false` whenever tools are sent; `X-OpenRouter-Title` header for
  openrouter; API-key redaction (`[скрыто]`) in error text; the `finish_reason` → message
  mapping; `max_tokens`/`stream:false` body shape.
* **H45.** `saveAiConfig` key retention is per-provider: switching provider must **not**
  reuse the previous provider's key, and an omitted `api_key` must keep that provider's
  existing key. The legacy flat-shape migration must keep the key.
* **H46.** Agent gating is semantic, in Russian: `ACTION_WORDS` for any mutation,
  `DELETE_WORDS` for deletion, chapter-number extraction, and the `wantsChapterWriting`
  negations. A port that drops these turns a question ("кто главный герой?") into a write.
  The `agent.test.js` case "refuses a write when the author only asks a question" is the
  regression test.
* **H47.** Agent loop constants: 12 rounds; `toolChoice='required'` on round 0 for actionable
  messages and again once `writeNow`; `writeNow` at round ≥ 6 when nothing was written;
  mutation-only toolset when `writeNow`; **≤8 tool calls per model response** (the 9th is
  rejected); 30000-char tool-output truncation; 4 attempts in the chapter-writing fast path;
  `maxTokens:8000`, `timeoutMs:180000`.
* **H48.** `write_chapter_text` via the agent always forces `chapter_id` and
  `expected_revision` from the freshly read chapter, and strips any `book_id`.
  `assertToolScope` additionally requires the current revision when text already exists.
* **H49.** MCP tool errors are **JSON-RPC successes** (`isError:true` inside `content`),
  while unknown tool/method are JSON-RPC errors. `tools/call` output is
  `JSON.stringify(result, null, 2)` with a `text` content part. `dispatch` returns no reply
  at all when `id === undefined`.
* **H50.** `initialize` echoes `params.protocolVersion` or `'2025-11-25'` and reports
  `serverInfo {name:'storyforge', version:'1.1.0'}` with empty capability objects.
* **H51.** Resource URIs are `storyforge://<slug>/<key>` with exactly 9 allowed keys, listed
  only when the file exists; content type is always `text/markdown`.
* **H52.** Two transports share `dispatch`; the stdio one (`server/mcp.js`) is
  line-delimited, silently drops unparseable lines, and does **not** await handlers (ordering
  is not guaranteed). stdout must carry protocol only.

### 8.6 Import and preservation

* **H53.** Re-import is **destructive by design**: delete the `book` row (cascading to almost
  everything), then rebuild, all inside one `BEGIN IMMEDIATE`; a failure rolls back to the
  previous book (tested).
* **H54.** User data must be captured **before** deletion and restored **after** rebuild,
  remapped by business keys (character name, chapter number, event `ext_id`, secret title,
  old→new node id). The capture filter is `source <> 'импорт'` for both facts and knowledge
  (a NULL `source` is silently excluded by `<>`).
* **H55.** `restoreUserData` never restores `relationship_state`, even though
  `captureUserData` collects it (only rows with `note IS NULL`). Decide explicitly whether to
  keep or fix this asymmetry.
* **H56.** Import-time names are matched through a two-key index (raw and
  parenthetical-stripped); relationships split a target cell on `/,/`; secret holders split on
  `/[,;]|\sи\s/` **after** removing parentheticals and leading `только|все|весь`.
  `voice`/`voice_rules` are separated by `IS_RULE`.
* **H57.** `buildTimeline` is the only producer of `moment` rows and chapter spans: `seq`
  steps of 100, one moment per distinct `story_date` label, `world` guessed from the label,
  events pinned to `chapter.from_moment`, and unwritten chapters appended as
  `Глава N (план)`.
* **H58.** `migrateFacts` **overwrites** `stats.relationships` with a count of
  `relationship_state` rows. If `stats` is a public contract, either reproduce or rename.
* **H59.** Import runs inside the HTTP request (`POST /api/import`, `/api/import-all`) with no
  timeout: a large book blocks the whole server, and `importAll` rethrows on the first failing
  book, abandoning the rest.
* **H60.** `tools/export.js` overwrites generated markdown with a fixed header
  `<!-- Сгенерировано из базы: npm run export. Правки вносите в приложении. -->` and writes
  three file kinds (`bible/characters/<name>.md`, `state/current.md`, `timeline/events.md`).
  It is the only writer of those paths, so hand edits there are lost — a behaviour the
  `write_chapter` MCP prompt explicitly warns about.

### 8.7 Numeric, string and JSON details

* **H61.** `Number()` coercion of path/query values can produce `NaN`; `idList` drops `0`
  and `NaN`; `strList` keeps arbitrary Unicode; `full`/`withEntity` compare against the
  literal string `'1'`.
* **H62.** Booleans arriving as JSON `true/false` are converted to `1/0` via
  `x ? 1 : 0` (routes) or `Number(value)` (`entityTools.cleanData`). SQLite stores them as
  integers; the JSON returned by raw `SELECT *` is `0`/`1`, never `false`.
* **H63.** `JSON.stringify` drops `undefined` properties: R07's `chapter: undefined` and
  `closed`/`missing` conditionals rely on that. `null` is preserved.
* **H64.** `node.data` and `mcp_activity.detail` are JSON-in-TEXT: written with
  `JSON.stringify`, read with `JSON.parse` inside `try/catch` (node.data falls back to the raw
  string; activity detail throws on malformed input).
* **H65.** Timestamps come from SQLite `datetime('now')` — `'YYYY-MM-DD HH:MM:SS'` in UTC,
  **not** ISO-8601 with `T`/`Z`. They are compared and displayed as-is.
* **H66.** `ORDER BY` clauses are part of the API (clients rely on them): chapters by
  `number`, events by `ch.number, e.id`, moments by `seq`, facts in slices by
  `weight DESC, from_seq DESC, id DESC`, characters by `is_main DESC, name`, versions by
  `id DESC`, activity by `id DESC`, promises by `id`, glossary by `section, term`.
* **H67.** Defaults that look like "unset" but are values: `chapter.status='план'`,
  `event.status='план'`, `promise.status='ожидает'`, `annotation.status='открыта'`,
  `fact.status='канон'`, `fact.weight=1`, `fact.source='вручную'`,
  `knowledge.source='вручную'`, `reader_knowledge.stance='знает'`, `ai_run.status='running'`,
  `character.atmosphere_level=0`, `book.min_chars=10000`, `book.target_chars=12000`,
  `book.language='ru'`.
* **H68.** No statement cache: `db.prepare(...)` is called per operation. This is a
  performance characteristic, not semantics, but it explains the measured timings in
  `scaletest.js` (and means a Rust port with cached statements will be faster, not
  different).

---

## 9. Appendix — constants and limits

| Constant | Value | Where |
|---|---|---|
| Default port | `5123` | `index.js:24`, `start.js:11` |
| Bind address | `127.0.0.1` | `lib/http.js:121` |
| Node minimum | `22.5` | `db/index.js:28`, `start.js:20` |
| Far past sentinel | `-1e9` | `lib/state.js:15` |
| Timestamp epoch (reader knowledge) | `-1000000000` | `lib/scenePacket.js:32` |
| Graph layout | `COL_W=320`, `ROW_H=130`, `TOP=60`, first event `y=TOP+150` | `lib/graph.js:8` |
| Moment step on import | `100` (REAL axis) | `import/timeline.js:36` |
| Chat message length | 1..12000 | `index.js:61` |
| Chat history depth | last 8 messages, each last 6000 chars | `index.js:64,69` |
| AI context total | 22000 + 10000 chars | `lib/aiContext.js:120` |
| Chapter slice for extraction | 60000 chars | `index.js:135,210` |
| Agent rounds | 12 | `lib/agent.js:186` |
| Agent tool calls per step | 8 (9th rejected) | `lib/agent.js:238` |
| Agent tool output | 30000 chars | `lib/agent.js:263` |
| Chapter write attempts | 4 | `lib/agent.js:145` |
| Agent `maxTokens` / timeout | 8000 / 180000 ms | `lib/agent.js:147` |
| Chat `maxTokens` / timeout | 6000 / 180000 ms | `index.js:83` |
| Extract `maxTokens` / timeout | 3500 / 150000 ms | `index.js:141` |
| Task `maxTokens` / timeout | plan 3000/120000, draft 8000/180000 | `index.js:187` |
| Suggest `maxTokens` / timeout | 4500 / 150000 ms | `index.js:219` |
| Default `maxTokens` in `completeAiTurn` | 2500, timeout 90000 | `lib/ai.js:69` |
| Version list limit | 100 | `index.js:380` |
| Diff bucket limit | 200 each | `lib/state.js:372,379,387` |
| `list_entities` limit | 100 | `lib/entityTools.js:108,113` |
| Activity default limit | 50 | `index.js:493`, `lib/activity.js:16` |
| Text search length / limit | 2..150 chars / 1..100 (default 30) | `lib/search.js:3,4` |
| FTS snippet | 22 tokens, `[` `]` `…` | `lib/search.js:7` |
| Entity string field max | 120000 chars | `lib/entityTools.js:60` |
| Chapter body max (agent) | 160000 chars | `lib/agent.js:61` |
| Fact value max (proposals) | 500 chars | `lib/proposals.js:37` |
| Source quote | 4..1000 chars, verbatim | `lib/proposals.js:33` |
| Proposal batch | ≤100 | `lib/proposals.js:17,23` |
| Style example body | 40..6000 chars, scene_type ≤60, title ≤120 | `index.js:283,289` |
| API key / model maxima | 500 / 120 chars, no CR/LF | `lib/ai.js:54,57` |
| AI config file mode | `0o600` | `lib/ai.js:62` |

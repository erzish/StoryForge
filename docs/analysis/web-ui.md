# Story Forge web UI — feature and behaviour inventory

Source of truth for this document: the 12 files under `<js-edition>\web`
(`index.html`, `style.css`, `app.js`, `lib.js`, `editor.js`, `rich.js`, `graph.js`, `ai.js`,
`codex.js`, `statev.js`, `info.js`, `activity.js`) and `README.md` (Russian). Server route
implementations were read only to confirm request/response shapes that the frontend depends on.
No runtime observation was performed.

The UI is plain ES-module JavaScript with **no framework, no bundler, no build step, no router, no
TypeScript, and no dependencies**. The server serves `index.html` at `/`, `style.css` at
`/style.css`, and `app.js` at `/app.js`; browser-native module imports resolve the rest. The entire
UI is in Russian and uses Russian strings as data enums (`план`, `черновик`, `написано`,
`открыта`, `применена`, `знает`, `подозревает`, `заблуждается`, `не знает`). Those strings are
part of the wire contract, not just labels.

Conventions below: exact selectors/ids/routes/function names are in `code`; exact UI strings are in
quotes. “Left panel / right panel” mean the fixed columns of the current view, not browser chrome.
In behavioural prose, API paths are written without the `/api` prefix because `lib.api` adds it;
the complete inventory in §5 spells out `/api/...` for every call.

---

## 0. Runtime topology and lifecycle

### 0.1 Modules

| Module | Exports | Role |
|---|---|---|
| `lib.js` | `api`, `el`, `clear`, `num`, `toast`, `prompt`, `showText`, `pasteBox`, `confirm`, `contextMenu`, `closeContextMenu`, `editable` | DOM builder, HTTP wrapper, toasts, modal dialogs, context menu |
| `app.js` | none (side-effect entry) | Boot, global app state, header, nav, view switching, import |
| `graph.js` | `graphView(root, state)` | Blueprint canvas, nodes/edges, selection, layout, node property panel |
| `editor.js` | `editorView(root, state)` | Chapter list, contenteditable editor, notes panel, versions, manual AI edit loop |
| `rich.js` | `mdToHtml`, `htmlToMd`, `textMap`, `rangesForOffsets`, `rangeFromOffsets`, `selectionOffsets`, `findFlexible`, `charsNoSpace` | Markdown ↔ HTML ↔ flat-text projection and note anchoring |
| `codex.js` | `codexView(root, state)` | Character list and editable dossier |
| `statev.js` | `stateView(root, state)` | Moment slider, world-state slice, diff, manual fact extraction |
| `info.js` | `infoView(root, state)` | Reference tabs: promises, secrets, timeline, world, events |
| `activity.js` | `activityView(root, state)` | MCP activity feed |
| `ai.js` | `aiView(root, state)` | Provider settings, per-chapter chat, agent mode, scene context, AI tasks, drafts |

### 0.2 View contract

Every view is a function `view(root, state)`. It is called after `#view` has been cleared, appends
exactly one root subtree, and performs its own async loading. There is **no unmount lifecycle**:
document-level listeners registered by a view stay registered forever and guard themselves with
`root.isConnected` (graph keyboard handler, editor `selectionchange` handler, AI ticker). Any port
must introduce an explicit mount/unmount or equivalent guard.

`show(name, chapterId)` in `app.js` is the only navigator:

1. If `state.beforeLeave` exists, await it; abort if it returns false.
2. If the guard passed or was absent, clear `state.beforeLeave` and `state.editorDirty`.
3. If `chapterId != null`, set `state.currentChapter`.
4. `clear(view)` and toggle `.on` on every `#nav button` whose `textContent === name`.
5. Call `VIEWS[name](view, state)` synchronously inside `try/catch`; on throw, append
   `div.empty` with `Ошибка: <message>`.

Only synchronous throws are caught; view IIFEs are not awaited, so async failures become unhandled
promise rejections or toasts inside the view.

### 0.3 Boot sequence (`boot()`)

1. `GET /api/books`. On failure, append to `#view`: `Сервер не отвечает: <message>` and stop.
2. If the array is empty: show `В базе пусто.` plus a primary button `Импортировать книги`
   (runs the same import as the header button) and stop. No nav is built.
3. `state.book = await GET /api/books/<books[0].id>` — **always the first book in the list**
   (server orders by title). There is no book switcher.
4. `state.chapters = GET /api/books/:id/chapters`; `state.characters = GET /api/books/:id/characters`.
5. `#book-title` = `book.title`; `#book-meta` = `${written.n} глав · ${num(written.chars)} знаков`.
6. Build `#nav` once from `Object.keys(VIEWS)` in order: `Граф`, `Текст`, `Состояние`,
   `Персонажи`, `Активность`, `ИИ`, `Справка`.
7. `show(document.querySelector('#nav button.on')?.textContent || 'Граф')` — on a fresh render no
   button has `.on`, so the app always opens on `Граф`, including after reload.

### 0.4 Error / loading conventions

- `api.req` throws `Error(data?.error || "Ошибка <status>")` for non-2xx; callers either `toast(e.message, true)`
  or let the rejection escape.
- There are no global error boundaries, retries, abort controllers, loading skeletons or spinners
  except the AI waiting bubble and the text `Загрузка…` inside `index.html`.
- Views render an empty shell immediately, then fill asynchronously. A slow `GET` leaves a blank
  panel with no progress indicator.
- `beforeunload` is prevented only when `state.editorDirty?.()` is true.

### 0.5 Reload / refresh semantics

A browser reload destroys all client state, re-runs `boot()`, and lands on `Граф`. The only state
that survives reload is `localStorage` (editor drafts, AI selected chapter, AI mode) and server-side
data (including AI chat history). Viewport, graph selection, filter/scope, open tab and dialog state
do not survive.

---

## 1. Screen map

### 1.1 Primary screens

| Nav label | View function | Root element | Reached by | Shows |
|---|---|---|---|---|
| `Граф` | `graphView` | `#graph` (`div`) | default view; nav | Infinite pannable/zoomable blueprint canvas of chapters, events and free plan nodes; SVG edges; left-top toolbar; bottom-left hint bar; right `320px` details panel |
| `Текст` | `editorView` | `#editor` (`div`) | nav; graph node `Текст` button; node context `Открыть текст`; editor list; AI `Открыть главу`; AI scene-context `Источник: глава #N`; `state.openEditor(id)` | `210px` chapter list, document bar (title, counter, `Версии`, `Сохранить`), formatting bar, centred contenteditable prose, right `300px` notes panel |
| `Состояние` | `stateView` | `#statev` (`div`) | nav | Moment slider and stepper, scope selector, text filter, `Срез для ИИ` / `Извлечь факты`, then slice summary, per-step changes, character cards, relationships, open plot threads, unfired promises |
| `Персонажи` | `codexView` | `#codex` (`div`) | nav | `230px` character list (`+ Персонаж`, main-star, fact counts) and a scrollable dossier editor |
| `Активность` | `activityView` | `#activity` (`div`) | nav | Header bar `Активность ИИ — что записано через MCP` + `Обновить`; flat list of MCP tool actions with relative time |
| `ИИ` | `aiView` | `#ai-view` (`div`) | nav | Top bar (title, mode, chapter, connection status, `Настройки API`), mode hint, collapsible settings, action strip, message history, compose box |
| `Справка` | `infoView` | `div.scroll` (no id) | nav | Five tab buttons and a reference table/card area |

There is no `#info` element and no CSS for `#info`. `Активность` also has no dedicated CSS id/class
rules; it inherits generic `.time-bar`, `.tl-row`, `.empty` styling.

### 1.2 Other reachable states

| State / overlay | Trigger | Content / behaviour |
|---|---|---|
| Empty database screen | `GET /books` returns `[]` | Replaces `#view`: `В базе пусто.` + primary `Импортировать книги`; header import button still present |
| Server-down screen | `GET /books` rejects | `Сервер не отвечает: <message>` in `#view`; no nav |
| View crash screen | a view constructor throws synchronously | `show()` appends `div.empty` `Ошибка: <message>` after clearing/partially filling `#view`, and logs to the console; the nav stays |
| Import result modal | header `Импорт` / empty-state import | `showText('Импорт из markdown', …)` with per-book stats and up to 20 warnings; then toast `Импортировано книг: N` and `boot()` |
| Toast | any `toast()` | One fixed bottom-centre bubble; previous toast removed; auto-hide 2600 ms; error variant uses `var(--bad)` border |
| Context menu | right-click canvas/node/multi-selection/edge | `div.ctx-menu` at cursor, viewport-clamped; closes on outside mousedown or Escape |
| Text-input modal | `prompt()` | Title, single-line or multiline input, `Отмена` / configurable OK; backdrop click and Escape cancel; Enter (or Ctrl+Enter multiline) submits |
| Long-text modal | `showText()` | Read-only textarea, optional hint, `Закрыть` + `Копировать` (clipboard with `execCommand` fallback, label flips to `Скопировано ✓`) |
| Paste modal | `pasteBox()` | 14-row textarea, placeholder `{"edits":[…]}`, optional hint, `Отмена` / `Применить` |
| Confirm modal | `confirm()` | Question, `Отмена` / configurable destructive button (default `Удалить`) |
| Versions modal | editor `Версии` | Version button list, read-only preview, `Восстановить выбранную` |
| Edit review modal | editor `Применить правки` / `ИИ предложит` | Checkbox list of edits, two readonly panes (current / after), `Применить выбранные` |
| Scene-context modal | AI `Контекст сцены` | Four canon sections with optional source-chapter jumps |
| Draft modal | AI `Черновик` / `Открыть черновик` | Editable draft, `Проверить`, `Принять в главу` |
| Fact-proposal modal | AI `Извлечь факты` | Checkbox list of extracted facts, `Записать выбранные` |
| New-fact modal | Codex dossier `+ Факт` | Kind select, slot, value, from-chapter |
| AI settings panel | AI `Настройки API` | Inline collapsible panel (not a modal), `div.ai-settings.on` |

---

## 2. DOM structure of `index.html` and element ids

### 2.1 Static HTML (complete)

```html
<!DOCTYPE html>
<html lang="ru">
<head>
  <meta charset="utf-8">
  <meta name="viewport" content="width=device-width, initial-scale=1">
  <title>Story Forge</title>
  <link rel="stylesheet" href="/style.css">
</head>
<body>
  <div id="shell">
    <header>
      <h1 id="book-title">Story Forge</h1>
      <span class="meta" id="book-meta"></span>
      <span class="grow"></span>
      <button id="import-btn" class="btn"
              title="Импортировать книги из markdown (npm run import)">Импорт</button>
      <nav id="nav"></nav>
    </header>
    <div id="view"><div class="empty">Загрузка…</div></div>
  </div>
  <script type="module" src="/app.js"></script>
</body>
</html>
```

`html, body` are `height:100%`; `body` is `overflow:hidden` and `#shell` is a full-height column flex
container. `header` is fixed-content height; `#view` is `flex:1; overflow:hidden; position:relative`.
Consequence: every view must handle its own scrolling; only `info.js` uses the generic `.scroll`
helper (`height:100%; overflow:auto`), while all other roots are `position:absolute; inset:0` and
scroll internally.

### 2.2 Static element ids

| Id | Element | Written by | Used for |
|---|---|---|---|
| `shell` | `div` | styles only | Full-height flex column |
| `book-title` | `h1` | `app.js` `boot()` | Current book title; initial text `Story Forge` |
| `book-meta` | `span.meta` | `app.js` `boot()` | `"{n} глав · {chars} знаков"` (written chapters only) |
| `import-btn` | `button.btn` | `app.js` `runImport()` | Header import; disabled and labelled `Импортирую…` during request, restored after; title mentions `npm run import` |
| `nav` | `nav` | `app.js` `boot()` | Buttons generated from `VIEWS`; active button carries `.on` |
| `view` | `div` | `app.js` `show()` | Cleared and repopulated for every view; error / empty states appended here |

### 2.3 Generated root ids

| Id | Created in | CSS | Notes |
|---|---|---|---|
| `graph` | `graph.js` | `#graph { position:absolute; inset:0; display:flex }` | Contains `.canvas` + `.details` |
| `editor` | `editor.js` | `#editor { position:absolute; inset:0; display:flex }` | Contains `.chapters-list`, `.doc`, `.notes` |
| `prose` | `editor.js` | `#prose` typography and `.ann` highlight | `contenteditable="true"`, `spellcheck="false"` |
| `codex` | `codex.js` | `#codex { position:absolute; inset:0; display:flex }` | Contains `.codex-list`, `.codex-body` |
| `statev` | `statev.js` | `#statev { position:absolute; inset:0; display:flex; flex-direction:column }` | Contains `.time-bar`, `.state-body` |
| `ai-view` | `ai.js` | `#ai-view { position:absolute; inset:0; display:flex; flex-direction:column }` | Contains `.ai-top`, `.ai-mode-hint`, `.ai-settings`, `.ai-actions`, `.ai-messages`, `.ai-compose` |
| `activity` | `activity.js` | **no rule** | Contains `.time-bar`, `.activity-list` (also unstyled) |

### 2.4 DOM trees produced per view

Graph:

```html
<div id="graph">
  <div class="canvas">
    <svg class="edges"><g> …path… </g></svg>
    <div class="layer">
      <div class="node chapter|event|plan [sel] [hidden-ev] [plan-node]"
           data-node="ID" style="left:…px; top:…px">
        <div class="head">…</div>
        <div class="body">…</div>
        <div class="foot"><button class="btn">Текст</button></div> <!-- chapter only -->
        <div class="port in"></div>
        <div class="port out"></div>
      </div>
    </div>
    <div class="toolbar">
      <button class="btn">+ Глава</button>
      <button class="btn">+ Событие</button>
      <button class="btn">+ План</button>
      <div class="zoom-box">
        <button class="btn" title="Отдалить">−</button>
        <span class="lvl">100%</span>
        <button class="btn" title="Приблизить">+</button>
        <button class="btn" title="Вместить всё">Вместить</button>
      </div>
    </div>
    <div class="hint-bar">Клик — выбрать · протянуть по пустому — рамка · два пальца — прокрутка · щипок — масштаб</div>
  </div>
  <div class="details">…property panel…</div>
</div>
```

Editor:

```html
<div id="editor">
  <div class="chapters-list">
    <div class="item [on]"><b>3. Название</b><small>12 345 зн. · 2 зам.</small></div>
  </div>
  <div class="doc">
    <div class="doc-bar">
      <b>3. Название</b><span class="grow"></span>
      <span class="counter ok|low"><b>12 345</b> <span class="dim">/ 10 000 знаков</span></span>
      <button class="btn">Версии</button>
      <button class="btn primary" disabled>Сохранить</button>
    </div>
    <div class="fmt-bar">
      <button class="fmt" data-cmd="bold">Ж</button>
      <button class="fmt" data-cmd="italic">К</button>
      <span class="fmt-sep"></span>
      <button class="fmt" data-cmd="formatBlock">H1</button>
      <button class="fmt" data-cmd="formatBlock">H2</button>
      <button class="fmt" data-cmd="formatBlock">¶</button>
      <span class="fmt-sep"></span>
      <button class="fmt">—</button> <!-- insertHorizontalRule -->
      <span class="fmt-sep"></span>
      <button class="fmt">↶</button><button class="fmt">↷</button>
      <span class="fmt-sep"></span>
      <button class="btn" disabled>+ Заметка</button>
    </div>
    <div class="doc-wrap">
      <div id="prose" contenteditable="true" spellcheck="false"><p>…</p></div>
    </div>
  </div>
  <div class="notes">
    <h3 class="notes-head">Заметки · 2</h3>
    <div class="ai-row">
      <button class="btn">Собрать для ИИ</button>
      <button class="btn">Применить правки</button>
      <button class="btn">ИИ предложит</button>
    </div>
    <div class="note [done]" data-note="ID">
      <div class="q" title="Показать в тексте">Цитата…</div>
      <textarea class="n-edit" rows="1" placeholder="Что поправить?"></textarea>
      <div class="pill warn">привязка потеряна</div>
      <div class="acts">
        <button class="btn">Готово</button> <!-- or Вернуть -->
        <button class="btn danger">×</button>
      </div>
    </div>
    <button class="btn">Показать выполненные · 3</button>
  </div>
</div>
```

Codex:

```html
<div id="codex">
  <div class="codex-list">
    <div class="item" style="color:var(--accent)">+ Персонаж</div>
    <div class="item [on]"><b>★ Имя</b><small class="dim"> 12 фактов</small></div>
  </div>
  <div class="codex-body">
    <h2>Имя</h2><div class="dim">статус · возраст · пол · группа</div>
    <section><h4>Основное</h4> …label.field… </section>
    <section><h4>Характер</h4></section>
    <section><h4>Голос</h4></section>
    <section><h4>Атмосфера</h4></section>
    <section><h4>Знает / не знает</h4></section>
    <section><h4>Досье</h4> <div class="dos-branch"><div class="dos-head">…</div><div class="dos-row">…</div></div></section>
    <section><h4>Ключевые события · N</h4><div class="tl-row">…</div></section>
    <section><h4>Отношения</h4><div class="tl-row">…</div></section>
    <section><h4>Появления · N</h4><div class="tl-row">…</div></section>
    <button class="btn danger">Удалить персонажа</button>
  </div>
</div>
```

State viewer:

```html
<div id="statev">
  <div class="time-bar">
    <button class="btn" title="Назад">‹</button>
    <div class="time-mid"><input type="range"><div class="moment-label">…</div></div>
    <button class="btn" title="Вперёд">›</button>
    <select>…scope…</select>
    <input type="text" placeholder="фильтр по имени или факту…">
    <button class="btn" title="Собрать срез для модели">Срез для ИИ</button>
    <button class="btn primary" title="Разобрать главу на факты">Извлечь факты</button>
  </div>
  <div class="state-body">
    <div class="slice-info [heavy]">…</div>
    <section class="block"><h4>Изменилось здесь</h4><div class="changes">…</div></section>
    <section class="block"><h4>Персонажи · N</h4><div class="state-grid">…</div></section>
    <section class="block"><h4>Связи · N</h4>…</section>
    <section class="block"><h4>Незакрытое</h4>…</section>
    <section class="block"><h4>Ружья в ожидании · N</h4>…</section>
  </div>
</div>
```

AI panel:

```html
<div id="ai-view">
  <div class="ai-top">
    <b>ИИ-помощник</b>
    <select class="ai-mode" aria-label="Режим ИИ">…</select>
    <select>Вся книга / N. title</select>
    <span class="ai-connection-status" role="status">…</span>
    <button class="btn">Настройки API</button>
  </div>
  <div class="ai-mode-hint">…</div>
  <div class="ai-settings [on]">provider/model/key + buttons</div>
  <div class="ai-actions">…9 buttons…</div>
  <div class="ai-messages">
    <div class="ai-message user|assistant|error|ai-waiting">…</div>
  </div>
  <div class="ai-compose"><textarea rows="4"></textarea><div class="row"><button class="btn primary">…</button></div></div>
</div>
```

Info has no wrapper id: `div.scroll > (div[style=flex…] tabs, div[style=padding…] body)`.

### 2.5 Class-name inventory that carries behaviour

| Class | Used for |
|---|---|
| `.btn`, `.btn.primary`, `.btn.danger` | All buttons; `.danger` only styles on hover; disabled opacity `.4` |
| `.pill`, `.pill.ok`, `.pill.warn`, `.pill.plan`, `.pill.hid` | Status/count chips |
| `.empty` | Centred muted empty-state text, `padding:60px 20px` |
| `.dim` | Muted text |
| `.scroll` | `height:100%; overflow:auto` wrapper (Info only) |
| `.field` / `label.field` | Label + control stack (used as `label.field` and `div.field`) |
| `.toast` | Fixed bottom toast |
| `.modal-bg`, `.modal`, `.modal.wide` | Modal overlay (z-index 150), 440 px / 780 px |
| `.ctx-menu`, `.ctx-item`, `.ctx-ico`, `.ctx-label`, `.ctx-hint`, `.ctx-sep` | Context menu (z-index 300) |
| `.canvas`, `.panning`, `.space`, `.selecting`, `.linking` | Graph canvas cursor/selection modes |
| `.layer` | Transformed HTML node layer |
| `.edges` | SVG layer, `pointer-events:none`; paths re-enable `stroke` |
| `.node`, `.node.sel`, `.node.chapter`, `.node.event`, `.node.hidden-ev`, `.node.plan-node`, `.head`, `.body`, `.foot`, `.no`, `.chips` | Graph node pieces |
| `.port`, `.port.in`, `.port.out` | 13 px circular ports |
| `.marquee` | Selection rectangle (z-index 5) |
| `.toolbar`, `.zoom-box`, `.lvl`, `.hint-bar` | Canvas overlay chrome |
| `.details` | 320 px right panel |
| `.chapters-list`, `.item`, `.item.on` | Chapter/codex list rows |
| `.doc`, `.doc-bar`, `.fmt-bar`, `.fmt`, `.fmt.on`, `.fmt-sep`, `.doc-wrap` | Editor chrome |
| `#prose`, `#prose .ann`, `.ann.flash` | Prose and note highlight |
| `.notes`, `.notes-head`, `.note`, `.note.done`, `.note .q`, `.note .n-edit`, `.note .acts`, `.note.flash` | Notes panel |
| `.counter`, `.counter.low`, `.counter.ok` | Character counter state |
| `.ai-row` | Three-button row above notes (also generic) |
| `.version-list`, `.edit-review-list`, `.diff-grid` | Version/review dialogs |
| `.dos-branch`, `.dos-head`, `.dos-row`, `.dos-slot`, `.dos-val` | Codex dossier sections |
| `.tl-row`, `.ch` | Timeline-like rows (codex, info, activity) |
| `.slice-info`, `.slice-info.heavy`, `.changes`, `.change`, `.badge.new`, `.badge.know`, `.badge.end` | State diff presentation |
| `.state-grid`, `.state-card`, `.sc-head`, `.sc-warn`, `.sc-know`, `.sc-kind`, `.kind-tag`, `.sc-fact` | State character cards |
| `.time-bar`, `.time-mid`, `.moment-label`, `.state-body` | State/AI/activity header bars |
| `.ai-top`, `.ai-mode`, `.ai-mode-hint`, `.ai-connection-status`, `.ai-key-status`, `.ai-settings`, `.ai-actions`, `.ai-context-scroll`, `.ai-context-row`, `.ai-proposal`, `.ai-done`, `.ai-done-row`, `.ai-pulse`, `.ai-message`, `.ai-message-head`, `.ai-message-text`, `.ai-compose` | AI panel |
| `.activity-list` | Activity rows (no CSS rule) |

---

## 3. Interaction specification per screen

### 3.1 Graph canvas (`graphView`)

#### 3.1.1 Coordinate model

- World coordinates are node `x/y` in CSS pixels; node width is the constant `NODE_W = 250`
  (also hard-coded as `.node { width:250px }`; these two must stay in sync).
- View transform is `{ x, y, k }` with initial `{40, 40, 1}`. HTML layer:
  `translate(x px, y px) scale(k)`; SVG group uses the same transform as SVG attributes.
- Zoom percentage label shows `Math.round(k * 100) + '%'`.
- `k` is clamped to `[0.15, 2.5]`. Pan is unbounded.
- Edges attach at the vertical offset `y + 21` (visual centre of the header), right side of the
  source `(x + 250, y + 21)` to left side of the target `(x, y + 21)`.

#### 3.1.2 Rendering and update points

`render()` clears `.layer`, re-creates every node, calls `drawEdges()`, `applyTransform()` and
`renderDetails()`. It is called after most mutations, so the selection highlight, details panel and
edge geometry always re-derive from `graph`. Drag and marquee are the only paths that update DOM
positions without a full render.

Node height is measured from DOM (`offsetHeight`) for marquee hit-testing; edges use a fixed `21px`
port height. Node `data-node` is the numeric node id serialized as a string.

#### 3.1.3 Node kinds and content

| Kind (`n.type`) | Extra class | Header | Body | Footer |
|---|---|---|---|---|
| `chapter` | `.chapter` | `${ent.number ?? '?'}.` in `.no` + `ent.title \|\| 'Без названия'` | status pill (`ok` when `написано`, else `plan`), chars pill (`warn` when status is `написано` **and** `chars_no_space < state.book.min_chars`), summary `slice(0,110) + '…'` (ellipsis always added) | `Текст` button → `state.openEditor(ent.id)` |
| `event` | `.event`, plus `.hidden-ev` when `ent.hidden` | optional `🔒` span with `title="скрытая линия"` + `ent.title \|\| 'Событие'` | summary `slice(0,130)` + `…` only if longer; participant `.chip` list | none |
| `plan` | `.plan` + `.plan-node` | `n.data?.title \|\| 'План'` | `n.data?.text \|\| 'Опиши, что здесь произойдёт'` | none |

Every node also gets `.port.in` (decorative, no handler) and `.port.out` (mousedown starts linking).

#### 3.1.4 Selection

`selection` is a `Set` of numeric node ids. `setSelection(ids, {silent})` rebuilds the set, toggles
`.sel` on each `.node` by comparing `Number(box.dataset.node)`, and re-renders details unless
`silent`. `clearSelection()` is `setSelection([])`.

- **Click (mousedown) a node**: if `Ctrl`/`Cmd`/`Shift` held, toggle membership; otherwise, if the
  node is not already selected, replace selection with it. Selection happens on mousedown so a
  group drag includes the clicked node immediately. After selection, `startDrag` is called.
- **Empty-canvas left mousedown**: clears selection via marquee completion (if no drag and not
  additive) and begins marquee.
- **Right-click empty**: immediately `clearSelection()` and show the empty menu.
- **Right-click node**: if node not selected, select only it; then show multi menu if
  `selection.size > 1`, otherwise node menu.
- **`Escape`**: clears selection and closes the context menu.
- **`Ctrl/Cmd+A`**: selects every node.
- No shift-click on edges, no rubber-band add via keyboard, no “select connected”.

#### 3.1.5 Node dragging

`startDrag(e, n)`:

- Ignores non-left buttons. Ignored when `spaceHeld` (but canvas pan is also blocked over nodes).
- Listens on `document` for `mousemove`/`mouseup`.
- Group snapshot is taken lazily on first move: if the node is selected and selection size > 1,
  drag all selected nodes; otherwise drag only `n`. Originals `(ox, oy)` are per node.
- Delta is divided by `view.k`, so drag speed matches the cursor at any zoom.
- On mouseup, any node whose position changed is sent in one
  `POST /api/nodes/move { items:[{id,x,y}] }`; errors toast. No optimistic rollback on failure.
- Clicking without moving sends nothing.
- Dragging is possible from anywhere on the node except `.port` and the `Текст` button
  (`onmousedown` stops propagation), not just the header, despite the cursor being `move` only on
  `.head`.

#### 3.1.6 Marquee (rubber-band) selection

`startMarquee(e)` records start in canvas-local pixels. A drag only becomes a marquee after the
pointer moves `>= 4px`; before that it is still a click. Additive modes (`Ctrl`/`Cmd`/`Shift`) start
from a copy of the current selection; otherwise the base is empty.

On each move, computes each node's screen rectangle
`(view.x + n.x*view.k, view.y + n.y*view.k, 250*k, measuredHeight*k)` and intersects it with the
marquee. Selection is updated silently during the drag. A `.marquee` div is appended to `.canvas`
with inline `left/top/width/height`; `.canvas.selecting` sets a crosshair.

On mouseup: remove listeners, remove the box; if no movement and not additive, clear selection;
otherwise re-render details. `Ctrl`-marquee adds to the previous selection, it does not intersect.

#### 3.1.7 Pan, zoom, wheel

| Input | Behaviour |
|---|---|
| Middle-button drag on empty canvas | Pan; `.canvas.panning` gives `grabbing` cursor |
| `Space` held (focus not in input/textarea/select/contenteditable) + left drag on empty canvas | Pan; `.canvas.space` gives `grab`, `.panning` gives `grabbing`. `Space` keydown is prevented |
| Wheel / two-finger scroll | `view.x -= deltaX; view.y -= deltaY`; if `Shift` and `deltaX === 0`, horizontal only: `view.x -= deltaY`. `preventDefault` always (listener `passive:false`) |
| `Ctrl`/`Cmd` + wheel (browser pinch) | Zoom anchored at pointer: `zoomAt(Math.exp(-deltaY * 0.01), px, py)` |
| Toolbar `+` / `−` | `zoomBy(1.25)` / `zoomBy(1/1.25)` anchored at canvas centre |
| `Ctrl/Cmd` + `=` or `+` / `-` | Same 1.25 zoom steps at centre |
| `Ctrl/Cmd` + `0` | Sets `k = 1` only; does not reset pan |
| Toolbar `Вместить` / empty menu `Вместить всё` | `fit()` |
| Empty menu `Сбросить масштаб` | `k = 1` only, no recentring |

`zoomAt(factor, px, py)` clamps `k` and adjusts `x/y` so the point under the cursor stays fixed:
`view.x = mx - (mx - view.x) * (k / view.k)`, same for `y`, where `mx/my` default to canvas centre.

`fit()` computes the bounding box of node origins plus `250` width and `160` height, adds `80px`
padding, uses `k = min(1, width/(boxW+80), height/(boxH+80))` clamped to `>= 0.15`, and places the
box at `40px` inset: `view.x = 40 - minX*k`, `view.y = 40 - minY*k`. `fit()` runs once on mount via
`requestAnimationFrame` after the first render, so re-entering the tab resets the viewport.

#### 3.1.8 Edge drawing

- `startLink(e, from)` on `.port.out` mousedown: prevents default/stop propagation, adds
  `.canvas.linking` (`crosshair` on nodes), creates a dashed accent SVG “ghost” path and sets
  `linking = { from, ghost }`.
- Ghost follows the pointer in world coordinates: `(clientX - rect.left - view.x) / view.k`; same
  cubic formula as real edges.
- On mouseup, finds `ev.target.closest('[data-node]')`; if target exists and is a different node:
  - edge kind is `flow` when **both** endpoint nodes are chapters, otherwise `data`;
  - `POST /api/books/:id/edges { from_node, to_node, kind }`;
  - pushes the returned edge and redraws. A duplicate (server 409) shows the server message as an
    error toast.
- Releasing on empty canvas creates nothing. `.linking` and `linking` are always cleared.
- No snapping/preview target highlight; no way to draw from an in-port; no multi-edge labelling.

Edge styling: `flow` = solid `#7aa2f7`, width `2.5`; `data` = `#bb9af7`, width `1.8`,
`stroke-dasharray="5 4"`, `opacity=".75"`. These hex colours are **hard-coded in JS**, not read
from CSS variables.

Edge interactions:

- **Left click** a path: `confirm('Удалить связь?')`, then `DELETE /api/edges/:id`, remove locally,
  redraw, toast `Связь удалена`. (Deletion is not optimistic; it awaits the request.)
- **Right click** a path: context menu with
  `↔ Сделать причинной` (if current `flow`) / `↔ Сделать порядком глав` (if current `data`),
  separator, `🗑 Удалить связь`.
  Type switching is implemented as `DELETE` then `POST` a new edge, not a PATCH; the new edge gets a
  new id. Errors from either half are not caught here.
- Edge paths have `pointer-events: stroke`; the SVG itself is `pointer-events: none`. Mousedown on a
  path still bubbles to the canvas, so starting a click on an edge also arms an empty-canvas marquee
  (usually zero-size), but the subsequent `click` handler stops propagation.

#### 3.1.9 Context menus (exact items)

Empty canvas (`emptyMenu`), positioned at cursor; `Вставить` disabled unless module-level `clipboard`
is non-null:

| Icon | Label | Hint | Action |
|---|---|---|---|
| `📖` | `Добавить главу` | — | `addNode('chapter', canvasPos)` |
| `✦` | `Добавить событие` | — | `addNode('event', canvasPos)` |
| `📝` | `Добавить план` | — | `addNode('plan', canvasPos)` |
| — | separator | | |
| `📋` | `Вставить` | `Ctrl+V` | `pasteNode(canvasPos)` |
| `⬚` | `Выделить всё` | `Ctrl+A` | select all |
| — | separator | | |
| `⤢` | `Вместить всё` | — | `fit()` |
| `1:1` | `Сбросить масштаб` | — | `k = 1` |

Single node (`nodeMenu`):

| Condition | Icon | Label | Hint | Action |
|---|---|---|---|---|
| chapter | `✎` | `Открыть текст` | — | `state.openEditor(ent.id)` |
| always | `⧉` | `Копировать` | `Ctrl+C` | `copyNodes([n])` |
| always | `⎘` | `Дублировать` | `Ctrl+D` | copy + paste at `(n.x+40, n.y+40)` |
| — | separator | | | |
| event | `🔒` | `Убрать из скрытой линии` / `Пометить скрытой` | — | `PATCH /events/:id { hidden: 0/1 }`, merge response, `render()` |
| always | `↔` | `Убрать все связи` | — | disabled if no incident edges; `unlink([n])` |
| — | separator | | | |
| always | `🗑` | `Убрать с холста` | `Delete` | `removeNodes([n])` |

Multi-selection (`multiMenu`):

| Icon | Label | Hint | Action |
|---|---|---|---|
| `⧉` | `Копировать (N)` | `Ctrl+C` | copy all |
| — | separator | | |
| `⇥` | `Выровнять по левому краю` | — | `align('left')` |
| `⇧` | `Выровнять по верхнему краю` | — | `align('top')` |
| `⇹` | `Разложить в ряд` | — | `distribute('h')` |
| `⇳` | `Разложить в столбец` | — | `distribute('v')` |
| — | separator | | |
| `↔` | `Убрать связи` | — | `unlink(selected)` |
| `🗑` | `Убрать с холста (N)` | `Delete` | `removeNodes(selected)` |

#### 3.1.10 Group layout operations

- `align('left')`: set every selected `x` to the minimum selected `x`.
- `align('top')`: set every `y` to the minimum selected `y`.
- `distribute('h')`: sort by `x`, keep the first node as origin, then place at
  `x = x0 + i*(250+40)`, `y = y0`.
- `distribute('v')`: sort by `y`, place at `x = x0`, `y = y0 + i*150`.
- Both require `>= 2` nodes. They mutate local positions, then `persistPositions()` does one
  `POST /nodes/move` followed by a full `render()` (which also re-renders details).
- The same four operations are duplicated as buttons in the multi-selection details panel.

#### 3.1.11 Clipboard, duplicate, paste

`clipboard` is a **module-level** variable, so it survives leaving and re-entering `Граф` in the
same page session, but not reload. It stores, per node:

```js
{ type, data, entity: {...} | null, dx: x - nodes[0].x, dy: y - nodes[0].y }
```

`copyNodes(nodes, silent)` fills it and toasts `Скопировано` or `Скопировано: N`.

`pasteNode(pos)` for each clipboard item:

1. `POST /api/books/:id/nodes { type, x: pos.x+dx, y: pos.y+dy, data }` — this creates the node and,
   for chapter/event types, the underlying entity.
2. If the created node has `ref_table === 'event'`, `PATCH /events/:ref_id` with
   `{ title: ent.title ? ent.title + ' (копия)' : 'Событие', summary, hidden, chapter_id }`.
3. If `ref_table === 'chapter'`, `PATCH /chapters/:ref_id` with
   `{ title: ent.title + ' (копия)' | 'Глава', plan, pov }`.
4. After all items, `reload()` (graph + chapters), select the new ids silently, `render()`, toast
   `Вставлено` / `Вставлено: N`.

Not copied: event participants, chapter status/summary/arc, node ids, edges. The copy always gets a
` (копия)` suffix. Pasting into the same book at a fixed offset can create overlapping nodes.

`Ctrl/Cmd+D` does copy + paste at `+40,+40` in one step (silent copy). `Ctrl/Cmd+V` pastes at
`toCanvas(rect.left + width/2, rect.top + height/3)` — i.e. one third down the visible canvas, not
at the cursor (the cursor position is lost when focus returns to the page).

#### 3.1.12 Node creation

`addNode(type, pos?)`:

- Position: context-menu coordinates when provided; otherwise
  `x = (rect.width/2 - view.x)/view.k - 250/2`, `y = (rect.height/3 - view.y)/view.k`
  (visible centre, one third down).
- For `plan`, opens `prompt('Новая нода плана', { multiline:true, placeholder:'Что здесь произойдёт…' })`;
  cancel (`null`) aborts; the text becomes `data = { title:'План', text }`.
- `POST /api/books/:id/nodes { type, x, y, ...extra }`.
- Pushes the returned node; for a chapter node re-fetches `/chapters` (new chapter appears in other
  panels); selects the new id silently; `render()`; toast
  `Глава создана` / `Событие создано` / `Нода добавлена`.

No title prompt for chapter/event; server defaults (`Глава N`, `Новое событие`, status `план`).
Creation is immediate, with no undo.

#### 3.1.13 Deletion and unlinking

- `unlink(nodes)`: collect all edges incident to any selected node; for each sequentially
  `DELETE /api/edges/:id` (no confirmation), filter locally, redraw, toast `Убрано связей: N`.
  Errors abort midway with no local reconciliation.
- `removeNodes(nodes)`: confirm `Убрать ноду с холста?` or `Убрать N нод с холста?`; sequentially
  `DELETE /api/nodes/:id`; remove nodes and all incident edges from local graph; clear selection;
  render. **The server keeps the chapter/event entity** (default `withEntity` is false), so the
  entity remains in `state.chapters`/the editor while its canvas node is gone; there is no UI to
  put the node back (a re-import may seed nodes again server-side, but the frontend never requests
  that). Local `state.chapters` is not refreshed here.
- There is no UI to delete a chapter entity, event entity, edge from the details panel, or a node
  with its entity (`withEntity=1` is never sent).

#### 3.1.14 Details panel (`renderDetails`)

Empty selection: `div.empty` with `Выбери ноду, чтобы увидеть свойства`.

Multiple selection: `renderMultiDetails(nodes)`:

- `h3` `Выбрано: N`.
- `.sel-summary` with up to 12 `.chip`s (title fallback `Нода`), then ` и ещё N` if more.
- `h3 Разложить`, then a flex row of `По левому краю`, `По верхнему краю`, `В ряд`, `В столбец`.
- If events selected: `h3 События · N`; a chapter select labelled `Перенести в главу` (options:
  `— выбрать главу —` when value null, `— вне главы —` otherwise, then `N. title`) that immediately
  PATCHes every selected event's `chapter_id`; then two buttons `Скрытая линия` / `Обычные` that
  PATCH `hidden:1` / `hidden:0` on every selected event. Each action does `reload()` + `render()` +
  toast (`Глава назначена`, `Помечены скрытыми`, `Убраны из скрытой линии`).
- If chapters selected: `h3 Главы · N`; a status select labelled `Сменить статус` with
  `— не менять —` plus `план`, `черновик`, `написано`; choosing a value PATCHes all chapters and
  toasts `Статус изменён`.
- Footer: `Копировать` and danger `Убрать все`.

Single node (`n.type`):

| Type | Fields (all save on blur unless noted) |
|---|---|
| chapter | `Номер` (number input → `Number(v)`), `Название`, `POV`, `Статус` select `план/черновик/написано`, `Замысел` (textarea rows 4), `Выжимка` (textarea rows 5), primary `Открыть текст`, muted `N знаков без пробелов` |
| event | `Заголовок`, `Что происходит` (rows 6), `Статус` select `план/написано`, `Глава` select (all chapters, `— вне главы —` when set), `Скрытая линия` checkbox (`hidden` 1/0), participants block |
| plan | `Заголовок` and `Текст плана` (rows 8); each updates `n.data` and `PATCH /nodes/:id { data }` |

All chapter/event saves use `PATCH /chapters/:id` or `/events/:id`, merge the returned row into
`n.entity`, then `render()` — which rebuilds the whole canvas and the details form, so focus is lost
after every committed edit. The footer always has danger `Убрать с холста`.

Participants block: existing participants are `.chip`s labelled `name ×`; clicking removes via
`DELETE /events/:ent.id/characters/:characterId`, mutates `ent.participants`, re-renders. An “add”
select lists characters not already participants (`+ добавить участника` placeholder); choosing one
POSTs `/events/:id/characters { character_id }`, appends `{id,name}` locally, re-renders. Server
stores `state_snapshot` as null here.

#### 3.1.15 Graph keyboard shortcuts

The handler is on `document` in `graphView`. On every keydown it first checks `root.isConnected`
(the view may have been unmounted). It treats focus inside `INPUT`, `TEXTAREA`, `SELECT` or
`contentEditable` as “editing” and ignores shortcuts (except the Space acquisition path).

| Key | Action |
|---|---|
| `Space` (keydown, not editing) | `spaceHeld = true`, `.canvas.space`, preventDefault; enables pan |
| `Space` (keyup) | `spaceHeld = false`, remove `.canvas.space` |
| `Escape` | clear selection; close context menu |
| `Delete` | `removeNodes(selected)` if any |
| `Ctrl/Cmd+A` | select all nodes; preventDefault |
| `Ctrl/Cmd+C` | copy selected; preventDefault |
| `Ctrl/Cmd+D` | duplicate selected (copy silent + paste at +40/+40); preventDefault |
| `Ctrl/Cmd+V` | paste from clipboard at canvas centre/third if clipboard exists; preventDefault |
| `Ctrl/Cmd+=` or `Ctrl/Cmd++` | zoom in 1.25 |
| `Ctrl/Cmd+-` | zoom out 1/1.25 |
| `Ctrl/Cmd+0` | `k = 1` |

Notes: `Backspace` does nothing; arrow keys do not nudge; there is no undo/redo; `Ctrl+Z` is not
handled; `Ctrl+V` works without a prior click in the canvas as long as focus is not in a form field.
`Space` on a focused toolbar button is swallowed and enables pan instead of activating the button.

#### 3.1.16 Mobile / touch

There are **no `touchstart`/`touchmove`/`pointer*` handlers**. Trackpad scrolling and pinch work
because browsers synthesize `wheel` / `ctrl+wheel`; a touchscreen cannot pan, zoom, marquee, drag or
open a context menu through the implemented handlers (long-press may produce a `contextmenu` event
on some platforms). The README’s “two fingers / pinch / two-finger tap” wording describes trackpad
behaviour, not touch events.

---

### 3.2 Chapter text editor (`editorView`)

#### 3.2.1 Layout

`#editor` is a horizontal flex row: `.chapters-list` (210 px) | `.doc` (`flex:1`, `min-width:0`) |
`.notes` (300 px). `.doc` is a vertical flex: `.doc-bar`, `.fmt-bar`, `.doc-wrap` (scrolls,
`justify-content:center`, padding `26px 20px 60px`). `#prose` is `max-width:760px`, full width,
`min-height:100%`, `outline:0`, `font:16px/1.75 Georgia, "Times New Roman", serif`. The editor is a
document-like page, not a textarea.

#### 3.2.2 Chapter list

Rendered from `state.chapters` (server order by number). Each row:

- `<b>${number}. ${title ?? 'Без названия'}</b>`
- `<small>`: if `status === 'написано'`, `${num(chars_no_space)} зн.` plus, when `open_notes` > 0,
  ` · ${open_notes} зам.`; otherwise the literal `план`.
- Active chapter row gets `.on` (accent-dim background); hover `.item:hover`.
- Click → `open(c.id)`.

Opening is blocked while `saving` (`toast('Дождитесь завершения сохранения', true)`). If `dirty`:
save a local draft, then `confirm('Есть несохранённые правки. Открыть другую главу?', {ok:'Открыть'})`;
declining returns false and leaves the chapter open.

#### 3.2.3 Opening a chapter and draft restore

`open(id)`:

1. `GET /chapters/:id/text` then `GET /chapters/:id/annotations` (sequential).
2. `chapter = loaded`; `body = chapter.body ?? ''`.
3. Reads `localStorage['storyforge:draft:<bookId>:<chapterId>']`. If parse succeeds and
   `draft.body` is a string different from `body`:
   - if `draft.base_revision !== chapter.revision`, confirm
     `Найден локальный черновик, но глава на диске изменилась. Восстановить черновик для просмотра?`;
   - else confirm `Найден несохранённый локальный черновик. Восстановить?` (`ok: 'Восстановить'`);
   - restore sets `body = draft.body` and `dirty = true` (so saving will create a new revision);
   - declining removes the draft key; malformed JSON also removes it.
4. `prose.innerHTML = mdToHtml(body)`; title `<b>` = `${number}. ${title ?? ''}`;
   `state.currentChapter = id`; render list; apply note highlights; render notes; update counter/bar.

If the chapter has no text, `mdToHtml('')` returns `<p><br></p>` (the CSS `#prose:empty::before`
placeholder `Текст главы…` is therefore effectively unreachable).

#### 3.2.4 Formatting bar

Buttons are created by `fmtBtn(label, title, cmd, arg)` with `data-cmd="<cmd>"`,
`onmousedown={e => e.preventDefault()}` (so the contenteditable selection is not lost), then
`document.execCommand(cmd, false, arg)`, `prose.focus()`, `onEdit()`, `syncToolbar()`.

| Label | Title | Command/arg |
|---|---|---|
| `Ж` | `Жирный — Ctrl+B` | `bold` |
| `К` | `Курсив — Ctrl+I` | `italic` |
| `H1` | `Заголовок главы` | `formatBlock`, `h1` |
| `H2` | `Подзаголовок` | `formatBlock`, `h2` |
| `¶` | `Обычный абзац` | `formatBlock`, `p` |
| `—` | `Разделитель сцены` | `insertHorizontalRule` (no `data-cmd`) |
| `↶` | `Отменить — Ctrl+Z` | `undo` |
| `↷` | `Повторить — Ctrl+Shift+Z` | `redo` |
| `+ Заметка` | — | calls `addNote()`; disabled until a non-collapsed prose selection exists; gets `.primary` while enabled; `.fmt-bar .btn` pushes it right with `margin-left:auto` |

The toolbar has separators `span.fmt-sep`. `syncToolbar()` calls
`document.queryCommandState(cmd)` for every `.fmt[data-cmd]` except `formatBlock` and toggles `.on`.
Heading buttons and ¶ never show active state. `undo`/`redo` call `onEdit()` (mark dirty) after the
execCommand, but no custom history is kept.

On mount: `document.execCommand('defaultParagraphSeparator', false, 'p')` in a try/catch.

#### 3.2.5 Editing, dirty state, counters

- Any `input` event in `#prose` calls `onEdit()`: `dirty = true`, `updateBar()`, and
  `clearTimeout(draftTimer); draftTimer = setTimeout(saveDraft, 700)`.
- `updateBar()` recomputes `charsNoSpace(textMap(prose).text)`. The projection is markdown-shaped
  (heading `#` prefixes, `**`/`*` markers, blank lines), so the counter matches the text that will
  be written rather than the visible glyphs. It sets `.counter.ok` when `n >= state.book.min_chars`,
  else `.counter.low`; innerHTML is
  `<b>{num(n)}</b> <span class="dim">/ {num(min)} знаков</span>` (tabular numerals via CSS).
- `saveBtn.textContent` is `Сохранить •` when dirty, otherwise `Сохранить`; disabled when
  `!dirty || saving`.
- Counter sits in `.doc-bar` (the README says “внизу”; implementation is top).
- There is no word count, no reading time, no per-selection count.

#### 3.2.6 Saving

`save()` returns early if no chapter, not dirty, or already saving.

1. `saving = true`, update bar.
2. `md = htmlToMd(prose)`.
3. `PUT /api/chapters/:id/text { body: md, expected_revision: chapter.revision }`.
4. On success: `chapter.revision = r.revision`, `chapter.body = md`,
   `dirty = htmlToMd(prose) !== md` (catches edits made while the request was in flight),
   save or remove draft, update bar, toast `Сохранено · {num(r.chars_no_space)} знаков`,
   re-fetch `/chapters` and re-render the list.
5. On error: toast error text, `saveDraft()` (so work is not lost). A 409 revision conflict is
   surfaced verbatim from the server; there is no merge UI.
6. `finally`: `saving = false`, update bar.

No server autosave: only explicit `Сохранить`, `Ctrl+S`, version restore, AI edit application and
AI draft acceptance write chapter text. Local drafts are the only autosave.

#### 3.2.7 Local draft autosave

- Key: `storyforge:draft:<bookId>:<chapterId>`.
- Value: `JSON.stringify({ body: htmlToMd(prose), base_revision: chapter.revision, saved_at: Date.now() })`.
- Written by the 700 ms debounce after input, before leaving the view, before switching chapters, on
  save failure, and from `state.beforeLeave`.
- Deleted on successful save when no unsaved delta remains, when the user declines restoring an old
  draft, and when the draft JSON is malformed.
- Storage errors (private mode/quota) show `Не удалось сохранить локальный черновик`.
- Drafts are never garbage-collected globally; one key per chapter remains until resolved.

Navigation guard: `state.beforeLeave = async () => { if (saving) { toast('Дождитесь завершения сохранения', true); return false; } if (!dirty) return true; saveDraft(); return confirm('Есть несохранённый текст. Уйти из редактора? Черновик останется в браузере.', {ok:'Уйти'}); }`.
`state.editorDirty = () => dirty` feeds `beforeunload`.

#### 3.2.8 Keyboard and paste inside prose

Listener on `#prose` only (not document):

| Key | Action |
|---|---|
| `Ctrl/Cmd+S` | `save()`; preventDefault |
| `Ctrl/Cmd+B` | `document.execCommand('bold')`; `onEdit()` (toolbar is not re-synced here) |
| `Ctrl/Cmd+I` | `document.execCommand('italic')`; `onEdit()` |
| `Ctrl/Cmd+Alt+M` | `addNote()`; preventDefault |
| `Ctrl/Cmd+Z`, `Ctrl/Cmd+Shift+Z` | Native contenteditable undo/redo (no handler; browser default) |

Paste is intercepted: `preventDefault`, read `text/plain`, `execCommand('insertText', text)`. All
foreign formatting (HTML, images, tables) is dropped. There is no “keep formatting” option, no
drag-drop file handling, and no explicit paste-as-markdown parser (pasted markdown stays literal
text).

#### 3.2.9 Versions

`showVersions()`:

- Requires a loaded chapter. If `dirty`, toast
  `Сначала сохраните главу или оставьте локальный черновик` and abort.
- `GET /chapters/:id/versions` (server returns up to 100, newest first, fields
  `id, reason, created_at, chars`). Empty → toast `Предыдущих версий пока нет`.
- Builds a modal `.modal.wide` with `h3 Предыдущие версии главы`, a `.version-list` of buttons
  labelled `${created_at} · ${reason || 'правка'} · ${num(chars)} симв.`, a readonly 15-row
  textarea preview, and footer `Закрыть` + danger `Восстановить выбранную` (disabled until a version
  is selected).
- Selecting a version: `GET /chapters/:id/versions/:id`, put `row.body` into the preview, enable
  restore.
- Restore: confirm `Восстановить выбранную версию? Текущая будет сохранена в истории.`; then
  `POST /chapters/:id/versions/:id/restore { expected_revision: chapter.revision }`; close modal,
  `open(chapter.id)`, toast `Версия восстановлена`. Errors toast.
- The modal has no Escape/backdrop close and no focus trap; `Закрыть` is the only non-destructive
  exit.

#### 3.2.10 Markdown round-trip (`rich.js`)

The dialect is deliberately small; anything else is treated as literal paragraph text.

`mdToHtml(md)`:

- Normalize CRLF; split lines.
- Blank line flushes the current paragraph; paragraphs join their lines with a single space.
- Line matching `^(---|\*\*\*|___)\s*$` → `<hr>`.
- Line matching `^(#{1,6})\s+(.*)$` → heading; level clamped to max 3 (`h1..h3`).
- Otherwise accumulate trimmed lines into `<p>`.
- Inline, after escaping `& < >`:
  - `**text**` → `<strong>`;
  - `*text*` → `<em>`, only when preceded by start/space/`(`/`«`/`—` and the opening `*` is not
    followed by space, closing `*` not preceded by space;
  - `_text_` → `<em>` with the same boundary rules.
- Empty input → `<p><br></p>`. Result joined with `\n`.

`htmlToMd(root)` only walks direct children of `#prose`:

- Text nodes are trimmed and emitted as blocks.
- `<hr>` → `---`.
- `<hN>` → `'#'.repeat(N) + ' ' + inline`.
- Any other element is flattened through `inlineToMd` and emitted as a paragraph.
- `inlineToMd`: text nodes are copied with the source's space-normalization replace; `<br>` → `\n`;
  `<strong>/<b>` → `**…**`; `<em>/<i>` → `*…*`; unknown wrappers (including `.ann` note spans) are
  unwrapped. Block-in-inline structure is not preserved.
- Blocks joined with `\n\n`, runs of 3+ newlines collapsed to 2, trimmed, **always terminated with
  one `\n`**.

Consequences: nested/combined emphasis, links, lists, blockquotes, code, images or tables are
flattened or kept as literal characters. `**bold**` and `*italic*` round-trip; heading levels 4–6
are clamped on load and then re-emitted at level 3, so they do not round-trip. The empty document
loads as `<p><br></p>` and saves as `"\n"`.

`textMap(root)` produces the flat markdown projection used for note offsets and the counter:

- Blocks (`P,H1..H6,DIV,HR,BLOCKQUOTE,LI`) are separated by at least one blank line (`\n\n`).
- `<hr>` contributes `---`; `<br>` contributes `\n`; headings contribute `#`×level + space;
  `<strong>/<b>` contribute `**`, `<em>/<i>` contribute `*` around their contents.
- Text-node offsets are recorded in `parts: [{node,start,end}]`.
- Nothing is trimmed: offsets match `text` exactly.

`rangesForOffsets(root,start,end)` maps a flat range to one `Range` per overlapping text node
(used to highlight multi-paragraph notes without breaking document structure).

`selectionOffsets(root)` maps the current window selection to `{start,end,text}` in that projection;
returns null for collapsed/outside selections. If an endpoint is an element rather than a text node,
it uses the first/last contained text part. `text` is taken from the projection, not
`range.toString()`.

`findFlexible(text, needle)` first does an exact `indexOf` scan (returns first index and count),
then a whitespace-tolerant escaped regex (`\s+` for runs of whitespace); returns `{start,end,count}`
or `{start:-1,end:-1,count:0}`.

`charsNoSpace(text)` = `text.split(/\s+/).join('').length`.

#### 3.2.11 Note creation and anchoring

`CTX = 40` characters of prefix/suffix context.

`addNote()` requires a non-collapsed selection:

1. `POST /api/chapters/:id/annotations` with
   `{ quote: sel.text, prefix: text.slice(max(0,sel.start-40), sel.start), suffix: text.slice(sel.end, sel.end+40), start_pos: sel.start, end_pos: sel.end, note: '' }`.
2. Push the returned annotation into local `notes`; re-apply highlights; re-render the notes panel.
3. Find `[data-note="<id>"] .n-edit`, scroll it into view (`block:'nearest'`) and focus it.

There is no dialog and no “create note” confirmation. Note text is written only on blur. An empty
note is deleted on blur; leaving the view while the new field is focused triggers blur (and the
async delete) but the request can race navigation.

`applyHighlights()`:

1. `unwrapHighlights()`: move every `.ann` span's children before it, remove the span, then
   `prose.normalize()`.
2. For each note with `status === 'открыта'`, recompute `textMap(prose)` (previous wrapping split
   text nodes), then `locate(text, n)`.
3. `locate`: exact `text.slice(n.start_pos,n.end_pos) === n.quote` wins; else
   `findFlexible(text, prefix + quote)` and then find the quote inside that match; else
   `findFlexible(text, quote)`. No match → `n._lost = true`.
4. Convert `[start,end]` to ranges with `rangesForOffsets`; wrap each in
   `<span class="ann" data-ann="<id>" title="<note>">` via `range.surroundContents` (exceptions
   skipped). If zero ranges wrapped, mark lost.

Anchoring is quote-plus-context, not character offsets: edits above the note do not break it, but a
changed quote (or an ambiguous short quote with changed context) loses the anchor. `_lost` is
ephemeral and never persisted.

Highlight click: find the card `[data-note="<id>"]`, `scrollIntoView({block:'center', behavior:'smooth'})`,
add `.flash` for 900 ms, focus its `.n-edit`.

`jumpTo(n)` from the quote block: if no `.ann[data-ann="<id>"]`, toast
`Фрагмент не найден — текст сильно изменился`; else scroll the first span to centre and flash all
spans for 900 ms.

#### 3.2.12 Notes panel

Header: `h3.notes-head` `Заметки · {openCount}` where open means `status === 'открыта'`.

Empty (no open, no done): muted block `Выдели текст и нажми «+ Заметка»`, line break, and
`или Ctrl+Alt+M` at 12 px / 70 % opacity.

If there is at least one open note, a three-button `.ai-row` is shown above the cards:

| Button | Title | Action |
|---|---|---|
| `Собрать для ИИ` | `Собрать текст и заметки для модели` | `buildAiPrompt()` |
| `Применить правки` | `Вставить ответ модели` | `applyAiEdits()` |
| `ИИ предложит` | `Получить предложения через настроенный API` | `suggestAiEdits()` |

Each open note is a `.note` card with `data-note`:

- `.q` quote preview: `quote.slice(0,90)` + `…` if longer; `title="Показать в тексте"`; click →
  `jumpTo(n)`.
- `textarea.n-edit` rows 1, placeholder `Что поправить?`, value `note`; `readonly` when done.
  Auto-sized to `max(34, scrollHeight)` on input and once in `requestAnimationFrame`.
  - `Escape`: restore the last saved `n.note` into the field and blur (single-level revert; the
    blur handler then sees no change and does nothing).
  - `Ctrl/Cmd+Enter`: blur (commit).
  - Blur: trim; if unchanged, no-op. If empty, `DELETE /annotations/:id`, remove locally, re-highlight,
    re-render. Else `PATCH /annotations/:id { note }`, update `n.note`, update `title` on all
    matching `.ann` spans. Errors are not caught here (unhandled rejection).
- If open and `_lost`: `.pill.warn` `привязка потеряна`.
- `.acts`: open → `Готово` (PATCH `{status:'применена'}`); done → `Вернуть` (PATCH
  `{status:'открыта'}`); always danger `×` (title `Удалить`, immediate `DELETE`, no confirm).
- `.note.done` for non-open notes (opacity .5, readonly field).

`resolve(n,status)` PATCHes, updates local status, re-highlights, re-renders notes, then refetches
`/chapters` and re-renders the chapter list so `open_notes` counts are current.

Done notes are hidden behind a full-width button `Показать выполненные · N` / `Скрыть выполненные · N`
(local `showDone`); when shown, done cards appear after it. The count is all non-open statuses.

`+ Заметка` enablement is driven by a document `selectionchange` listener: enabled and `.primary`
only when `selectionOffsets(prose)` returns a range; disabled otherwise. The listener guards
`root.isConnected`.

#### 3.2.13 Manual AI edit cycle — prompt assembly

`buildAiPrompt()`:

- Refuses if `dirty` (`Сначала сохрани главу`).
- `GET /chapters/:id/ai-context`; if `ctx.notes.length === 0` → `Нет открытых заметок`.
- Builds notes text:

```text
[{id}] «{quote}»
  абзац: {paragraph}                       (only if paragraph)
  ⚠ цитата встречается {occurrences} раз    (only if !unique && found)
  ⚠ фрагмент не найден в тексте            (only if !found)
  что поправить: {note}
```

Entries joined by a blank line. Then the prompt is assembled from this line array:

```text
Ты редактируешь главу книги «{ctx.book.title}».
Стиль — профиль «{ctx.book.style}» (styles/{ctx.book.style}.md).   (only if style)
Глава {ctx.chapter.number}. «{ctx.chapter.title ?? ''}»
POV: {ctx.chapter.pov}                                             (only if pov)

Ниже текст главы и заметки автора. Для каждой заметки верни точную замену.

ФОРМАТ ОТВЕТА — только JSON, без пояснений:
{"edits":[{"annotation_id":12,"old":"<фрагмент дословно из текста>","new":"<новый текст>"}]}

Правила:
— "old" должен встречаться в тексте ДОСЛОВНО, символ в символ;
— бери целое предложение или абзац, а не пару слов: так замена однозначна;
— не меняй ничего, о чём не просили;
— сохраняй манеру речи персонажей и стиль книги;
— в главе сейчас {num(chars_no_space)} знаков без пробелов, порог площадки {num(min_chars)}.

=== ТЕКСТ ГЛАВЫ ===
{ctx.text}

=== ЗАМЕТКИ ===
{notesText}
```

**Quirk:** the array contains `''` entries intended as blank lines, but it is finalized with
`.filter(x => x !== '')`, so all outer blank-line separators are removed. The embedded chapter text
keeps its own blank lines, but the prompt’s sections run together without the intended spacing.
Ports that want the originally intended layout must fix this deliberately.

Shown in `showText('Промпт для модели', p, { hint })`. The hint is either
`Скопируй, отдай модели, потом вернись и нажми «Применить правки».` or, when some notes lost their
anchor, `Скопируй и отдай модели. Внимание: {lost} заметок потеряли привязку к тексту.`

#### 3.2.14 Applying model edits

`applyAiEdits()`:

1. `pasteBox('Ответ модели', { hint: 'Вставь JSON вида {"edits":[{"annotation_id":…,"old":"…","new":"…"}]}' })`;
   cancel aborts.
2. Strip a leading/trailing Markdown code fence with
   `raw.trim().replace(/^```(?:json)?\s*|\s*```$/g, '')`, then `JSON.parse`. Parse failure →
   toast `Не похоже на JSON`.
3. Accept either a bare array or `{edits:[…]}`; empty/non-array → `В ответе нет правок`.
4. Requires clean `dirty` (`Сначала сохраните главу`).
5. `POST /chapters/:id/apply-edits { edits, dry_run:true }`. If `plan.applied.length === 0`, show
   `showText('Правки отклонены', plan.rejected.map(r => r.reason).join('\n'))` and stop.
6. `reviewAiEdits(path, plan)` (below); cancel keeps nothing.
7. `POST /chapters/:id/apply-edits { edits: selected, expected_revision: plan.revision }`.
8. `open(chapter.id)` (reloads text and notes), toast `Применено: N` plus ` · отклонено: M`; if any
   rejected, `showText('Отклонённые правки', …)` with each item
   `Заметка {annotation_id ?? '—'}: {reason}\n  old: {String(old).slice(0,200)}` and a hint that
   those places were not changed.

The review modal (`reviewAiEdits`):

- Title `Проверьте изменения перед применением`.
- `.edit-review-list` with one checked checkbox per applied edit, label
  `Заметка {annotation_id ?? '—'}: {old.slice(0,90)} → {new.slice(0,90)}`.
- If `plan.rejected.length`, muted `Отклонено: N`.
- `.diff-grid` two columns: `Текущий текст` readonly textarea with `chapter.body` (the last saved
  body), and `После правок` readonly textarea with `plan.preview`.
- Any checkbox change recomputes selection; `Применить выбранные` is disabled when none; with none
  selected the preview reverts to `chapter.body`; otherwise re-runs the dry-run POST and sets
  `preview` into the right pane.
- `Отмена` resolves null; confirm resolves the selected edit objects.

Rejection semantics (server): exact fragment missing → rejected; ambiguous fragment → try to
narrow by note context, otherwise rejected; unique match → applied. Applied edits set their notes
to `применена` and record `resolved_at`/`ai_result`. UI only displays reasons and never auto-retries.

`suggestAiEdits()` skips the copy/paste step: `POST /chapters/:id/ai/suggest-edits {}`, expects the
same plan shape, then uses the same review/commit path. It shows rejected reasons only via toast
count (`Применено: N · отклонено: M`), not the detailed modal.

#### 3.2.15 Editor edge cases

- The “empty book” case renders the chapter list and no chapter, then appends
  `div.empty` `Глав пока нет — создай их на холсте` to `.doc` (below the toolbar).
- `open()` does not clear the prose while loading, so the previous chapter remains visible until the
  new body is applied.
- `renderList()` uses `state.chapters`, which is refreshed after save, note resolution and AI edits,
  but not after graph edits.
- Formatting `formatBlock` with `p` on an already-paragraph block is a browser-level no-op; no
  custom block conversion.
- Native spellcheck is disabled.
- There is no find/replace, no focus mode, no chapter reordering, no chapter creation/deletion from
  the editor, no word goal, no snapshot comparison beyond versions.

---

### 3.3 Character codex (`codexView`)

#### 3.3.1 Layout and list

`#codex` = `.codex-list` (230 px, scrolls) + `.codex-body` (`flex:1`, scrolls, padding
`20px 24px`).

List:

- First item `+ Персонаж` in accent colour; click prompts `Имя нового персонажа` (`ok:'Создать'`);
  empty/cancel aborts; `POST /books/:id/characters { name }`; refetch all characters; re-render;
  open the created id.
- Each character: `<b>` `(is_main ? '★ ' : '') + name`, `<small class="dim"> {state_count} фактов`;
  active row `.on`; click `open(id)`.
- `state.characters` comes from app boot (and is refreshed after create/save/delete), not refetched
  on every mount.

#### 3.3.2 Opening

`open(id)`:

1. `GET /characters/:id` → `current`.
2. `GET /dictionaries` once per view mount (`dict ??=`); used for fact-kind leaves and `key_hint`.
3. `GET /characters/:id/state?chapter=<atChapter>` when `atChapter` is set; without it, the server
   defaults to the last written moment.
4. `current.state = <state response>`; render list and body.

`atChapter` is a local chapter **number** (not id); it starts null and is changed only by the
dossier chapter select.

#### 3.3.3 Body

Header: `h2` name; muted line joining truthy `[status, age, gender, group_name]` with ` · `, or `—`.

Sections, in order:

1. **Основное** (`h4`): editable `Имя`; a flex row of `Статус`, `Возраст`, `Пол`; editable `Группа`;
   checkbox field `Главный герой` (`is_main` 1/0).
2. **Характер**: `Текущий характер` (`summary`, textarea rows 4), `История изменений`
   (`character_history`, rows 3).
3. **Голос**: `Манера речи` (`voice`, rows 3), `Ограничения` (`voice_rules`, rows 3).
4. **Атмосфера**: muted explanation
   `Пока персонаж в сцене, повествование окрашивается его атмосферой`; `Описание`
   (`atmosphere_desc`); `Влияние (0–5)` (`atmosphere_level`, number; saved as
   `Math.max(0, Math.min(5, Number(v) || 0))`).
5. **Знает / не знает** only when `c.knows` is truthy: single multiline editable with an empty
   label (`rows 4`). (The column exists in the `character` table, so this section appears only for
   imported characters that had that section.)
6. **Досье** (below).
7. **Ключевые события · N**: one `.tl-row` per `c.states` entry:
   chapter `гл.{chapter_no}` or `—`, text, optional `.pill.hid` `скрыто`, danger `×` (confirm
   `Удалить факт?`, `DELETE /character-states/:id`, reopen). Footer `+ Факт` quick-add: prompt
   `Новый факт` (multiline, placeholder `гл.8: что произошло с персонажем`, ok `Добавить`); chapter
   number extracted with `/гл\.?\s*(\d+)/i`; hidden set when `/🔒|за кадром/i` matches; POST
   `/characters/:id/states { text, chapter_no, hidden }`; reopen.
8. **Отношения**: one `.tl-row` per `c.relationships` (`to_name` in accent with min-width 110 px,
   `current_state` flexible, danger `×` → confirm `Удалить связь?` → `DELETE /relationships/:id`);
   if none, muted `Связей нет`. Below, an add select `+ добавить связь` listing characters not
   already related; choosing prompts `Что между ними?` (`ok:'Добавить'`); cancel resets the select;
   confirm POSTs `/books/:id/relationships { from_char, to_char, current_state }`; reopen.
9. **Появления · N** only when `c.events.length`: chapter, title, optional muted `state_snapshot`.
10. Danger `Удалить персонажа`: confirm `Удалить персонажа «{name}»?`; `DELETE /characters/:id`;
    refetch characters; clear current; re-render both list and body.

The character payload also contains `incoming` (relationships pointing at this character),
`secrets`, and `history_available`; **none of these are rendered** by the codex. Only outgoing
`relationships`, `states`, `events`, and the computed `state` dossier are shown.

Editable controls (`editable()` from `lib.js`) save on blur, and label Enter blurs single-line /
Ctrl+Enter blurs multiline. There is no Escape revert and no explicit save button. Errors show a
toast. `save(patch)` PATCHes `/characters/:id`, merges the returned row, refetches characters and
re-renders the **list only** — the body header (`h2`, status line) is not re-rendered, so name/status
edits look stale until the character is reopened. The input itself keeps the typed value.

#### 3.3.4 Dossier

Header row: `h4 Досье`, chapter select (`— последняя написанная —` / `на главу N`), muted
`state.at.label`, spacer, `+ Факт`.

Chapter select sets `atChapter` to the selected **number** and reopens the character
(`GET /characters/:id/state?chapter=N`). No chapter means “last written”.

If `state.branches` is empty: muted
`Фактов на этот момент нет. Добавь первый или разбери главу на факты во вкладке «Состояние».`

Otherwise each branch is `.dos-branch` with `.dos-head` = branch title and `.dos-row`s. For every
kind item:

- `.dos-slot` = `kind.title` plus ` · {f.key}` when key exists.
- `.dos-val` = fact value.
- muted `гл.{from_chapter}` when present.
- danger `×` (title `Удалить факт`) → `DELETE /facts/:id`, reopen (no confirm).

Then, separately, an **Осведомлённость** branch built from `state.awareness` groups in this order:
`знает` (pill `ok`), `подозревает` (`warn`), `заблуждается` (`warn`), `не знает` (`plan`); only
non-empty groups. Each row shows the pill and the joined `secret_title ?? topic` values with ` · `.

`+ Факт` opens a modal (not `lib.prompt`):

- `Вид` select of `dict.fact_kinds.filter(k => k.parent)` (leaves only), label
  `{title}  ({parent title})`.
- Hint line `обычные слоты: {key_hint}` updated on change; hidden when none.
- `Слот` text input (`слот — необязательно`), `Значение` textarea rows 3, `С какой главы` number
  default `atChapter ?? last written chapter number ?? 1`.
- `Отмена` / `Добавить`; backdrop click cancels; no Escape.
- Save requires non-empty value; POST `/books/:id/facts { subject_type:'character', subject_id,
  kind, key: key||null, value, from_chapter: Number||null }`; reopen; toast `Факт добавлен`.

---

### 3.4 World-state viewer (`stateView`)

#### 3.4.1 Time bar

Ordered controls:

1. `‹` button (title `Назад`) → `step(-1)`.
2. `.time-mid` containing a full-width `input[type=range]` (`min 0`, `max moments.length-1`,
   `accent-color: var(--accent)`) and `.moment-label`.
3. `›` button (title `Вперёд`) → `step(1)`.
4. Scope select (`max-width:180px`): `участники момента` (`scene`, default), `только главные`
   (`main`), `все персонажи` (`all`).
5. Filter text input (`max-width:220px`, placeholder `фильтр по имени или факту…`); input updates
   `filter` lowercased and re-renders locally (no server call).
6. `Срез для ИИ` (title `Собрать срез для модели`) → `buildPrompt()`.
7. Primary `Извлечь факты` (title `Разобрать главу на факты`) → manual extraction flow.

The range input’s `oninput` calls `load()` immediately on every tick — no debounce, so dragging the
slider can fire many `/state` + `/diff` request pairs. `step(d)` clamps to `[0, moments.length-1]`
and also reloads.

#### 3.4.2 Initial moment

`GET /books/:id/moments`; empty → `.empty` message
`Линии времени нет. Переимпортируй книгу: npm run import` and stop.

Default index: the last moment whose `chapters` comma-list contains the highest written chapter
number; if none, index 0. `slider.max = moments.length-1`.

Moment rows (server) include `id`, `seq`, `label`, `world`, `entries`, and a `chapters` string of
chapter numbers active at that moment. Time is a story axis, not chapter order.

#### 3.4.3 Loading a moment

`load()`:

1. Label HTML: `<b>{moment.label}</b> <span class="dim">{world ?? ''}{chapters ? ' · главы ' + chapters : ''}</span>`.
2. Determine the character scope:
   - `all` → no `characters` parameter.
   - `main` → ids of characters with `is_main`.
   - `scene` → `GET /books/:id/events`; keep events whose `chapter_no` is in the moment’s chapter
     list; split each `participants` string on `, `; map names to `state.characters`; if no match,
     fall back to main-character ids; if the moment has no chapters, fall back to main ids.
3. `GET /books/:id/state?moment=<id>[&characters=1,2,3]`.
4. If `idx > 0`, `GET /books/:id/diff?from_moment=<prev.id>&to_moment=<current.id>`; otherwise
   `changes = null`. Requests are sequential.
5. `render()`.

#### 3.4.4 Rendering

Empty `data` → blank body.

- `.slice-info` (class `heavy` when `totals.facts_in_slice > 400`):
  `в срезе {num(facts_in_slice)} фактов из {num(facts_total)} в базе`, muted
  ` · персонажей {data.characters.length}`, and when heavy ` · для промпта лучше сузить охват`.
- **Изменилось здесь** block only when diff has any of `appeared`, `learned`, `ended`:
  - appeared: `.badge.new` `+`, bold `subject ?? subject_type`, muted `[{kind}]`, value;
  - learned: `.badge.know` `?`, bold `who ?? holder_label`, muted `{stance}: `,
    `secret_title ?? topic`;
  - ended: `.badge.end` `−`, bold subject, `<s>` value.
  `changes` container has a 2 px accent left border (`.changes`).
- **Персонажи · N** `.state-grid` (auto-fill, min 300 px). Characters are filtered by `filter`
  matching `c.name` or any `c.facts[].value`. Card:
  - `.sc-head` bold `(★ )name` + muted `group_name`;
  - `.sc-warn` when misbeliefs: `⚠ заблуждается: ` + joined titles;
  - `.sc-know` when knows: `знает: ` + joined titles;
  - facts grouped by `kind`: `.sc-kind` with `.kind-tag` (uppercase kind) and `.sc-fact` rows;
    each fact has `title` `с {from_label ?? '?'}` plus ` до {to_label}` when it ended;
  - if no facts and no knows: muted `фактов на этот момент нет`.
- **Связи · N** only if any relationships match the filter: `.tl-row` with
  `from_name → to_name` (min-width 190 px) and muted `state`.
- **Незакрытое** when `data.plot` non-empty: rows with `.kind-tag` and value.
- **Ружья в ожидании · N** when `data.open_promises` non-empty: `гл.{planted_ch}` or `—`, title,
  muted status.

All data is rendered from the last loaded slice; filtering is client-side only over the loaded
characters/relationships. The state response also carries `scope`, `world`, `locations`, and
`totals.knowledge_in_slice`; the screen does **not** render them.

#### 3.4.5 “Срез для ИИ” text

`stateAsText()` builds, in order:

```text
СОСТОЯНИЕ МИРА на момент: {label}{ (world)}
Главы этого момента: {chapters}

=== ПЕРСОНАЖИ ===
{name}{ (group)}
  характер: {summary}
  голос: {voice}
  атмосфера: {atmosphere_desc} {level}/5
  ЗНАЕТ: {secret_title ?? topic}
  ЗАБЛУЖДАЕТСЯ: {secret_title ?? topic}{ — верит: belief}
  НЕ ЗНАЕТ: {secret_title ?? topic}
  [{kind}] {value}
...
=== СВЯЗИ ===
{from} → {to}: {state ?? '—'}

=== НЕЗАКРЫТОЕ ===
— {value}

=== РУЖЬЯ В ОЖИДАНИИ ===
— {title} (посеяно гл.{planted_ch ?? '?'})
```

The character and relationship headers are always emitted; `Главы этого момента` appears only when
the moment has chapters, and the last two sections only when non-empty. `suspects` are not included
in this text export (only `знает`, `заблуждается`, `не знает`).

`showText('Срез мира', text, { hint: 'Это состояние на выбранный момент. Отдай модели вместо пересказа предыдущих глав.' })`.

#### 3.4.6 Manual fact extraction

`extractFacts()` is the README’s “Извлечь факты” loop and does not call the configured AI API:

1. Written chapters = `state.chapters.filter(c => c.status === 'написано')`; none → toast
   `Нет написанных глав`.
2. `prompt('Из какой главы извлекать факты?', { value: String(lastWritten.number), ok: 'Собрать промпт' })`.
   Empty/cancel aborts. Match by chapter **number**; not found → `Глава не найдена`.
3. `GET /chapters/:id/text` for the body; `GET /books/:id/state?chapter=<number-1>` for the state
   before the chapter.
4. Builds a “known facts” list from `before.characters` that have facts or knowledge:
   `{name}: [kind] value · ЗНАЕТ title …`, and a character id list `{id} — {name}`.
5. Shows the prompt (retained verbatim, including blank lines):

```text
Разбери главу книги «{book.title}» на факты для базы состояния мира.
Глава {n}. «{title}»

Нужны только НОВЫЕ или ИЗМЕНИВШИЕСЯ факты — то, чего не было в состоянии до главы.

ФОРМАТ ОТВЕТА — только JSON, без пояснений:
{"facts":[{"character_id":3,"kind":"статус","value":"в шоке после случившегося у оврага"}],
 "knowledge":[{"character_id":5,"stance":"заблуждается","topic":"пекарь неравнодушен к Клио","belief":"он на неё засматривается"}],
 "closed":[{"character_id":3,"was":"старый факт, который перестал быть верным"}]}

Виды фактов (kind): статус, место, черта, внешность, предмет, имя, эмоция, цель, состав, событие.
Позиции знания (stance): знает, не знает, заблуждается, подозревает.

Правила:
— один факт = одно утверждение, не абзац;
— не повторяй то, что уже было верно до главы;
— заблуждение фиксируй отдельно от незнания: персонаж действует исходя из ложного;
— предметы при себе тоже факт: они потом выстреливают.

=== ПЕРСОНАЖИ (id — имя) ===
{chars}

=== СОСТОЯНИЕ ДО ГЛАВЫ ===
{known || '(пусто)'}

=== ТЕКСТ ГЛАВЫ ===
{text}
```

Hint: `Отдай модели, потом вернись и нажми «Извлечь факты» ещё раз — предложит вставить ответ.`
Then `pasteBox('Ответ модели: факты', { hint: 'Вставь JSON с полями facts / knowledge / closed. Можно закрыть и вставить позже.', ok: 'Разобрать' })`.

6. Parse JSON after stripping fences; accept `payload.facts` and `payload.knowledge`; if both empty
   → `В ответе нет фактов`.
7. Preview lines: `+ [{kind}] {name}: {value}` and `? {name} — {stance}: {topic}`.
8. Confirm `Добавить {facts.length + knows.length} записей за главу {n}?`.
9. Sequentially `POST /books/:id/facts` for each fact with
   `{ subject_type:'character', subject_id, kind: kind||'событие', value, from_chapter: n, source:'ИИ' }`
   (skips empty values), and `POST /books/:id/knowledge` for each knowledge with
   `{ character_id, secret_id ?? null, topic ?? null, stance: stance||'знает', belief ?? null, from_chapter: n, source:'ИИ' }`
   (skips entries with neither topic nor secret_id).
10. Toast `Добавлено записей: {ok}`, show `Что добавлено` preview with hint
    `Всё можно поправить в кодексе персонажа.`, then `load()`.

`closed` entries in the model response are ignored by this UI path. There is no confirmation per
fact beyond the one aggregate confirm, and no rollback if a later POST fails.

---

### 3.5 Reference (`infoView`)

Wrapped in `.scroll`. A tab strip (`display:flex; gap:4px; padding:12px 20px 0`) with five
`.btn`s; the active tab gets `.primary`. Clicking clears `.primary` from all siblings, adds it to the
clicked button, and calls the section loader. Initial tab is always `Ружья` (its loader runs at
mount); the selected tab is not remembered across view switches.

| Tab | Endpoint | Rendering |
|---|---|---|
| `Ружья` | `GET /books/:id/promises` | Table headers `#`, `Что заявлено`, `Гл.`, `Должно выстрелить`, `Статус`. Rows: `ext_no` muted, title, `planted_ch` muted or `—`, `expected_payoff` or empty, status pill: `ok` if `/выстрелило/i` matches status, else `warn` |
| `Тайны` | `GET /books/:id/secrets` | One card per secret (`background var(--panel)`, border, radius 8, padding 12/14, max-width 860): bold title; knowledge pills `{who} — {stance}` (`ok` when stance `знает`, else `plan`) with 5 px gaps; muted footer `читатель: {reader_knows ?? '—'} · раскроется: {reveal_plan ?? '—'}` |
| `Таймлайн` | `GET /books/:id/timeline` | `.tl-row` per entry: `гл.{chapter_no}` or `—`, muted `story_date` (min-width 190, 12 px), description, optional `.pill.hid` `скрыто`, muted `who` |
| `Мир` | `GET /books/:id/world` | `Правила мира` rows with `🔒` (title `нерушимое`) when `locked`, text. If `hidden`: `Скрытые линии` cards with `{code} — {name}`, truth, status. If `locations`: `Локации` rows with accent name (min 130 px) and description first 160 chars. Then `Глоссарий · N` table `Термин`, `Значение`, `Раздел` |
| `События` | `GET /books/:id/events` | Table `Гл.`, `Событие`, `Участники`, `Статус`; title prefixed with `🔒 ` when hidden; summary first 150 chars muted below; participants string; status pill `ok` when `написано`, else `plan` |

All sections are read-only. There is no create/edit/delete here for promises, secrets, rules,
locations, glossary, timeline or events (events are edited only from the graph details panel).
Loaders are async and not awaited by the click handler; errors are unhandled (blank area).

`table(head, rows)` builds `table > thead > tr > th…` and `tbody > tr…`; global `th, td` styles give
left alignment, 7/10 padding, bottom border, top vertical alignment; `th` is muted 12 px.

---

### 3.6 Activity (`activityView`)

- `.time-bar`: a flexible block with bold `Активность ИИ`, muted `— что записано через MCP`, and
  `Обновить` button; then `.activity-list`.
- `load()` does `GET /books/:id/activity`; on failure toast `Не удалось загрузить: {message}` and
  return. Empty → `.empty` with `Пока пусто.` + line break +
  `Как только модель через MCP запишет факт, сохранит главу или отметит ружьё — здесь появится строка.`
- Each row is `.tl-row` with `title` = raw tool name:
  - icon span `.ch` from `TOOL_ICON`: `add_fact +`, `close_fact –`, `add_knowledge ?`,
    `write_chapter_text ✎`, `add_promise ☆`, `update_promise ☆`, `run_import ↻`, fallback `•`;
  - summary text (flex 1);
  - muted right-aligned time (min-width 110, 12 px) from `timeAgo(created_at)`.
- `timeAgo` treats the SQLite timestamp as UTC by replacing the space with `T` and appending `Z`:
  `<5s` → `только что`; `<60s` → `N сек назад`; `<1h` → `N мин назад`; `<24h` → `N ч назад`;
  otherwise `toLocaleString('ru-RU', {day:'numeric', month:'short', hour:'2-digit', minute:'2-digit'})`.
- No auto-refresh, no pagination, no filtering. Server default limit is 50; no `limit` query is sent.
- This screen has **no layout CSS**: `#activity` and `.activity-list` are ordinary blocks with no
  `overflow`, and `#view` is `overflow:hidden`. Rows beyond the viewport are therefore clipped, not
  scrollable.

---

### 3.7 AI panel (`aiView`)

#### 3.7.1 Persistent selections

- Chapter key: `storyforge:ai-chapter:<bookId>` stores either `'all'` or a numeric chapter id.
- Mode key: `storyforge:ai-mode:<bookId>` stores `agent` or `chat`.
- On first entry for a book, `state.aiChapter` is derived: stored value `'all'` → null; no stored
  value → `state.currentChapter`; stored id that still exists → id; invalid → null.
- The chapter select is rebuilt on every mount and on chapter change. Changing it:
  sets `state.aiChapter`, sets `state.currentChapter` when non-null, writes the storage key,
  `clear(root)`, and calls `aiView(root, state)` again. This rebuilds the whole panel but keeps the
  per-chapter chat caches in `state`.
- The mode select is restored from storage; its `change` updates `localStorage`, the mode hint and
  the input placeholder.

#### 3.7.2 Header / chrome

`.ai-top` row: bold `ИИ-помощник`; `.ai-mode` select (`Агент · меняет книгу` / `Диалог · только ответ`,
`aria-label="Режим ИИ"`); chapter select (`Вся книга` + every chapter as `N. title`); a
`.ai-connection-status` span with `role="status"`; `Настройки API` button toggling
`.ai-settings.on`.

Mode hint text:

- agent: `Агент выполняет просьбы в книге: пишет главы, меняет персонажей, состояние и события. Выполненные действия появятся под ответом.`
- chat: `Диалог отвечает на вопросы и предлагает текст в чате, не меняя книгу.`

Input placeholders:

- agent: `Например: напиши главу 11 и сохрани её; добавь персонажа; обнови его состояние…`
- chat: `Спросите о сюжете, персонаже или попросите предложить фрагмент…`

#### 3.7.3 Settings

Collapsible `.ai-settings` (`display:none` → `.on`; max-height 55vh, scrolls):

- `Провайдер` select: `DeepSeek` (`deepseek`) / `OpenRouter` (`openrouter`).
- `Модель` text input. Defaults: `deepseek` → `deepseek-flash`, `openrouter` → `openrouter/free`
  (`DEFAULT_MODEL`).
- `API-ключ` password input, `autocomplete="off"`, placeholder `Введите API-ключ`.
- `.ai-key-status` line (`role="status"` on the connection line only; key status is a div).
- Row: primary `Сохранить`, `Проверить подключение`, danger `Удалить ключ`.
- Static note: `После сохранения поле ключа очищается: сервер не возвращает секрет в браузер. Ключи DeepSeek и OpenRouter хранятся отдельно. Для переключения выберите провайдера и нажмите «Сохранить». Проверка и сообщения могут расходовать лимит API.`

`loadSettings()`: `GET /ai/settings` → set provider/model, `setStatus(config)`, and auto-open the
panel when `!config.has_key`.

`setStatus(config)`:

- stores `savedConfig`;
- connection line: `Активен: {DeepSeek|OpenRouter} · {model} · {ключ сохранён|нет ключа}`;
- per-provider profile (`config.profiles?.[provider.value]` fallback to top-level) controls key
  status: `Ключ {provider} сохранён. Поле пустое, потому что ключ скрыт. Чтобы заменить его, введите новый и сохраните.`
  or `Для {provider} ключ ещё не сохранён.`;
- key placeholder becomes `Ключ сохранён · введите новый для замены` or `Введите API-ключ`.

Provider change: model is set to `savedConfig.profiles[provider].model || DEFAULT_MODEL[provider]`,
key field cleared, status refreshed, then ` Выбор ещё не применён: нажмите «Сохранить».` appended to
the key status. Model input change replaces key status with
`Имя модели изменено. Нажмите «Сохранить», чтобы применить его.`

Save: `PUT /ai/settings { provider, model, api_key }`; clear the key field on success; `setStatus`;
toast `Выбран {provider} · {ключ сохранён|ключ не задан}`.
Delete key: `PUT /ai/settings { provider, model, clear_key:true }`; clear field; `setStatus`; toast
`API-ключ удалён`.
Test: refuses when provider/model differ from `savedConfig` with
`Сначала сохраните выбранного провайдера и модель, затем проверьте подключение.`; disables the
button and labels it `Проверяю…`; calls `POST /ai/test {}`; on success key status
`Подключение работает. Ответила модель {model}.` and toast `Подключено: {model}`; on failure
`Проверка не прошла: {message}`; restores label.

Keys are never returned by the server; the UI only knows `has_key` per profile.

#### 3.7.4 Action strip

`.ai-actions` is a wrapping row of `.btn`s, all present regardless of mode:

| Label | Action |
|---|---|
| `Обновить переписку` | `loadHistory(true)` |
| `Контекст сцены` | `showContext()` (requires a chapter) |
| `Поиск по книге` | `searchText()` |
| `Извлечь факты` | `extractFacts()` (requires a chapter; server-side AI) |
| `План главы` | `runTask('plan')` |
| `Черновик` | `runTask('draft')` |
| `Разбор стиля` | `runTask('style_review')` |
| `Открыть черновик` | requires chapter; shows session draft if present, else toast `Черновика пока нет` |
| `Образец стиля` | `addStyleExample()` |

`requireChapter()` toasts `Сначала выберите главу` when the chapter select is on `Вся книга`.

#### 3.7.5 Messages and send

History is stored per `key = "<bookId>:<chapterId ?? ''>"` in `state.aiChats`; loading flags,
has-more and pending are in `state.aiLoaded`, `state.aiHasMore`, `state.aiPending`. These survive
tab switches (module state lives on `state`), not reload.

`renderMessages()`:

- While loading: `.empty` `Загружаю историю переписки…`.
- On history error: `.ai-message.error` with `Не удалось загрузить переписку: {message}` and
  `Повторить загрузку`.
- Empty and no pending: `.empty` with
  `Напишите агенту, что сделать в книге. Он сможет создать или изменить главу, персонажа и другие записи. Для обсуждения без изменений выберите «Диалог».`
- If `has_more`: muted `Показаны последние 200 запросов этой главы.`
- Each message `.ai-message {role}` with `.ai-message-head` (`Вы` / `Помощник` / `Ошибка запроса`),
  `.ai-message-text` preserving newlines (`white-space:pre-wrap`), and:
  - if `actions.length`: `.ai-done` block `Выполнено действий: N`; per action `.ai-done-row` with
    `action.label` and, for `entity_type === 'chapter'` with id, `Открыть главу` → `state.openEditor(id)`;
  - assistant: `Копировать` button (clipboard; toast `Ответ скопирован` / `Не удалось скопировать`);
  - error with `retry`: `Повторить запрос` button that refills the input with `m.retry` and calls
    `send()` unless pending.
- Pending: `.ai-message.ai-waiting` with `role="status"` and `aria-live="polite"`; header
  `Агент выполняет задание` or `Помощник отвечает`; text with `.ai-pulse` (8 px accent dot,
  1.2 s pulse; disabled under `prefers-reduced-motion`) and
  `{Читает книгу, вызывает инструменты и сохраняет изменения|Ждём ответ модели} · {N} с`.
- Send button disabled when pending/loading/error; label `Выполняю…`, `Загружаю…`,
  `Поручить агенту` (agent) or `Отправить` (chat).
- `messages.scrollTop = messages.scrollHeight` after every render.

A 1 s `setInterval` ticker (stops when `panel` is disconnected) updates the elapsed seconds text and
re-renders when the history length changes or pending appears/disappears.

`send()`:

1. Trim input; ignore if empty, pending, loading or history error.
2. Build `prior`: filter history to assistant messages plus user messages **not followed by an
   error**, take the last 8, map to `{role, content: content.slice(-6000)}`.
3. Push the user message into local history; record `state.aiPending[key] = {started:Date.now(), mode}`;
   clear input; render.
4. `POST /books/:id/ai/{agent|chat}` with
   `{ chapter_id: selected ? Number : null, message, history: prior }`.
5. If `result.error`: push an error message with `retry: message` and `actions` (agent route returns
   HTTP 200 with an error object on agent failure) and toast. Otherwise push assistant
   `{content: result.text, actions: result.actions || []}`.
6. If any actions: refetch `/chapters` and `/characters` in parallel, update `state`, rebuild the
   chapter select preserving the current value. Refresh failure is a toast
   `Действия сохранены, но список не обновился: {message}`.
7. If `finish_reason === 'length'`: toast `Ответ обрезан лимитом модели`.
8. Network/HTTP failure: push an error message with `retry` and toast; `finally` clears pending and
   re-renders.

No streaming, no stop/cancel, no regenerate, no message editing, no delete-history UI. The waiting
bubble shows elapsed time only; the answer appears all at once.

History load: `GET /books/:id/ai/history` (append `?chapter_id=<id>` when a chapter is selected) →
`result.messages` replaces the local array, `result.has_more` stored. Guarded against concurrent
loads and pending requests (`Дождитесь ответа, затем обновите переписку`).

#### 3.7.6 Scene context modal

`showContext()` requires a chapter; `GET /books/:id/scenes/:chapter/context`; modal `.modal.wide`
titled `Контекст: глава {packet.chapter.number}` with `.ai-context-scroll` (max-height 55vh) and
`Закрыть`.

Sections:

1. `Канон мира до главы` — up to 100 `packet.world_truth` rows
   `[{kind}] {subject || subject_name || ''}: {value}`.
2. `Что доступно POV: {packet.pov_knowledge?.character || 'не определено'}` — iterate stances
   `knows`, `suspects`, `misbeliefs`, `unaware`; rows `{stance}: {topic || secret_title}{ — belief}`.
3. `Что знает читатель` — if `!packet.reader_knowledge_tracked`, muted
   `Осведомлённость читателя ещё не размечена.`; rows from `packet.reader_knowledge`
   `{stance}: {topic}`.
4. `За кадром — только для автора` — rows `{line}: {offscreen}{ → visible_effect}`.

Every row can append a `Источник: глава #N` button when `source_chapter_id` (closes modal, opens
editor) and a muted `«{source_quote.slice(0,150)}»` when present.

#### 3.7.7 Search

`searchText()` prompts `Поиск по главам и событиям за кадром`; empty/cancel aborts.
`GET /books/:id/text-search?q=<encodeURIComponent(q.trim())>`; renders
`Глава {number} «{title}»: {snippet}` for `result.chapters` and
`За кадром, гл.{chapter_no}: {offscreen}` for `result.offscreen`, joined by blank lines, in
`showText('Поиск: {q}', …)`; no matches → literal `Совпадений нет`. No navigation to the hit.

#### 3.7.8 Extract facts (server-side AI)

`extractFacts()` requires a chapter; `POST /chapters/:id/ai/extract {}`:

- No `accepted` → `showText('Факты не найдены', rejected reasons joined by newline or
  `Новых утверждений нет`)`.
- Otherwise a modal `Проверьте факты перед записью в канон` with a checked checkbox per accepted
  proposal (`.ai-proposal` label), text
  `{p.type}: {p.value || p.topic} · источник: «{p.source_quote.slice(0,170)}»`, optional muted
  `Отклонено без источника или с ошибкой: N`, and `Отмена` / primary `Записать выбранные`.
- Commit sends `POST /chapters/:id/ai/commit-facts { expected_revision: result.revision, proposals }`
  for the checked subset; toast `Записано утверждений: {saved.inserted}`.
- Selection is all checked by default; no per-fact edit.

#### 3.7.9 Plan / draft / style tasks

`runTask(kind)` requires a chapter and a per-chapter session workspace
`state.aiWorkspace[chapterId] = { plan:'', draft:'', revision:null, audited:false }`.

- `style_review` requires a non-empty `work.draft`; otherwise toast `Сначала подготовьте черновик`.
- `draft` first prompts `Тип сцены для подбора образцов стиля` with current `work.sceneType` or
  `общий`; cancel aborts; trimmed value capped at 60 chars, empty → `общий`.
- `POST /chapters/:id/ai/task { kind, input: kind === 'draft' ? work.plan : work.draft, scene_type: work.sceneType || 'общий' }`.
  (For `plan`, `input` is `work.draft` by this expression; the server ignores it for plan and uses
  the built-in plan instruction.)
- `plan` reply: `prompt('План главы — можно поправить перед черновиком', { value: result.text, multiline:true, ok:'Сохранить план' })`;
  if not cancelled, store in `work.plan` and toast `План сохранён для этого сеанса`. Nothing is
  persisted server-side.
- `draft` reply: store `work.draft = result.text`, `work.revision = result.revision`,
  `work.audited = false`; open `showDraft(id, work)`.
- `style_review` reply: `showText('Редакторский разбор стиля', result.text)`.

`showDraft(id, work)` modal `Черновик главы`:

- 20-row editable textarea with `work.draft`; typing updates `work.draft`, resets `audited` and
  `reviewedText`, disables `Принять в главу`.
- Muted `Проверьте текст. Перед записью запустите проверку канона и стиля.`
- `Закрыть`; `Проверить`: `POST /chapters/:id/ai/task { kind:'audit', input: submitted, scene_type }`.
  If the textarea is unchanged when the response arrives, store `reviewedText`, set
  `work.audited = true`, enable accept. Always show
  `showText('ИИ-проверка черновика — проверьте замечания самостоятельно', audit.text)`.
- `Принять в главу`: refuses when `reviewedText !== area.value` with
  `Текст изменился после проверки. Запустите её снова.`; confirm
  `Записать черновик в главу? Предыдущая версия сохранится.`; then
  `PUT /chapters/:id/text { body: area.value, expected_revision: work.revision, reason: 'черновик ИИ' }`;
  refetch chapters; close; toast `Черновик записан; проверьте факты главы`; `state.openEditor(id)`.

`addStyleExample()`: prompt `Тип сцены для образца` (default `общий`), then prompt
`Одобренный автором фрагмент` (multiline, ok `Сохранить образец`); cancel at either step aborts;
`POST /books/:id/style-examples { scene_type, body, source_chapter_id: selectedChapter() }`; toast
`Образец сохранён и будет доступен помощнику`. No list/delete UI.

#### 3.7.10 Input keyboard

`Ctrl/Cmd+Enter` in the compose textarea sends (preventDefault). Plain Enter inserts a newline.
There is no Shift+Enter distinction. Send button is also clickable.

---

### 3.8 Import / export UI

- The only import UI is the header `Импорт` button and the empty-state `Импортировать книги`
  button. Both call `runImport()`, which is equivalent to `npm run import` with no arguments:
  `POST /api/import-all`.
- During the request the button is disabled and labelled `Импортирую…`; original label restored in
  `finally`.
- Result: array `books` each `{title, stats, warnings}`. Toast `Импортировано книг: {n}`;
  `showText('Импорт из markdown', lines.join('\n\n'))` where each book is:
  `"{title}\n  {stats entries joined by '  ·  '}"` plus, when warnings exist,
  `"\n  ⚠ предупреждений: N\n  " + first 20 warnings joined by "\n  "`.
- Then `boot()` re-fetches book/chapters/characters and re-renders the currently active nav view
  (falls back to `Граф` if no nav button is marked `.on`).
- Failure: toast `Ошибка импорта: {message}`.
- **There is no export UI at all.** Export is CLI-only (`npm run export`), as is single-book import
  (`POST /api/import` is never called by the frontend), report, scaletest, evaluate and MCP stdio.
- There is no book switcher, no “new book”, no rename/delete book, no settings for `min_chars`,
  style profile, POV, genre or target length.

---

## 4. Client state model

### 4.1 In-memory state

#### Global (`app.js`)

| Field | Type | Lifetime | Notes |
|---|---|---|---|
| `state.book` | object | page session | First book; replaced only by `boot()` (import) |
| `state.chapters` | array | page session | Refreshed by boot, editor save/resolve, graph chapter creation/paste/reload, AI actions/draft, codex create/save/delete |
| `state.characters` | array | page session | Refreshed by boot, graph paste, AI actions, codex create/save/delete |
| `state.currentChapter` | number/null | page session | Set by `show(name, chapterId)`, editor `open`, AI chapter change |
| `state.openEditor(id)` | function | module constant | Calls `show('Текст', id)` |
| `state.beforeLeave` | `() => Promise<boolean>`/null | set by editor, cleared by `show`/`runImport` after a passing guard | Single-slot navigation guard |
| `state.editorDirty` | `() => boolean`/null | set by editor, cleared by `show`/`runImport` | Feeds `beforeunload` |

Note: `show()` calls `state.beforeLeave()` and then nulls both hooks; `runImport()` does the same.
If a view other than the editor sets `beforeLeave`, it works identically. Because only one slot
exists, two simultaneous guards would overwrite each other (not currently possible).

#### Graph-local (`graphView` closure)

| Field | Meaning | Persistence |
|---|---|---|
| `graph` | `{nodes, edges}` from `GET /graph` | Server; mutated locally on edits |
| `view` | `{x,y,k}` viewport | Not persisted; reset to `{40,40,1}` then `fit()` on each mount |
| `selection` | `Set<nodeId>` | Not persisted; cleared on remount |
| `linking` | `{from, ghost}` while drawing | transient |
| `spaceHeld` | Space-pan modifier | transient |
| `clipboard` (module scope, outside view) | array of copied node descriptors | Survives view remount, not reload |

#### Editor-local (`editorView` closure)

| Field | Meaning | Persistence |
|---|---|---|
| `chapter` | loaded chapter (`id, number, title, body, revision, …`) | server |
| `notes` | annotation array | server |
| `dirty` | unsaved changes | memory + mirrored into draft on autosave |
| `saving` | request in flight | memory |
| `draftTimer` | 700 ms debounce handle | memory |
| `showDone` | whether done notes are expanded | memory, resets on mount |
| `chapter.body` | last saved markdown snapshot | used as “current” in edit-review pane |

#### Codex-local

`current` (full character + `state`), `dict` (dictionaries, fetched once per mount), `atChapter`
(selected chapter number), plus `state.characters`.

#### State-viewer-local

`moments`, `idx`, `data` (last slice), `changes` (last diff), `filter`, `scope` (default `scene`).
All reset on mount. `filter` and `scope` do not survive view switches.

#### AI caches (stored on shared `state`)

| Field | Meaning |
|---|---|
| `state.aiChapter` | selected chapter id or null (`Вся книга`) |
| `state.aiChats` | `{ [bookId:chapterId]: messages[] }` |
| `state.aiLoaded` | `{ [key]: true }` history loaded |
| `state.aiHasMore` | `{ [key]: boolean }` |
| `state.aiPending` | `{ [key]: {started, mode} }` while a request runs |
| `state.aiWorkspace` | `{ [chapterId]: {plan, draft, revision, audited} }` session-only drafts/plans |

These survive tab switches but not reload. Server-side chat history is the durable copy; drafts and
plans in `aiWorkspace` are **not** persisted anywhere and are lost on reload.

### 4.2 LocalStorage keys

| Key | Value | Written when | Removed when |
|---|---|---|---|
| `storyforge:draft:<bookId>:<chapterId>` | `{body, base_revision, saved_at}` JSON | 700 ms after editor input; before leaving; before chapter switch; save failure | successful clean save; user declines restore; malformed JSON |
| `storyforge:ai-chapter:<bookId>` | `'all'` or numeric chapter id string | AI chapter select change | never (overwritten) |
| `storyforge:ai-mode:<bookId>` | `'agent'` or `'chat'` | AI mode change / `syncMode()` | never |

All `localStorage` access is wrapped in try/catch (disabled storage tolerated); draft write failure
toasts. There is no `sessionStorage`, IndexedDB, cookie or Cache API use. No service worker, no
offline mode.

### 4.3 What is persisted to the server, and when

| Data | Server action | Trigger |
|---|---|---|
| Chapter markdown | `PUT /chapters/:id/text` | explicit save, version restore, apply-edits, AI draft accept |
| Chapter metadata | `PATCH /chapters/:id` | graph property panel blur/change; paste copy |
| Event metadata | `PATCH /events/:id` | graph property panel; hidden toggle; multi-event move/hide |
| Plan node data | `PATCH /nodes/:id {data}` | details panel blur |
| Node positions | `POST /nodes/move` | drag release; align/distribute |
| Nodes | `POST /books/:id/nodes`, `DELETE /nodes/:id` | create, paste, delete-from-canvas |
| Edges | `POST /books/:id/edges`, `DELETE /edges/:id` | draw, type toggle, delete, unlink |
| Event participants | `POST/DELETE /events/:id/characters[/:cid]` | details panel |
| Annotations | `POST/PATCH/DELETE /annotations[/:id]` | create, note blur, status, delete |
| Character | `POST/PATCH/DELETE /characters[/:id]` | create, field blur, delete |
| Character states | `POST /characters/:id/states`, `DELETE /character-states/:id` | quick fact add/delete |
| Facts | `POST /books/:id/facts`, `DELETE /facts/:id` | codex add, state extraction, dossier delete |
| Knowledge | `POST /books/:id/knowledge` | state extraction |
| Relationships | `POST /books/:id/relationships`, `DELETE /relationships/:id` | codex add/delete |
| AI settings | `PUT /ai/settings` | explicit save/delete key |
| AI chat | `POST /books/:id/ai/chat` or `/agent` (server logs run + returns) | send |
| AI drafts | `PUT /chapters/:id/text` | accept draft |
| Style examples | `POST /books/:id/style-examples` | explicit add |
| Import | `POST /import-all` | header/empty-state import |

Not persisted: graph viewport/selection/clipboard, state-viewer filter/scope/moment (recomputed),
Info tab (always `Ружья`), AI draft/plan workspace, note `_lost` flags, done-notes expansion,
modal state.

### 4.4 Debouncing and autosave

- Editor draft: **700 ms** trailing debounce per input; plus immediate save on navigation/leave and
  on save failure. Not a server autosave.
- Character/codex and graph property edits: commit on `blur` (and on `change` for selects and
  checkboxes); no debounce, no batching.
- Graph node movement: one batched `POST /nodes/move` at mouseup; no intermediate requests.
- State-view slider: **no debounce**, one request pair per `input` event.
- AI elapsed ticker: 1 s interval while the panel is mounted.
- Toast lifetime: 2600 ms; copy-button feedback 1600 ms; note flash 900 ms; toast pop 200 ms;
  context menu 90 ms.
- No polling anywhere; no SSE/WebSocket; activity/history refresh is manual.

### 4.5 Undo stacks and recovery

- **Editor**: only the browser’s native contenteditable undo/redo (`execCommand('undo'/'redo')`
  buttons, `Ctrl+Z`/`Ctrl+Shift+Z` browser default). No custom history, no coalescing, no
  programmatic snapshot before formatting. Switching chapters discards the native undo stack.
- **Notes**: `Escape` restores the last saved note text (one field, one level); `done` status can be
  toggled back with `Вернуть`; deleting a note is immediate and unrecoverable via UI.
- **Chapter text**: server version history is the only recovery. A version is created by the server
  on every `PUT`; the Versions modal lists and restores them with revision checking. AI apply-edits
  and draft acceptance also create versions, so those are recoverable.
- **Graph**: no undo. Deletions are not confirmed for edges, are confirmed for nodes, and node
  deletion keeps the underlying entity. Positions have no history.
- **Codex/state/info**: no undo; deletes are immediate (facts, states, relationships) or confirmed
  (character). There is no fact-close or knowledge-edit UI.
- **Drafts**: local draft is the safety net for unsaved chapter text; there is no draft UI to list
  or discard all drafts.
- **AI settings**: no history; saving overwrites.

Revision-based concurrency (optimistic locking) is used for chapter text (`expected_revision` on
save, version restore, apply-edits, draft accept, AI commit-facts uses the extraction revision).
A mismatch surfaces as a toast with the server message and does not merge; the user must reopen the
chapter / re-run the preview.

---

## 5. Every fetch call the frontend makes

`lib.js` wraps all calls:

```js
fetch('/api' + path, {
  method,
  headers: body ? {'Content-Type':'application/json'} : undefined,
  body: body ? JSON.stringify(body) : undefined,
})
```

It parses JSON with `.catch(() => null)`; non-OK throws `Error(data?.error || 'Ошибка <status>')`.
`api.get/post/patch/del` map to GET/POST/PATCH/DELETE. Paths below include the `/api` prefix that
`req` adds; all are same-origin, no auth, no custom headers, no abort signal, no retry.

### 5.1 `app.js`

| # | Caller | Request | Payload | Response use |
|---|---|---|---|---|
| 1 | `boot` | `GET /api/books` | — | Array; `[0]` becomes the active book; emptiness drives empty state |
| 2 | `boot` | `GET /api/books/:id` | — | `state.book`; uses `title`, `written.n`, `written.chars`, `min_chars` (editor), `id` |
| 3 | `boot` | `GET /api/books/:id/chapters` | — | `state.chapters`; list fields `id, number, title, status, chars_no_space, open_notes` |
| 4 | `boot` | `GET /api/books/:id/characters` | — | `state.characters`; `id, name, is_main, state_count` |
| 5 | `runImport` | `POST /api/import-all` | none (no body, no Content-Type) | `{books:[{title,stats,warnings}]}` → toast + result modal; then `boot()` |

### 5.2 `graph.js`

| # | Caller | Request | Payload | Response use |
|---|---|---|---|---|
| 6 | mount IIFE, `reload` | `GET /api/books/:id/graph` | — | `{nodes:[{id,type,ref_table,ref_id,x,y,data,entity}], edges:[{id,from_node,to_node,kind}]}` |
| 7 | `reload`, `addNode` (chapter) | `GET /api/books/:id/chapters` | — | replaces `state.chapters` (details chapter select, editor list) |
| 8 | `startDrag` mouseup, `persistPositions` | `POST /api/nodes/move` | `{items:[{id,x,y}]}` | `{ok,moved}` ignored |
| 9 | `startLink`, edge-type toggle | `POST /api/books/:id/edges` | `{from_node,to_node,kind: flow/data}` | edge row appended to `graph.edges`; 409 duplicate → toast |
| 10 | edge click, type toggle, `unlink` | `DELETE /api/edges/:id` | — | `{changed}` ignored; local filter on success |
| 11 | `addNode`, `pasteNode` | `POST /api/books/:id/nodes` | `{type,x,y,data?}`; paste adds only these | created node incl. `ref_table`,`ref_id`,`entity`; pushed/selected |
| 12 | `pasteNode` (event copy), hidden toggle, multi-event ops | `PATCH /api/events/:id` | `{title,summary,hidden,chapter_id}` or subsets `{hidden}`, `{chapter_id}` | updated event merged into `n.entity` (paste) or full `reload()` |
| 13 | `pasteNode` (chapter copy), details save, multi status | `PATCH /api/chapters/:id` | `{title,plan,pov}` / `{number,title,pov,status,plan,summary}` / `{status}` | updated chapter merged or `render()` |
| 14 | plan details | `PATCH /api/nodes/:id` | `{data:{title,text}}` | updated node ignored except local `n.data` set before call |
| 15 | `removeNodes` | `DELETE /api/nodes/:id` | — | `{changed}` ignored; node + incident edges removed locally |
| 16 | participant chip | `DELETE /api/events/:id/characters/:cid` | — | `{ok}` ignored; local participant removal |
| 17 | participant add | `POST /api/events/:id/characters` | `{character_id}` | `{ok}` ignored; local participant append |

### 5.3 `editor.js`

| # | Caller | Request | Payload | Response use |
|---|---|---|---|---|
| 18 | `open` | `GET /api/chapters/:id/text` | — | chapter object + `body`, `revision`; drives editor |
| 19 | `open` | `GET /api/chapters/:id/annotations` | — | annotation array (`id,quote,prefix,suffix,start_pos,end_pos,note,status,…`) |
| 20 | `save` | `PUT /api/chapters/:id/text` | `{body: markdown, expected_revision: number}` | `{revision, chars_no_space}`; updates chapter state and toast count |
| 21 | `save`, `resolve` | `GET /api/books/:id/chapters` | — | refresh list counts (`open_notes`, chars) |
| 22 | `showVersions` | `GET /api/chapters/:id/versions` | — | `[{id,reason,created_at,chars}]` |
| 23 | version button | `GET /api/chapters/:id/versions/:version` | — | `{body}` into preview |
| 24 | restore | `POST /api/chapters/:id/versions/:version/restore` | `{expected_revision}` | saved result; then `open()` |
| 25 | `addNote` | `POST /api/chapters/:id/annotations` | `{quote,prefix,suffix,start_pos,end_pos,note:''}` | created annotation (id needed for card/highlight) |
| 26 | `resolve` | `PATCH /api/annotations/:id` | `{status: 'применена'/'открыта'}` | updated row ignored; local status set |
| 27 | note blur, delete `×` | `PATCH /api/annotations/:id` / `DELETE /api/annotations/:id` | `{note}` / — | updated row ignored; local mutation |
| 28 | `buildAiPrompt` | `GET /api/chapters/:id/ai-context` | — | `{chapter:{id,number,title,pov,summary,chars_no_space,min_chars}, book:{title,style}, text, notes:[{id,note,quote,found,unique,occurrences,paragraph,…}]}` |
| 29 | `applyAiEdits` (preview), `reviewAiEdits` re-preview | `POST /api/chapters/:id/apply-edits` | `{edits:[…], dry_run:true}` | `{revision,preview,applied:[{annotation_id,old,new}],rejected:[{reason,…}]}` |
| 30 | `applyAiEdits` commit | `POST /api/chapters/:id/apply-edits` | `{edits:selected, expected_revision:plan.revision}` | `{applied,rejected,chars_no_space,revision}`; reopen chapter; toasts |
| 31 | `suggestAiEdits` | `POST /api/chapters/:id/ai/suggest-edits` | `{}` | same plan shape; then review + commit |

### 5.4 `codex.js`

| # | Caller | Request | Payload | Response use |
|---|---|---|---|---|
| 32 | `open` | `GET /api/characters/:id` | — | character + `states`, `relationships`, `incoming`, `events`, `secrets` |
| 33 | `open` (once per mount) | `GET /api/dictionaries` | — | `{tree,fact_kinds,stances}`; fact-kind select, `key_hint`, titles |
| 34 | `open`, dossier chapter change | `GET /api/characters/:id/state` | query `chapter=<number>` optional | `{at:{label}, branches:[{title,kinds:[{title,items:[…]}]}], awareness:{knows,suspects,misbeliefs,unaware}}` |
| 35 | `save` | `PATCH /api/characters/:id` | any subset of `{name,status,age,gender,group_name,is_main,summary,character_history,voice,voice_rules,atmosphere_desc,atmosphere_level,knows}` | updated character merged; refetch list |
| 36 | create/save/delete | `GET /api/books/:id/characters` | — | refresh `state.characters` |
| 37 | `+ Персонаж` | `POST /api/books/:id/characters` | `{name}` | created character (id) |
| 38 | key-event `+ Факт` | `POST /api/characters/:id/states` | `{text, chapter_no, hidden}` | row ignored; reopen |
| 39 | state `×` | `DELETE /api/character-states/:id` | — | ignored; reopen |
| 40 | dossier `+ Факт` | `POST /api/books/:id/facts` | `{subject_type:'character', subject_id, kind, key, value, from_chapter}` | ignored; reopen |
| 41 | dossier fact `×` | `DELETE /api/facts/:id` | — | ignored; reopen |
| 42 | add relationship | `POST /api/books/:id/relationships` | `{from_char, to_char, current_state}` | ignored; reopen |
| 43 | relationship `×` | `DELETE /api/relationships/:id` | — | ignored; reopen |
| 44 | delete character | `DELETE /api/characters/:id` | — | ignored; clear current; refetch list |

### 5.5 `statev.js`

| # | Caller | Request | Payload | Response use |
|---|---|---|---|---|
| 45 | mount | `GET /api/books/:id/moments` | — | `[{id,seq,label,world,chapters,entries}]`; slider bounds and default |
| 46 | `load` | `GET /api/books/:id/state` | query `moment=<id>` and optional `characters=<ids>` | `{totals:{facts_in_slice,facts_total},characters:[…],relationships:[…],plot:[…],open_promises:[…]}` |
| 47 | `load` | `GET /api/books/:id/diff` | query `from_moment`, `to_moment` | `{appeared,learned,ended}` for “Изменилось здесь” |
| 48 | `scopeCharacters` (`scene`) | `GET /api/books/:id/events` | — | events with `chapter_no` and `participants` string, used to resolve POV-scene cast |
| 49 | `extractFacts` | `GET /api/chapters/:id/text` | — | chapter `body` for prompt |
| 50 | `extractFacts` | `GET /api/books/:id/state` | query `chapter=<number-1>` | “state before chapter” (`characters[].facts/knows`) |
| 51 | `extractFacts` | `POST /api/books/:id/facts` | `{subject_type:'character', subject_id, kind, value, from_chapter, source:'ИИ'}` per fact | ignored |
| 52 | `extractFacts` | `POST /api/books/:id/knowledge` | `{character_id, secret_id, topic, stance, belief, from_chapter, source:'ИИ'}` per knowledge | ignored |

Note: `closed` facts returned by the model are not sent to `/facts/:id/close`; that route is unused
by the UI.

### 5.6 `info.js`

| # | Tab / caller | Request | Response use |
|---|---|---|---|
| 53 | `promises` | `GET /api/books/:id/promises` | table rows `ext_no,title,planted_ch,expected_payoff,status` |
| 54 | `secrets` | `GET /api/books/:id/secrets` | `[{title,knowledge:[{who,stance}],reader_knows,reveal_plan}]` |
| 55 | `timeline` | `GET /api/books/:id/timeline` | `[{chapter_no,story_date,description,hidden,who}]` |
| 56 | `world` | `GET /api/books/:id/world` | `{rules,locations,glossary,hidden}` |
| 57 | `events` | `GET /api/books/:id/events` | `[{chapter_no,title,summary,participants,status,hidden}]` |

### 5.7 `activity.js`

| # | Caller | Request | Response use |
|---|---|---|---|
| 58 | `load` | `GET /api/books/:id/activity` | `[{tool,summary,created_at}]`; no query (server default 50) |

### 5.8 `ai.js`

| # | Caller | Request | Payload | Response use |
|---|---|---|---|---|
| 59 | `loadSettings` | `GET /api/ai/settings` | — | `{provider,model,has_key,profiles}` → selects/status/panel open |
| 60 | `saveSettings` | `PUT /api/ai/settings` | `{provider, model, api_key}` | public config; clears key field; toast |
| 61 | `removeKey` | `PUT /api/ai/settings` | `{provider, model, clear_key:true}` | public config; clears field; toast |
| 62 | `testConnection` | `POST /api/ai/test` | `{}` | `{ok,model,answer}`; status line + toast |
| 63 | `loadHistory` | `GET /api/books/:id/ai/history` | query `chapter_id` optional | `{messages:[{role,content,actions?,retry?}], has_more}` |
| 64 | `send` (agent mode) | `POST /api/books/:id/ai/agent` | `{chapter_id, message, history:[{role,content}]}` | `{text, actions:[{label,entity_type,id}], …}` or `{error,actions,steps}` |
| 65 | `send` (chat mode) | `POST /api/books/:id/ai/chat` | same | `{text, model, tokens_in, tokens_out, finish_reason?}` |
| 66 | `showContext` | `GET /api/books/:id/scenes/:chapter/context` | — | `{chapter:{number}, world_truth, pov_knowledge:{character, knows, suspects, misbeliefs, unaware}, reader_knowledge_tracked, reader_knowledge, offscreen_for_author}` |
| 67 | `searchText` | `GET /api/books/:id/text-search` | query `q` | `{chapters:[{number,title,snippet}], offscreen:[{chapter_no,offscreen}]}` |
| 68 | `extractFacts` | `POST /api/chapters/:id/ai/extract` | `{}` | `{accepted:[{type,value,topic,source_quote}], rejected:[{reason}], revision}` |
| 69 | commit facts | `POST /api/chapters/:id/ai/commit-facts` | `{expected_revision, proposals}` | `{inserted}` |
| 70 | `runTask` | `POST /api/chapters/:id/ai/task` | `{kind: plan/draft/audit/style_review, input, scene_type}` | `{text, model, revision}` |
| 71 | accept draft | `PUT /api/chapters/:id/text` | `{body, expected_revision: work.revision, reason:'черновик ИИ'}` | save result; refetch chapters; open editor |
| 72 | `addStyleExample` | `POST /api/books/:id/style-examples` | `{scene_type, body, source_chapter_id}` | row ignored; toast |
| 73 | after actions / draft accept | `GET /api/books/:id/chapters` | — | refresh `state.chapters` and chapter select |
| 74 | after actions | `GET /api/books/:id/characters` | — | refresh `state.characters` |

Unused by the UI but present on the server: `POST /api/import`, `GET/POST /api/books/:id/facts`
(GET), `PATCH /api/facts/:id`, `POST /api/facts/:id/close`, `PATCH /api/knowledge/:id`,
`GET /api/books/:id/knowledge`, `GET /api/books/:id/facts/search`, `GET /api/books/:id/relationships`,
`POST /api/books/:id/chapters`, `POST /api/books/:id/events`, `DELETE /api/chapters/:id`,
`DELETE /api/events/:id`, promise creation/editing/deletion, style-example listing/deletion,
`GET /api/books/:id/style-examples`, `POST /api/import`. This is why the UI cannot manage promises,
secrets, world rules, locations, glossary, timeline, knowledge directly, or delete chapters/events.

---

## 6. Keyboard shortcuts, gestures and platform behaviour

### 6.1 Keyboard

#### Graph (document listener while `#graph` is connected; ignored while an input/textarea/select/contenteditable is focused)

| Key | Action |
|---|---|
| `Space` (hold) | Pan mode; keyup exits |
| `Escape` | Clear selection; close context menu |
| `Delete` | Delete selected nodes (with confirm) |
| `Ctrl/Cmd+A` | Select all nodes |
| `Ctrl/Cmd+C` | Copy selected |
| `Ctrl/Cmd+D` | Duplicate selected with +40/+40 offset |
| `Ctrl/Cmd+V` | Paste clipboard at canvas centre/third |
| `Ctrl/Cmd+=`, `Ctrl/Cmd++` | Zoom in |
| `Ctrl/Cmd+-` | Zoom out |
| `Ctrl/Cmd+0` | Reset zoom to 100 % (pan unchanged) |

#### Editor (`#prose` keydown only)

| Key | Action |
|---|---|
| `Ctrl/Cmd+S` | Save chapter |
| `Ctrl/Cmd+B` | Bold |
| `Ctrl/Cmd+I` | Italic |
| `Ctrl/Cmd+Alt+M` | Create note from selection |
| `Ctrl/Cmd+Z` / `Ctrl/Cmd+Shift+Z` | Native contenteditable undo/redo |
| Enter | New paragraph (browser default) |
| Paste | Plain text only |

#### Note field (`.n-edit`)

| Key | Action |
|---|---|
| `Escape` | Revert to last saved text and blur |
| `Ctrl/Cmd+Enter` | Blur = commit |
| Enter | Newline in note text (does not submit) |

#### AI compose

| Key | Action |
|---|---|
| `Ctrl/Cmd+Enter` | Send |
| Enter | Newline |

#### Modals / prompts

| Key | Action |
|---|---|
| `Enter` | Submit `prompt` if single-line; Ctrl+Enter if multiline |
| `Escape` | Cancel `prompt` only |
| Backdrop click | Cancel `prompt`, `showText`, `pasteBox`, `confirm`, new-fact modal |
| Enter in `editable` (single-line) | Blur/commit |
| Ctrl+Enter in `editable` (multiline) | Blur/commit |

`showText`, `pasteBox`, `confirm`, versions, edit-review, draft, scene-context and fact-proposal
modals do **not** handle Escape; only explicit buttons (and, where implemented, backdrop click)
close them. No modal traps focus; Tab can move focus behind the overlay. No modal restores focus on
close.

#### Global / browser

- No shortcut for switching views, opening settings, searching the book, saving AI settings or
  closing the app.
- `Ctrl+S` is intercepted only inside `#prose`; elsewhere it invokes the browser's save-page dialog.
- `beforeunload` prompts only when the editor reports dirty; the browser controls the message text.
- Browser zoom (Ctrl+wheel outside the graph canvas) is not intercepted.

### 6.2 Pointer gestures

| Gesture | Scope | Behaviour |
|---|---|---|
| Left click | node | Select (replace or toggle with Ctrl/Cmd/Shift); begin drag |
| Left drag | node | Move selection; persist on release |
| Left drag | empty canvas | Marquee selection after 4 px |
| Left click | empty canvas | Clear selection |
| Left drag | `.port.out` | Draw edge ghost; drop on node to create |
| Left click | edge path | Confirm and delete edge |
| Right click | canvas/node/edge | Context menu |
| Middle drag | empty canvas | Pan |
| Space + left drag | empty canvas | Pan |
| Wheel / two-finger scroll | canvas | Pan (Shift = horizontal) |
| Ctrl/Cmd + wheel / pinch | canvas | Zoom at cursor |
| Single click | nav / buttons | Standard |

No double-click action anywhere (double-clicking a node does nothing; double-clicking a chapter row
reopens it). No hover previews, tooltips are native `title` only. No drag-and-drop of files. No
edge reconnection by dragging endpoints (only delete/recreate). No multi-touch.

### 6.3 Platform-specific behaviour

- **Modifier**: every shortcut uses `e.ctrlKey || e.metaKey`, so Cmd works on macOS. No
  `Alt`/`Option`-specific bindings beyond `Ctrl+Alt+M` (on macOS this is Ctrl+Option+M, not
  Cmd+Option+M).
- **Trackpad**: two-finger scroll and pinch rely on browser-generated wheel events; `preventDefault`
  on wheel disables page scroll and browser zoom over the canvas.
- **Clipboard**: `navigator.clipboard.writeText` first, then fallback `area.select();
  document.execCommand('copy')`. Requires a user gesture/secure context; failure is silent for
  `showText` (label still flips) and toasted for AI copy.
- **Local storage**: wrapped in try/catch for disabled storage/private mode; drafts fail with a
  toast, AI preferences silently.
- **Dates**: `toLocaleString('ru-RU', …)` in activity; numbers use `toLocaleString('ru-RU')`
  (`lib.num`). Activity parses SQLite `YYYY-MM-DD HH:MM:SS` as UTC by injecting `T` and `Z`; the
  display is in the browser's local timezone.
- **Language**: `lang="ru"`; all UI copy is Russian; no i18n layer, no pluralization library, no
  locale switch. Server messages are Russian and displayed verbatim.
- **Rendering engine**: depends on `document.execCommand`, `Selection`/`Range`,
  `queryCommandState`, `contenteditable`, `scrollIntoView({behavior:'smooth'})`, `requestAnimationFrame`
  and CSS `accent-color`. Behaviour differs across Chromium/WebKit/WebView2; Firefox supports the
  used commands but formatting/undo details can differ.
- **No native integration**: no file system access, no dialogs, no tray, no menu, no notifications,
  no deep links. Import is server-side directory scanning, not a file picker.
- **No offline/PWA**, no install prompt, no service worker.

---

## 7. Visual design notes

### 7.1 Design tokens (`:root`)

| Variable | Value | Used for |
|---|---|---|
| `--bg` | `#14161a` | Page background, canvas, inputs, prose focus background |
| `--panel` | `#1b1e24` | Header, panels, nodes, modals, cards |
| `--panel2` | `#21252c` | Buttons, hover rows, chips, notes, toast |
| `--line` | `#2a2f38` | 1 px borders/dividers everywhere |
| `--fg` | `#e6e8ec` | Primary text |
| `--dim` | `#8b93a1` | Secondary text, metadata, placeholders |
| `--accent` | `#7aa2f7` | Selection, focus, links/buttons hover, active slider, edge flow |
| `--accent-dim` | `#3d5a8a` | Active nav, primary button, selected list row, selection ring |
| `--warn` | `#e0af68` | Warnings, low counter, annotation highlight base, note quote border |
| `--ok` | `#9ece6a` | Success/count-enough, written status |
| `--bad` | `#f7768e` | Errors, destructive hover, ended facts |
| `--hidden` | `#bb9af7` | Hidden-line pills and causal (data) edges |
| `--radius` | `8px` | Nodes, modals, menus, cards |

There is **no light theme**, no `prefers-color-scheme`, no high-contrast variant. The palette is a
Tokyo-Night-like dark scheme. Several colours are duplicated outside the variables:
`#7aa2f7`/`#bb9af7` edge strokes in `graph.js`, `#e0af68` annotation highlight, node head tints
`#1e2a3d`, `#26222f`, `#2b2340`, `#2a2620`, badge backgrounds `#2c4020/#3b3320/#402424`, marquee
`#7aa2f722`, toast/menu shadows `#0006/#0008/#000a`.

### 7.2 Typography

- Body: `14px/1.5 -apple-system, "Segoe UI", Roboto, sans-serif`; `body` is the only place font is
  set; all controls inherit via `button, input, textarea, select { font: inherit; color: inherit; }`.
- Header title: 15 px / 600; meta 12 px muted.
- Nav: 14 px, 6/14 padding, muted; hover panel2; active accent-dim with white text.
- Prose: `16px/1.75 Georgia, "Times New Roman", serif`; `p` margin-bottom `1.05em`.
  `h1` 26 px/600, `h2` 20 px/600, `h3` 17 px/600, `strong` 700 and `#fff`, `em` italic,
  `hr` 1 px line, 40 % width, centred, 1.8em margins.
- Notes card text 13 px/1.45; quote 12 px italic with 2 px warn left border.
- Section headings (`h4`, `.details h3`, `.notes-head`, `.dos-head`, `.block h4`) are 11–13 px,
  uppercase, `letter-spacing:.05em`, muted, weight 500–600.
- Monospace only for `.modal.wide textarea` (`ui-monospace, Consolas, monospace`, 12/1.5).
- Counters use `font-variant-numeric: tabular-nums`; zoom level too.

### 7.3 Layout regions and fixed dimensions

| Region | Size / layout |
|---|---|
| Header | full width, `padding:10px 18px`, bottom 1 px line, flex row, gap 16, `flex-shrink:0` |
| `#view` | fills remaining height, `position:relative`, `overflow:hidden` |
| Graph canvas | absolute inset 0; dotted grid `radial-gradient(circle,#22262e 1px,transparent 1px) 0 0/24px 24px` over `--bg`; `.toolbar` top 12 left 12; `.hint-bar` bottom 12 left 12 |
| Graph details | fixed `320px`, left border, scrolls, padding 14 |
| Graph node | `250px` wide, auto height, radius 8, border line, shadow `0 4px 14px #0006`; header 7/10, body 8/10, footer 6/10 |
| Ports | 13 px circles, 2 px border, positioned `top:14px`, `left:-7px` / `right:-7px` |
| Editor list | `210px`, right border, scrolls |
| Editor doc | flexible; `.doc-bar` 8/14; `.fmt-bar` 6/14; `.doc-wrap` padding `26px 20px 60px`, scrolls, centres |
| Prose | max-width `760px`, min-height 100 % |
| Notes | `300px`, left border, scrolls, padding 12 |
| Codex list | `230px`, right border, scrolls |
| Codex body | flexible, padding `20px 24px`, scrolls |
| State | column: `.time-bar` fixed, `.state-body` scrolls `16px 20px 50px` |
| State cards | grid auto-fill `minmax(300px,1fr)`, gap 10 |
| AI | column: top bar, hint, settings (`max-height:55vh`), actions, messages (`flex:1; min-height:0; overflow:auto`), compose; message/card max-width 850 px |
| Modals | `440px` (max 92vw), wide `780px`; overlay `#000a`; centred |
| Context menu | min-width `210px`, padding 5, shadow, 8 px viewport clamp |
| Toast | fixed bottom 18, centred, z-index 200 |

### 7.4 Component look

- Buttons: panel2 fill, 1 px line border, 6 px radius, 5×12 padding; hover border accent;
  primary uses accent-dim fill + accent border; danger only changes border/text on hover; disabled
  opacity .4.
- Pills: 999 px radius, 1 px border, 12 px; `ok` green with `#38512a` border, `warn` amber with
  `#5a4522`, `plan` muted, `hid` violet with `#4a3a6b`.
- Inputs: `--bg` fill, line border, 6 px radius, 6×9 padding, full width; focus border accent,
  outline removed; password same.
- Cards: panel fill, line border, radius 8; `.note` panel2; `.state-card` panel; `.dos-branch`
  panel border with a panel-coloured `.dos-head`.
- Graph selection: `.node.sel` accent border plus `0 0 0 2px accent-dim` ring and stronger shadow.
- Annotation highlight: `#e0af6840` background, 2 px `#e0af68` bottom border, 2 px radius, pointer
  cursor; hover `#e0af6866`; `.flash` animates background to `#e0af68cc` at 35 % over 0.9 s.
- Marquee: 1 px accent border, `#7aa2f722` fill, 2 px radius, `pointer-events:none`, z-index 5.
- Toast: panel2, accent border, radius 6, `pop .2s ease` (opacity + 8 px rise); error border bad.
- Context menu: fixed panel, shadow `0 8px 26px #000a`, `ctxIn .09s ease` scale .97 → 1; items 13 px, icon 16 px
  at .75 opacity, hint 11 px muted; disabled .35.
- AI waiting: accent-dim border, 8 px pulsing dot (scale .7 / opacity .25 at 50 %), animation
  disabled under reduced motion.
- Scrollbars are native and unstyled.

### 7.5 Z-index stack

`.port` 2 < `.marquee` 5 < `.toolbar`/`.hint-bar` 10 < `.modal-bg` 150 < `.toast` 200 < `.ctx-menu`
300. `#view` is `position:relative`, so view content participates in its own stacking context only
if positioned; roots are absolute.

### 7.6 Responsive / motion

- Only one media query: `@media (max-width:850px)` makes `.ai-top` wrap, chapter/select full width,
  connection status max-width 100 %. Every other screen is fixed-width columns and will overflow or
  squeeze on narrow windows (`.doc` has `min-width:0`, so prose shrinks; graph details stays 320 px).
- `@media (prefers-reduced-motion: reduce)` disables only `.ai-pulse`; toast, context-menu,
  annotation flash and smooth scrolling remain.
- No print stylesheet, no touch-specific enlargement, no hover-capability queries.

### 7.7 What would be lost if the CSS were rebuilt

- Exact dark palette/tints and the semantic mapping (chapter/event/hidden/plan header colours,
  flow vs causal edges, annotation amber, warn/ok/bad).
- The 250 px node width coupling with `NODE_W` in JS; changing one without the other breaks edge
  endpoints and marquee hit-testing.
- Prose measure (760 px), Georgia 16/1.75 rhythm, heading scale, paragraph spacing, `hr` width.
- The three-column editor proportions (210 / flexible / 300) and the graph details 320.
- The dotted 24 px canvas grid and its subtle colour, node shadows, the selection ring.
- Annotation highlight/flash, note-card focus/hover states, `.note.done` fade.
- Marquee look, panning/space/selecting/linking cursors.
- Toolbar/hint overlay placement; context-menu viewport clamping (JS also clamps).
- Toast placement/animation, modal widths/overlay opacity, menu shadow/scale animation.
- Focus treatment: only form controls get an accent border; buttons rely on the browser default
  focus ring, which a rebuilt design may remove without adding an accessible replacement.
- Counter states (green/amber) and tabular numerals.
- The AI panel’s max-width 850 reading column and the 55vh settings/context caps.
- `#activity`’s accidental lack of layout (a loose block whose list is clipped by `#view`); a
  rebuild will probably change this, so decide deliberately rather than “fixing” it unknowingly.
- Spacing constants are not tokens: 12/18 header padding, 14/18 settings, 9/12 list rows,
  7/10 node header, 8/10 node body, 26/20/60 doc padding, 20/24 codex body, etc.

---

### 7.8 Accessibility (current state)

- Labels: `editable()` and the hand-built `label.field` wrappers put the `<span>` label and the
  control inside one `<label>`, so most form controls have an implicit accessible name. Icon-only
  buttons (`−`, `+`, `×`, `↶`, `↷`, `‹`, `›`) rely on `title` text; there is no `aria-label`.
- Landmarks/roles: no `role="dialog"`, `aria-modal`, `aria-labelledby` or focus trap on any modal;
  no `aria-current` on nav; the graph canvas is a plain `div` with absolutely positioned,
  non-focusable node `div`s, so it has no keyboard or screen-reader model.
- Live regions: only the AI connection status (`role="status"`) and the AI waiting bubble
  (`role="status" aria-live="polite"`); toasts and errors are not announced.
- Modals: opening does not move focus (except `prompt`/`showText`/`pasteBox`, which focus their
  field) and closing does not restore it; Tab escapes the overlay.
- Colour is supplemented by text (status pills, `⚠`, `+`/`−`/`?` badges), but the yellow note
  highlight is colour-only. `prefers-reduced-motion` covers only the AI pulse.
- The document has `lang="ru"`, a meaningful `<title>`, and semantic `header`/`nav`/`h1`; tables use
  real `th`/`td` but no `scope`; no skip link.

---

## 8. UI things that are hard to port, and why

1. **`contenteditable` + `document.execCommand` + Selection/Range.** The formatting toolbar, undo/
   redo, block format changes, plain-text paste and note highlighting all rely on deprecated
   `execCommand` and live DOM ranges. Rust cannot reproduce this outside a webview; a native editor
   (e.g. a custom text engine) would need its own document model, selection model, IME handling,
   undo stack, and markdown serializer. Keeping the webview preserves behaviour but inherits
   engine-specific quirks (WebView2 vs WebKitGTK vs WKWebView).
2. **Markdown ↔ DOM ↔ flat-projection equivalence for note anchors.** `textMap` deliberately
   projects `**bold**`, `# ` prefixes and blank lines so offsets match the on-disk markdown, while
   `mdToHtml`/`htmlToMd` must round-trip exactly the supported dialect. A tiny difference (an extra
   newline, a marker) breaks every note anchor. The port needs the same three representations and
   tests against real chapters.
3. **Multi-range highlighting.** A note over several paragraphs must be wrapped as one span per
   text node via `Range.surroundContents`, with silent per-fragment failure and a “lost anchor” flag.
   A naive single wrapper destroys block structure or fails; a rich-text model must support
   overlapping/annotated ranges.
4. **The manual AI loop is user-mediated.** Prompt assembly (`ai-context` → text with notes,
   paragraphs, warnings), copy-to-clipboard, paste-JSON, dry-run preview, per-edit checkbox review,
   revision-checked commit, and rejection explanations are a sequence of modal promise chains. The
   backend semantics (exact `old` matching, ambiguity rejection, auto-closing applied notes) must be
   preserved, not replaced by “ask the model and apply”.
5. **Revision-based optimistic concurrency everywhere.** Chapter save, version restore, apply-edits,
   draft accept and AI commit-facts all send `expected_revision`; the UI assumes 409 is possible and
   only shows a toast. A port must keep revisions and conflict surfaces, including the subtle
   “edits made while the save was in flight” re-dirty check.
6. **Local draft autosave with revision-sensitive restore.** The 700 ms debounce, per-book/per-chapter
   key, `base_revision` comparison and two different restore prompts are easy to lose; without them,
   a navigation guard becomes destructive. Browser storage may also be unavailable.
7. **Graph coordinate math and hit-testing.** World/screen conversion for drag, marquee, edge ghost,
   zoom-at-cursor and `fit()`; the fixed 21 px port offset; node height measured from DOM; the 250 px
   width duplicated in CSS and JS. A canvas/WebGL rewrite must reproduce all of these or the feel
   changes. Marquee semantics (4 px threshold, additive union, silent live selection) are part of it.
8. **Clipboard semantics.** Module-level clipboard survives view switches, stores entity snapshots
   plus relative offsets, duplicates by creating a new entity and patching a subset of fields with a
   ` (копия)` suffix. It is not the system clipboard and does not copy edges/participants/status.
9. **No unmount lifecycle.** Document-level keyboard/selection listeners persist across views and
   guard with `root.isConnected`; the AI ticker self-cancels when disconnected. A component-based
   port must add teardown or it will duplicate handlers.
10. **AI chat state is layered.** Server-persisted history, in-memory per-`book:chapter` cache,
    optimistic user message, pending timer, per-request error messages with retry payload, action
    lists that refresh chapters/characters, and a “last 200 requests” notice. Streaming is absent;
    preserving the exact all-at-once UX (and the 1 s elapsed ticker) is a deliberate choice.
11. **A modal/prompt DSL built on Promises.** `prompt`, `confirm`, `showText`, `pasteBox` and
    `contextMenu` return values through closure-resolved promises, with different close rules
    (backdrop/Escape availability). A ported dialog API must match which dialogs can be dismissed
    and what they return, or workflows hang.
12. **Russian strings as enums and server-derived fields.** Statuses, stances, fact kinds, reasons
    and messages cross the wire as Russian text; the UI branches on literals (`написано`,
    `открыта`, `применена`, `знает`, `выстрелило`). Renaming to Rust enums requires a wire mapping;
    the UI also assumes derived fields (`open_notes`, `state_count`, `participants` as a joined
    string, `written.n/chars`, `has_more`, `revision`).
13. **Manual fact extraction with sequential writes.** The state view builds a long prompt, parses
    a three-key JSON, shows an aggregate confirm, then POSTs facts and knowledge one by one with no
    transaction or rollback; a port should decide whether to preserve that partial-failure behaviour
    or improve it visibly.
14. **Fixed single-book assumption.** Boot always selects `books[0]`; there is no book picker. A
    Tauri port that wants multi-book support must add UI, not just plumbing, and the graph/editor/AI
    state keys already include book ids, which helps.
15. **Unstyled/overflowing screens.** Activity has no layout CSS; Info uses `.scroll`; long lists in
    codex/state rely on their own overflow. A rebuilt shell can easily change scroll ownership and
    hide content.

---

## 9. Behavioural quirks and latent bugs (do not “fix” accidentally)

These are implemented behaviours; a parity port should either reproduce them or consciously change
them and record the decision.

1. **No outer blank lines in the manual edit prompt.** `buildAiPrompt` filters out every empty
   string, including the intended separators; only blank lines inside the embedded chapter text
   survive.
2. **`#prose:empty::before` never shows.** Empty markdown becomes `<p><br></p>`, so the
   `Текст главы…` placeholder rule does not match. The prose area is blank instead.
3. **Chapter-node summary always ends with `…`**, even when shorter than 110 characters; event
   summaries only add it when longer than 130.
4. **Drag works anywhere on a node**, not only on the header; the header alone has `cursor:move`.
   Space+drag over a node does not pan because the canvas handler returns early for node targets.
5. **A click on an edge also arms a zero-size marquee**, because the edge path does not stop
   `mousedown`; the subsequent click handler stops propagation and confirms deletion.
6. **Edge type toggle deletes and recreates** the edge, so it gets a new id and can fail halfway
   with no local reconciliation.
7. **Node deletion keeps the chapter/event entity.** “Убрать с холста” is not “delete chapter”, and
   the graph does not refresh `state.chapters`; the editor may still list the entity.
8. **Duplicating a chapter node does not copy status/summary/arc**, and duplicating an event does not
   copy participants. Both append ` (копия)` to the title.
9. **`Ctrl+V` pastes at the canvas centre/third**, not at the last cursor position or pointer.
10. **Graph `Ctrl+0` resets only `k`**, leaving the pan where it was; the empty-menu
    `Сбросить масштаб` does the same. Only `fit()` recentres.
11. **Space on a focused toolbar button** is swallowed to enable pan, so keyboard activation of
    toolbar buttons via Space does not work.
12. **Codex field saves do not refresh the body header**, so an edited name/status looks stale until
    reopening the character. The list is refreshed.
13. **The dossier’s chapter select and the “before chapter” state lookup use chapter numbers**, not
    ids, while the AI chapter select and most API calls use ids. Off-by-one or a missing chapter
    number yields a different state slice.
14. **State slider fires requests on every `input`**, with no debounce or request cancellation;
    dragging quickly can produce out-of-order responses (last response wins visually because
    `render()` uses the module-level `data`).
15. **Manual extraction ignores `closed` facts** and writes facts/knowledge sequentially; a failure
    leaves a partially applied batch with a success-looking flow until the error toast.
16. **Extraction-confirm revision is used for commit-facts, but the extraction prompt/text is
    built client-side**, so the chapter may change between prompt generation and commit; only the
    commit is revision-checked.
17. **AI draft acceptance uses `work.revision` captured at draft time**; an audit or any external
    save in between causes a 409 toast, and the draft remains in the session workspace.
18. **AI `plan` task sends `input: work.draft`** (empty by default) because of the ternary
    `kind === 'draft' ? work.plan : work.draft`; the server ignores it for plan, but a reader of the
    request may be surprised.
19. **AI history caches are keyed by book+chapter**; switching chapter rebuilds the whole AI view
    (`clear(root); aiView(...)`) rather than re-rendering a subsection. Any unsent text in the
    compose box is lost on chapter switch.
20. **AI errors are sometimes HTTP errors and sometimes `{error}` in a 200 response** (agent
    route). The UI handles both, but the retry payload/message shapes differ.
21. **`loadSettings` can leave the settings panel open** when there is no key; after saving a key it
    stays open until the user toggles it.
22. **`provider` change mutates the key-status line by appending**, so repeated changes accumulate
    the “Выбор ещё не применён…” sentence until the next status refresh.
23. **Note text commits only on blur**; an empty note is deleted then. Navigating away while the
    field is focused relies on the blur firing and the async delete completing.
24. **Note `Escape` is a one-level revert** to the last saved value, not an undo of typing.
25. **`showVersions` refuses when dirty but offers no “save and continue”**; the user must save or
    discard manually.
26. **Versions/review/draft/scene-context modals cannot be closed with Escape** and do not trap
    focus; `showText`/`pasteBox`/`confirm` also lack Escape.
27. **Import result is formatted into a plain text modal**, not a structured table; warnings are
    truncated to the first 20.
28. **`beforeunload` fires only when the editor reports dirty**, so unsaved AI settings or a dirty
    note field (before blur) are not guarded.
29. **`show()` clears `state.editorDirty` after leaving the editor**, so the unload guard is removed
    even if a draft remains unsaved (the draft is already in localStorage if the debounce fired).
30. **No book selector, no chapter/event deletion, no promise/secret/world editing, no export UI.**
    These are absences to plan for, not bugs to preserve blindly.
31. **Info always opens on `Ружья`**, and `promises()` is invoked at mount; switching tabs does not
    cache results — every click refetches.
32. **Activity has no CSS** and no auto-refresh; its `created_at` relative time can go stale while
    the tab stays open.
33. **No empty/loading state for graph while `GET /graph` is in flight**; the canvas and details are
    blank until `render()`.
34. **`state.beforeLeave` is single-slot and cleared by `show()` after invocation**; if a future view
    sets its own guard while the editor is mounted, one overwrites the other.

---

## 10. Rebuild checklist (condensed)

- Reproduce the seven nav views in this order and their exact Russian labels.
- Keep `#graph`, `#editor`, `#prose`, `#codex`, `#statev`, `#ai-view`, `#activity` ids and the
  `.node/.details/.doc/.notes/.codex/.state-*/.ai-*` class contract if the existing `style.css` is
  reused or translated.
- Implement the markdown dialect and the flat-text projection before any note/editor feature.
- Treat annotations as quote+context anchored ranges with per-text-node wrapping and a lost-anchor
  state.
- Keep revision checks and local-draft restore prompts; they are the safety net.
- Implement graph pan/zoom/marquee/drag/edge math in one coordinate module; keep 250 px and 21 px
  constants centralized.
- Preserve the manual AI edit loop (prompt → copy → paste → dry-run → checkbox review → revisioned
  commit → rejection report) and the agent/chat distinction.
- Decide explicitly about the quirks in §9 (blank-line filtering, draft/plan workspace loss,
  single-book assumption, missing CRUD screens) rather than inheriting them by accident.

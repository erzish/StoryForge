/**
 * Редактор главы: форматированный текст, как в текстовом процессоре.
 * На диске остаётся markdown — при загрузке он превращается в размеченный
 * текст, при сохранении собирается обратно.
 *
 * Заметка вешается на выделенный фрагмент и подсвечивает его жёлтым.
 * Привязка хранится по цитате с окружением, а не по номеру символа,
 * поэтому переживает правки выше по тексту.
 */
import { api, el, clear, toast, confirm, num, showText, pasteBox } from './lib.js';
import {
  mdToHtml, htmlToMd, textMap, rangesForOffsets, selectionOffsets, charsNoSpace, findFlexible,
} from './rich.js';

const CTX = 40; // сколько символов контекста хранить вокруг цитаты

export function editorView(root, state) {
  let chapter = null;
  let notes = [];
  let dirty = false;
  let saving = false;
  let draftTimer;
  let showDone = false;

  const list = el('div.chapters-list');
  const prose = el('div', {
    id: 'prose', contenteditable: 'true', spellcheck: 'false',
  });
  const counter = el('span.counter');
  const saveBtn = el('button.btn.primary', { onclick: save }, 'Сохранить');
  const versionsBtn = el('button.btn', { onclick: showVersions }, 'Версии');
  const noteBtn = el('button.btn', { onclick: addNote, disabled: true }, '+ Заметка');
  const titleEl = el('b');
  const notesPanel = el('div.notes');

  // ── панель форматирования
  const fmtBtn = (label, title, cmd, arg) => el('button.fmt', {
    title,
    'data-cmd': cmd,
    onmousedown: (e) => e.preventDefault(), // не терять выделение
    onclick: () => { document.execCommand(cmd, false, arg); prose.focus(); onEdit(); syncToolbar(); },
  }, label);

  const toolbar = el('div.fmt-bar', {},
    fmtBtn('Ж', 'Жирный — Ctrl+B', 'bold'),
    fmtBtn('К', 'Курсив — Ctrl+I', 'italic'),
    el('span.fmt-sep'),
    fmtBtn('H1', 'Заголовок главы', 'formatBlock', 'h1'),
    fmtBtn('H2', 'Подзаголовок', 'formatBlock', 'h2'),
    fmtBtn('¶', 'Обычный абзац', 'formatBlock', 'p'),
    el('span.fmt-sep'),
    el('button.fmt', {
      title: 'Разделитель сцены',
      onmousedown: (e) => e.preventDefault(),
      onclick: () => { document.execCommand('insertHorizontalRule'); prose.focus(); onEdit(); },
    }, '—'),
    el('span.fmt-sep'),
    el('button.fmt', {
      title: 'Отменить — Ctrl+Z',
      onmousedown: (e) => e.preventDefault(),
      onclick: () => { document.execCommand('undo'); onEdit(); },
    }, '↶'),
    el('button.fmt', {
      title: 'Повторить — Ctrl+Shift+Z',
      onmousedown: (e) => e.preventDefault(),
      onclick: () => { document.execCommand('redo'); onEdit(); },
    }, '↷'),
    el('span.fmt-sep'),
    noteBtn
  );

  const bar = el('div.doc-bar', {}, titleEl, el('span.grow'), counter, versionsBtn, saveBtn);
  const wrap = el('div.doc-wrap', {}, prose);
  const doc = el('div.doc', {}, bar, toolbar, wrap);

  root.append(el('div', { id: 'editor' }, list, doc, notesPanel));

  try { document.execCommand('defaultParagraphSeparator', false, 'p'); } catch { /* не критично */ }

  const draftKey = (id) => `storyforge:draft:${state.book.id}:${id}`;
  const saveDraft = () => {
    if (!chapter || !dirty) return;
    try { localStorage.setItem(draftKey(chapter.id), JSON.stringify({
      body: htmlToMd(prose), base_revision: chapter.revision, saved_at: Date.now(),
    })); } catch { toast('Не удалось сохранить локальный черновик', true); }
  };
  state.editorDirty = () => dirty;
  state.beforeLeave = async () => {
    if (saving) { toast('Дождитесь завершения сохранения', true); return false; }
    if (!dirty) return true;
    saveDraft();
    return confirm('Есть несохранённый текст. Уйти из редактора? Черновик останется в браузере.', { ok: 'Уйти' });
  };

  // ═══════════ список глав ═══════════

  function renderList() {
    clear(list);
    for (const c of state.chapters) {
      list.append(el('div.item', {
        class: 'item' + (chapter?.id === c.id ? ' on' : ''),
        onclick: () => open(c.id),
      },
        el('b', {}, `${c.number}. ${c.title ?? 'Без названия'}`),
        el('small', {}, c.status === 'написано'
          ? `${num(c.chars_no_space)} зн.${c.open_notes ? ` · ${c.open_notes} зам.` : ''}`
          : 'план')
      ));
    }
  }

  // ═══════════ открытие главы ═══════════

  async function open(id) {
    if (saving) return toast('Дождитесь завершения сохранения', true);
    if (dirty) {
      saveDraft();
      if (!(await confirm('Есть несохранённые правки. Открыть другую главу?', { ok: 'Открыть' }))) return false;
    }
    const loaded = await api.get(`/chapters/${id}/text`);
    notes = await api.get(`/chapters/${id}/annotations`);
    chapter = loaded;
    let body = chapter.body ?? '';
    let restored = false;
    const rawDraft = localStorage.getItem(draftKey(id));
    if (rawDraft) {
      try {
        const draft = JSON.parse(rawDraft);
        if (typeof draft.body === 'string' && draft.body !== body) {
          const changed = draft.base_revision !== chapter.revision;
          restored = await confirm(changed
            ? 'Найден локальный черновик, но глава на диске изменилась. Восстановить черновик для просмотра?'
            : 'Найден несохранённый локальный черновик. Восстановить?', { ok: 'Восстановить' });
          if (restored) body = draft.body;
          else localStorage.removeItem(draftKey(id));
        }
      } catch { localStorage.removeItem(draftKey(id)); }
    }
    prose.innerHTML = mdToHtml(body);
    titleEl.textContent = `${chapter.number}. ${chapter.title ?? ''}`;
    dirty = restored;
    state.currentChapter = id;
    renderList();
    applyHighlights();
    renderNotes();
    updateBar();
    return true;
  }

  // ═══════════ правка ═══════════

  function onEdit() {
    dirty = true;
    updateBar();
    clearTimeout(draftTimer);
    draftTimer = setTimeout(saveDraft, 700);
  }

  prose.addEventListener('input', onEdit);

  prose.addEventListener('keydown', (e) => {
    const mod = e.ctrlKey || e.metaKey;
    if (mod && e.key.toLowerCase() === 's') { e.preventDefault(); save(); }
    if (mod && e.key.toLowerCase() === 'b') { e.preventDefault(); document.execCommand('bold'); onEdit(); }
    if (mod && e.key.toLowerCase() === 'i') { e.preventDefault(); document.execCommand('italic'); onEdit(); }
    // как в текстовых процессорах: примечание к выделенному
    if (mod && e.altKey && e.key.toLowerCase() === 'm') { e.preventDefault(); addNote(); }
  });

  // вставка — только текстом, чтобы не тащить чужое оформление
  prose.addEventListener('paste', (e) => {
    e.preventDefault();
    const text = e.clipboardData.getData('text/plain');
    document.execCommand('insertText', false, text);
  });

  function updateBar() {
    const n = charsNoSpace(textMap(prose).text);
    const min = state.book.min_chars;
    counter.className = 'counter ' + (n >= min ? 'ok' : 'low');
    counter.innerHTML = `<b>${num(n)}</b> <span class="dim">/ ${num(min)} знаков</span>`;
    saveBtn.textContent = dirty ? 'Сохранить •' : 'Сохранить';
    saveBtn.disabled = !dirty || saving;
  }

  async function save() {
    if (!chapter || !dirty || saving) return;
    saving = true;
    updateBar();
    try {
      const md = htmlToMd(prose);
      const r = await api.req('PUT', `/chapters/${chapter.id}/text`, {
        body: md, expected_revision: chapter.revision,
      });
      chapter.revision = r.revision;
      chapter.body = md;
      dirty = htmlToMd(prose) !== md;
      if (dirty) saveDraft();
      else localStorage.removeItem(draftKey(chapter.id));
      updateBar();
      toast(`Сохранено · ${num(r.chars_no_space)} знаков`);
      state.chapters = await api.get(`/books/${state.book.id}/chapters`);
      renderList();
    } catch (e) {
      toast(e.message, true);
      saveDraft();
    } finally {
      saving = false;
      updateBar();
    }
  }

  async function showVersions() {
    if (!chapter) return;
    if (dirty) return toast('Сначала сохраните главу или оставьте локальный черновик', true);
    const rows = await api.get(`/chapters/${chapter.id}/versions`);
    if (!rows.length) return toast('Предыдущих версий пока нет');
    const preview = el('textarea', { rows: 15, readonly: 'readonly' });
    const restore = el('button.btn.danger', { disabled: true }, 'Восстановить выбранную');
    const list = el('div.version-list');
    let selected = null;
    for (const v of rows) list.append(el('button.btn', {
      onclick: async () => {
        selected = v.id;
        const row = await api.get(`/chapters/${chapter.id}/versions/${v.id}`);
        preview.value = row.body;
        restore.disabled = false;
      },
    }, `${v.created_at} · ${v.reason || 'правка'} · ${num(v.chars)} симв.`));
    const bg = el('div.modal-bg', {}, el('div.modal.wide', {},
      el('h3', {}, 'Предыдущие версии главы'), list, preview,
      el('div.row', {}, el('button.btn', { onclick: () => bg.remove() }, 'Закрыть'), restore)));
    restore.addEventListener('click', async () => {
      if (!selected || !(await confirm('Восстановить выбранную версию? Текущая будет сохранена в истории.', { ok: 'Восстановить' }))) return;
      try {
        await api.post(`/chapters/${chapter.id}/versions/${selected}/restore`, {
          expected_revision: chapter.revision,
        });
        bg.remove();
        await open(chapter.id);
        toast('Версия восстановлена');
      } catch (e) { toast(e.message, true); }
    });
    document.body.append(bg);
  }

  // ═══════════ кнопка заметки ═══════════

  function syncToolbar() {
    for (const b of toolbar.querySelectorAll('.fmt[data-cmd]')) {
      const cmd = b.dataset.cmd;
      if (cmd === 'formatBlock') continue;
      try { b.classList.toggle('on', document.queryCommandState(cmd)); } catch { /* ignore */ }
    }
  }

  document.addEventListener('selectionchange', () => {
    if (!root.isConnected) return;
    const sel = selectionOffsets(prose);
    noteBtn.disabled = !sel;
    noteBtn.classList.toggle('primary', !!sel);
    syncToolbar();
  });

  /**
   * Заметка создаётся сразу по выделению — без диалога.
   * Карточка появляется справа с курсором внутри: пиши и уходи,
   * сохранится само. Пустая заметка удаляется, мусор не копится.
   */
  async function addNote() {
    const sel = selectionOffsets(prose);
    if (!sel) return;

    const { text } = textMap(prose);
    const created = await api.post(`/chapters/${chapter.id}/annotations`, {
      quote: sel.text,
      prefix: text.slice(Math.max(0, sel.start - CTX), sel.start),
      suffix: text.slice(sel.end, sel.end + CTX),
      start_pos: sel.start,
      end_pos: sel.end,
      note: '',
    });

    notes.push(created);
    applyHighlights();
    renderNotes();

    const field = notesPanel.querySelector(`[data-note="${created.id}"] .n-edit`);
    if (field) {
      field.scrollIntoView({ block: 'nearest' });
      field.focus();
    }
  }

  // ═══════════ подсветка ═══════════

  const openNotes = () => notes.filter((n) => n.status === 'открыта');

  function unwrapHighlights() {
    for (const span of [...prose.querySelectorAll('.ann')]) {
      const parent = span.parentNode;
      while (span.firstChild) parent.insertBefore(span.firstChild, span);
      span.remove();
    }
    prose.normalize();
  }

  /** Ищем цитату: на прежнем месте → по контексту → по самой цитате. */
  function locate(text, n) {
    if (!n.quote) return null;
    if (text.slice(n.start_pos, n.end_pos) === n.quote) return [n.start_pos, n.end_pos];

    const prefix = n.prefix ?? '';
    const byCtx = findFlexible(text, prefix + n.quote);
    if (byCtx.count > 0) {
      const inner = findFlexible(text.slice(byCtx.start, byCtx.end), n.quote);
      if (inner.count > 0) return [byCtx.start + inner.start, byCtx.start + inner.end];
    }

    const plain = findFlexible(text, n.quote);
    return plain.count > 0 ? [plain.start, plain.end] : null;
  }

  function applyHighlights() {
    unwrapHighlights();

    for (const n of openNotes()) {
      // карту считаем заново на каждой заметке: предыдущая подсветка
      // поделила текстовые узлы, хотя сам текст не изменился
      const { text } = textMap(prose);
      const pos = locate(text, n);
      n._lost = !pos;
      if (!pos) continue;

      // Выделение через несколько абзацев оборачиваем по кусочкам,
      // иначе один общий элемент разрушил бы структуру документа.
      const ranges = rangesForOffsets(prose, pos[0], pos[1]);
      if (!ranges.length) { n._lost = true; continue; }

      let wrapped = 0;
      for (const range of ranges) {
        const span = el('span.ann', { 'data-ann': n.id, title: n.note });
        try { range.surroundContents(span); wrapped++; } catch { /* пропускаем кусок */ }
      }
      if (!wrapped) n._lost = true;
    }
  }

  // Клик по жёлтому фрагменту — сразу курсор в его заметку, можно править
  prose.addEventListener('click', (e) => {
    const span = e.target.closest?.('.ann');
    if (!span) return;
    const card = notesPanel.querySelector(`[data-note="${span.dataset.ann}"]`);
    if (!card) return;
    card.scrollIntoView({ block: 'center', behavior: 'smooth' });
    card.classList.add('flash');
    setTimeout(() => card.classList.remove('flash'), 900);
    card.querySelector('.n-edit')?.focus();
  });

  // ═══════════ панель заметок ═══════════

  async function resolve(n, status) {
    await api.patch(`/annotations/${n.id}`, { status });
    n.status = status;
    applyHighlights();
    renderNotes();
    state.chapters = await api.get(`/books/${state.book.id}/chapters`);
    renderList();
  }

  function renderNotes() {
    clear(notesPanel);
    const open = openNotes();
    const done = notes.filter((n) => n.status !== 'открыта');

    notesPanel.append(el('h3.notes-head', {}, `Заметки · ${open.length}`));

    if (!open.length && !done.length) {
      notesPanel.append(el('div.empty', { style: { padding: '20px 0', fontSize: '13px' } },
        'Выдели текст и нажми «+ Заметка»', el('br'),
        el('span', { style: { fontSize: '12px', opacity: '.7' } }, 'или Ctrl+Alt+M')));
      return;
    }

    if (open.length) {
      notesPanel.append(el('div.ai-row', {},
        el('button.btn', { onclick: buildAiPrompt, title: 'Собрать текст и заметки для модели' },
          'Собрать для ИИ'),
        el('button.btn', { onclick: applyAiEdits, title: 'Вставить ответ модели' },
          'Применить правки'),
        el('button.btn', { onclick: suggestAiEdits, title: 'Получить предложения через настроенный API' },
          'ИИ предложит')
      ));
    }

    for (const n of open) notesPanel.append(noteCard(n));

    if (done.length) {
      notesPanel.append(el('button.btn', {
        style: { width: '100%', marginTop: '10px' },
        onclick: () => { showDone = !showDone; renderNotes(); },
      }, `${showDone ? 'Скрыть' : 'Показать'} выполненные · ${done.length}`));
      if (showDone) for (const n of done) notesPanel.append(noteCard(n));
    }
  }

  const autosize = (ta) => {
    ta.style.height = 'auto';
    ta.style.height = Math.max(34, ta.scrollHeight) + 'px';
  };

  function jumpTo(n) {
    const spans = prose.querySelectorAll(`.ann[data-ann="${n.id}"]`);
    if (!spans.length) return toast('Фрагмент не найден — текст сильно изменился', true);
    spans[0].scrollIntoView({ block: 'center', behavior: 'smooth' });
    spans.forEach((s) => s.classList.add('flash'));
    setTimeout(() => spans.forEach((s) => s.classList.remove('flash')), 900);
  }

  function noteCard(n) {
    const isOpen = n.status === 'открыта';

    const field = el('textarea', {
      class: 'n-edit',
      rows: 1,
      placeholder: 'Что поправить?',
      value: n.note ?? '',
      readonly: isOpen ? null : 'readonly',
      oninput: (e) => autosize(e.target),
      onkeydown: (e) => {
        if (e.key === 'Escape') { e.target.value = n.note ?? ''; e.target.blur(); }
        if (e.key === 'Enter' && (e.ctrlKey || e.metaKey)) { e.preventDefault(); e.target.blur(); }
      },
      onblur: async (e) => {
        const value = e.target.value.trim();
        if (value === (n.note ?? '').trim()) return;

        // Пустая заметка не нужна — убираем вместе с подсветкой
        if (!value) {
          await api.del(`/annotations/${n.id}`);
          notes = notes.filter((x) => x.id !== n.id);
          applyHighlights();
          renderNotes();
          return;
        }
        await api.patch(`/annotations/${n.id}`, { note: value });
        n.note = value;
        for (const s of prose.querySelectorAll(`.ann[data-ann="${n.id}"]`)) s.title = value;
      },
    });

    requestAnimationFrame(() => autosize(field));

    return el('div', { class: 'note' + (isOpen ? '' : ' done'), 'data-note': n.id },
      el('div.q', {
        title: 'Показать в тексте',
        onclick: () => jumpTo(n),
      }, (n.quote ?? '').slice(0, 90) + ((n.quote ?? '').length > 90 ? '…' : '')),
      field,
      isOpen && n._lost
        ? el('div', { class: 'pill warn', style: { marginTop: '6px' } }, 'привязка потеряна')
        : null,
      el('div.acts', {},
        isOpen
          ? el('button.btn', { onclick: () => resolve(n, 'применена'), title: 'Заметка отработана' }, 'Готово')
          : el('button.btn', { onclick: () => resolve(n, 'открыта') }, 'Вернуть'),
        el('button.btn.danger', {
          title: 'Удалить',
          onclick: async () => {
            await api.del(`/annotations/${n.id}`);
            notes = notes.filter((x) => x.id !== n.id);
            applyHighlights();
            renderNotes();
          },
        }, '×')
      )
    );
  }

  // ═══════════ мост к ИИ ═══════════

  /**
   * Собирает всё, что нужно модели: текст главы и заметки вместе с абзацами,
   * в которых они сидят. Просим вернуть точные замены — так правку можно
   * применить хирургически и проверить, а не переписывать главу целиком.
   */
  async function buildAiPrompt() {
    if (dirty) return toast('Сначала сохрани главу', true);
    const ctx = await api.get(`/chapters/${chapter.id}/ai-context`);
    if (!ctx.notes.length) return toast('Нет открытых заметок');

    const lost = ctx.notes.filter((n) => !n.found);
    const notesText = ctx.notes.map((n) => {
      const lines = [`[${n.id}] «${n.quote}»`];
      if (n.paragraph) lines.push(`  абзац: ${n.paragraph}`);
      if (!n.unique && n.found) lines.push(`  ⚠ цитата встречается ${n.occurrences} раз`);
      if (!n.found) lines.push('  ⚠ фрагмент не найден в тексте');
      lines.push(`  что поправить: ${n.note}`);
      return lines.join('\n');
    }).join('\n\n');

    const p = [
      `Ты редактируешь главу книги «${ctx.book.title}».`,
      ctx.book.style ? `Стиль — профиль «${ctx.book.style}» (styles/${ctx.book.style}.md).` : '',
      `Глава ${ctx.chapter.number}. «${ctx.chapter.title ?? ''}»`,
      ctx.chapter.pov ? `POV: ${ctx.chapter.pov}` : '',
      '',
      'Ниже текст главы и заметки автора. Для каждой заметки верни точную замену.',
      '',
      'ФОРМАТ ОТВЕТА — только JSON, без пояснений:',
      '{"edits":[{"annotation_id":12,"old":"<фрагмент дословно из текста>","new":"<новый текст>"}]}',
      '',
      'Правила:',
      '— "old" должен встречаться в тексте ДОСЛОВНО, символ в символ;',
      '— бери целое предложение или абзац, а не пару слов: так замена однозначна;',
      '— не меняй ничего, о чём не просили;',
      '— сохраняй манеру речи персонажей и стиль книги;',
      `— в главе сейчас ${num(ctx.chapter.chars_no_space)} знаков без пробелов, порог площадки ${num(ctx.chapter.min_chars)}.`,
      '',
      '=== ТЕКСТ ГЛАВЫ ===',
      ctx.text,
      '',
      '=== ЗАМЕТКИ ===',
      notesText,
    ].filter((x) => x !== '').join('\n');

    showText('Промпт для модели', p, {
      hint: lost.length
        ? `Скопируй и отдай модели. Внимание: ${lost.length} заметок потеряли привязку к тексту.`
        : 'Скопируй, отдай модели, потом вернись и нажми «Применить правки».',
    });
  }

  async function applyAiEdits() {
    const raw = await pasteBox('Ответ модели', {
      hint: 'Вставь JSON вида {"edits":[{"annotation_id":…,"old":"…","new":"…"}]}',
    });
    if (!raw) return;

    let payload;
    try {
      const json = raw.trim().replace(/^```(?:json)?\s*|\s*```$/g, '');
      payload = JSON.parse(json);
    } catch {
      return toast('Не похоже на JSON', true);
    }
    const edits = Array.isArray(payload) ? payload : payload.edits;
    if (!Array.isArray(edits) || !edits.length) return toast('В ответе нет правок', true);

    try {
      if (dirty) return toast('Сначала сохраните главу', true);
      const path = `/chapters/${chapter.id}/apply-edits`;
      const plan = await api.post(path, { edits, dry_run: true });
      if (!plan.applied.length) {
        showText('Правки отклонены', plan.rejected.map((r) => r.reason).join('\n'));
        return;
      }
      const selected = await reviewAiEdits(path, plan);
      if (!selected?.length) return;
      const res = await api.post(path, { edits: selected, expected_revision: plan.revision });
      await open(chapter.id);

      const parts = [`Применено: ${res.applied.length}`];
      if (res.rejected.length) parts.push(`отклонено: ${res.rejected.length}`);
      toast(parts.join(' · '), res.rejected.length > 0);

      if (res.rejected.length) {
        showText('Отклонённые правки', res.rejected.map((r) =>
          `Заметка ${r.annotation_id ?? '—'}: ${r.reason}\n  old: ${String(r.old ?? '').slice(0, 200)}`
        ).join('\n\n'), {
          hint: 'Эти правки не применены — фрагмент не найден или встречается неоднозначно. Текст главы не тронут в этих местах.',
        });
      }
    } catch (e) {
      toast(e.message, true);
    }
  }

  async function suggestAiEdits() {
    if (dirty) return toast('Сначала сохраните главу', true);
    try {
      const plan = await api.post(`/chapters/${chapter.id}/ai/suggest-edits`, {});
      if (!plan.applied.length) {
        showText('Правки отклонены', plan.rejected.map((r) => r.reason).join('\n'));
        return;
      }
      const path = `/chapters/${chapter.id}/apply-edits`;
      const selected = await reviewAiEdits(path, plan);
      if (!selected?.length) return;
      const res = await api.post(path, { edits: selected, expected_revision: plan.revision });
      await open(chapter.id);
      toast(`Применено: ${res.applied.length}${res.rejected.length ? ` · отклонено: ${res.rejected.length}` : ''}`,
        res.rejected.length > 0);
    } catch (e) { toast(e.message, true); }
  }

  function reviewAiEdits(path, plan) {
    return new Promise((resolve) => {
      const checks = plan.applied.map((e) => {
        const check = el('input', { type: 'checkbox', checked: 'checked' });
        return { edit: e, check };
      });
      const after = el('textarea', { rows: 16, readonly: 'readonly', value: plan.preview });
      const list = el('div.edit-review-list');
      for (const { edit, check } of checks) list.append(el('label', {}, check,
        el('span', {}, `Заметка ${edit.annotation_id ?? '—'}: ${edit.old.slice(0, 90)} → ${edit.new.slice(0, 90)}`)));
      const choose = el('button.btn.primary', {}, 'Применить выбранные');
      const close = (value) => { bg.remove(); resolve(value); };
      const bg = el('div.modal-bg', {}, el('div.modal.wide', {},
        el('h3', {}, 'Проверьте изменения перед применением'),
        list,
        plan.rejected.length ? el('p.dim', {}, `Отклонено: ${plan.rejected.length}`) : null,
        el('div.diff-grid', {},
          el('label.field', {}, el('span', {}, 'Текущий текст'), el('textarea', {
            rows: 16, readonly: 'readonly', value: chapter.body,
          })),
          el('label.field', {}, el('span', {}, 'После правок'), after)),
        el('div.row', {}, el('button.btn', { onclick: () => close(null) }, 'Отмена'), choose)));
      for (const { check } of checks) check.addEventListener('change', async () => {
        const selected = checks.filter((x) => x.check.checked).map((x) => x.edit);
        choose.disabled = !selected.length;
        if (!selected.length) { after.value = chapter.body; return; }
        try { after.value = (await api.post(path, { edits: selected, dry_run: true })).preview; }
        catch (e) { toast(e.message, true); }
      });
      choose.addEventListener('click', () => close(checks.filter((x) => x.check.checked).map((x) => x.edit)));
      document.body.append(bg);
    });
  }

  // ═══════════ запуск ═══════════

  renderList();
  const first = state.currentChapter
    ?? state.chapters.find((c) => c.status === 'написано')?.id
    ?? state.chapters[0]?.id;
  if (first) open(first);
  else doc.append(el('div.empty', {}, 'Глав пока нет — создай их на холсте'));
}

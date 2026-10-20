import { api, el, clear, toast, prompt, showText, confirm } from './lib.js';

const DEFAULT_MODEL = { deepseek: 'deepseek-flash', openrouter: 'openrouter/free' };

export function aiView(root, state) {
  const chapterStorageKey = `storyforge:ai-chapter:${state.book.id}`;
  const modeStorageKey = `storyforge:ai-mode:${state.book.id}`;
  if (!Object.hasOwn(state, 'aiChapter')) {
    let remembered = null;
    try { remembered = localStorage.getItem(chapterStorageKey); } catch { /* хранилище может быть отключено */ }
    const id = remembered && remembered !== 'all' ? Number(remembered) : null;
    state.aiChapter = remembered == null ? state.currentChapter
      : id && state.chapters.some((ch) => ch.id === id) ? id : null;
  }
  const key = `${state.book.id}:${state.aiChapter ?? ''}`;
  state.aiChats ??= {};
  state.aiLoaded ??= {};
  state.aiHasMore ??= {};
  state.aiPending ??= {};
  const history = state.aiChats[key] ??= [];
  let historyLoading = false;
  let historyError = '';
  let hasMore = Boolean(state.aiHasMore[key]);
  const provider = el('select', {},
    el('option', { value: 'deepseek' }, 'DeepSeek'),
    el('option', { value: 'openrouter' }, 'OpenRouter'));
  const model = el('input', { type: 'text', placeholder: 'Имя модели' });
  const apiKey = el('input', { type: 'password', placeholder: 'Введите API-ключ', autocomplete: 'off' });
  const keyStatus = el('div.ai-key-status', { role: 'status' });
  const connectionStatus = el('span.ai-connection-status', { role: 'status' }, 'Загружаю настройки…');
  const testBtn = el('button.btn', { onclick: testConnection }, 'Проверить подключение');
  let savedConfig = null;
  const settings = el('div.ai-settings', {},
    el('label.field', {}, el('span', {}, 'Провайдер'), provider),
    el('label.field', {}, el('span', {}, 'Модель'), model),
    el('label.field', {}, el('span', {}, 'API-ключ'), apiKey),
    keyStatus,
    el('div.row', {},
      el('button.btn.primary', { onclick: saveSettings }, 'Сохранить'),
      testBtn,
      el('button.btn.danger', { onclick: removeKey }, 'Удалить ключ')),
    el('p.dim', {}, 'После сохранения поле ключа очищается: сервер не возвращает секрет в браузер. Ключи DeepSeek и OpenRouter хранятся отдельно. Для переключения выберите провайдера и нажмите «Сохранить». Проверка и сообщения могут расходовать лимит API.'));
  const settingsBtn = el('button.btn', { onclick: () => settings.classList.toggle('on') }, 'Настройки API');
  const refreshBtn = el('button.btn', { onclick: () => loadHistory(true) }, 'Обновить переписку');
  const actions = el('div.ai-actions', {},
    refreshBtn,
    el('button.btn', { onclick: showContext }, 'Контекст сцены'),
    el('button.btn', { onclick: searchText }, 'Поиск по книге'),
    el('button.btn', { onclick: extractFacts }, 'Извлечь факты'),
    el('button.btn', { onclick: () => runTask('plan') }, 'План главы'),
    el('button.btn', { onclick: () => runTask('draft') }, 'Черновик'),
    el('button.btn', { onclick: () => runTask('style_review') }, 'Разбор стиля'),
    el('button.btn', { onclick: () => {
      const id = requireChapter(); if (id && workspace().draft) showDraft(id, workspace());
      else if (id) toast('Черновика пока нет');
    } }, 'Открыть черновик'),
    el('button.btn', { onclick: addStyleExample }, 'Образец стиля'));
  const chapter = el('select', {}, el('option', { value: '' }, 'Вся книга'));
  for (const ch of state.chapters) chapter.append(el('option', { value: ch.id }, `${ch.number}. ${ch.title || 'Без названия'}`));
  chapter.value = String(state.aiChapter ?? '');
  let rememberedMode = 'agent';
  try { rememberedMode = localStorage.getItem(modeStorageKey) || 'agent'; } catch { /* хранилище может быть отключено */ }
  const mode = el('select.ai-mode', { 'aria-label': 'Режим ИИ' },
    el('option', { value: 'agent' }, 'Агент · меняет книгу'),
    el('option', { value: 'chat' }, 'Диалог · только ответ'));
  mode.value = rememberedMode === 'chat' ? 'chat' : 'agent';
  const modeHint = el('div.ai-mode-hint');
  const syncMode = () => {
    modeHint.textContent = mode.value === 'agent'
      ? 'Агент выполняет просьбы в книге: пишет главы, меняет персонажей, состояние и события. Выполненные действия появятся под ответом.'
      : 'Диалог отвечает на вопросы и предлагает текст в чате, не меняя книгу.';
    input.placeholder = mode.value === 'agent'
      ? 'Например: напиши главу 11 и сохрани её; добавь персонажа; обнови его состояние…'
      : 'Спросите о сюжете, персонаже или попросите предложить фрагмент…';
    try { localStorage.setItem(modeStorageKey, mode.value); } catch { /* необязательно */ }
  };
  mode.addEventListener('change', syncMode);
  const messages = el('div.ai-messages');
  const input = el('textarea', { rows: 4, placeholder: 'Спросите о сюжете, персонаже или попросите предложить фрагмент…' });
  syncMode();
  const sendBtn = el('button.btn.primary', { onclick: send }, 'Отправить');
  const form = el('div.ai-compose', {}, input, el('div.row', {}, sendBtn));
  const panel = el('div', { id: 'ai-view' },
    el('div.ai-top', {}, el('b', {}, 'ИИ-помощник'), mode, chapter, connectionStatus, settingsBtn),
    modeHint, settings, actions, messages, form);
  root.append(panel);

  const selectedChapter = () => chapter.value ? Number(chapter.value) : null;
  const requireChapter = () => {
    const id = selectedChapter();
    if (!id) toast('Сначала выберите главу', true);
    return id;
  };
  state.aiWorkspace ??= {};
  const workspace = () => state.aiWorkspace[selectedChapter()] ??= { plan: '', draft: '', revision: null, audited: false };

  async function showContext() {
    const id = requireChapter(); if (!id) return;
    try {
      const packet = await api.get(`/books/${state.book.id}/scenes/${id}/context`);
      const content = el('div.ai-context-scroll');
      const section = (title) => { const box = el('section', {}, el('h4', {}, title)); content.append(box); return box; };
      const addSource = (row, item) => {
        if (item.source_chapter_id) row.append(el('button.btn', { onclick: () => {
          bg.remove(); state.openEditor(item.source_chapter_id);
        } }, `Источник: глава #${item.source_chapter_id}`));
        if (item.source_quote) row.append(el('small.dim', {}, `«${item.source_quote.slice(0, 150)}»`));
      };
      const truth = section('Канон мира до главы');
      for (const f of packet.world_truth.slice(0, 100)) {
        const row = el('div.ai-context-row', {}, `[${f.kind}] ${f.subject || f.subject_name || ''}: ${f.value}`);
        addSource(row, f);
        truth.append(row);
      }
      const pov = section(`Что доступно POV: ${packet.pov_knowledge?.character || 'не определено'}`);
      for (const stance of ['knows', 'suspects', 'misbeliefs', 'unaware'])
        for (const k of packet.pov_knowledge?.[stance] || []) {
          const row = el('div.ai-context-row', {},
            `${k.stance}: ${k.topic || k.secret_title || ''}${k.belief ? ` — ${k.belief}` : ''}`);
          addSource(row, k); pov.append(row);
        }
      const reader = section('Что знает читатель');
      if (!packet.reader_knowledge_tracked) reader.append(el('p.dim', {}, 'Осведомлённость читателя ещё не размечена.'));
      for (const k of packet.reader_knowledge) {
        const row = el('div.ai-context-row', {}, `${k.stance}: ${k.topic}`);
        addSource(row, k); reader.append(row);
      }
      const hidden = section('За кадром — только для автора');
      for (const b of packet.offscreen_for_author) hidden.append(el('div.ai-context-row', {},
        `${b.line}: ${b.offscreen}${b.visible_effect ? ` → ${b.visible_effect}` : ''}`));
      const bg = el('div.modal-bg', {}, el('div.modal.wide', {},
        el('h3', {}, `Контекст: глава ${packet.chapter.number}`), content,
        el('div.row', {}, el('button.btn', { onclick: () => bg.remove() }, 'Закрыть'))));
      document.body.append(bg);
    } catch (e) { toast(e.message, true); }
  }

  async function searchText() {
    const q = await prompt('Поиск по главам и событиям за кадром');
    if (!q?.trim()) return;
    try {
      const result = await api.get(`/books/${state.book.id}/text-search?q=${encodeURIComponent(q.trim())}`);
      const lines = [
        ...result.chapters.map((c) => `Глава ${c.number} «${c.title || ''}»: ${c.snippet}`),
        ...result.offscreen.map((b) => `За кадром, гл.${b.chapter_no}: ${b.offscreen}`),
      ];
      showText(`Поиск: ${q}`, lines.join('\n\n') || 'Совпадений нет');
    } catch (e) { toast(e.message, true); }
  }

  async function extractFacts() {
    const id = requireChapter(); if (!id) return;
    try {
      const result = await api.post(`/chapters/${id}/ai/extract`, {});
      if (!result.accepted.length) {
        showText('Факты не найдены', result.rejected.map((r) => r.reason).join('\n') || 'Новых утверждений нет');
        return;
      }
      const list = el('div.ai-context-scroll');
      const choices = result.accepted.map((p) => {
        const checkbox = el('input', { type: 'checkbox', checked: 'checked' });
        list.append(el('label.ai-proposal', {}, checkbox, el('span', {},
          `${p.type}: ${p.value || p.topic} · источник: «${p.source_quote.slice(0, 170)}»`)));
        return { p, checkbox };
      });
      const bg = el('div.modal-bg', {}, el('div.modal.wide', {},
        el('h3', {}, 'Проверьте факты перед записью в канон'), list,
        result.rejected.length ? el('p.dim', {}, `Отклонено без источника или с ошибкой: ${result.rejected.length}`) : null,
        el('div.row', {},
          el('button.btn', { onclick: () => bg.remove() }, 'Отмена'),
          el('button.btn.primary', { onclick: async () => {
            const proposals = choices.filter((x) => x.checkbox.checked).map((x) => x.p);
            if (!proposals.length) return;
            try {
              const saved = await api.post(`/chapters/${id}/ai/commit-facts`, {
                expected_revision: result.revision, proposals,
              });
              bg.remove(); toast(`Записано утверждений: ${saved.inserted}`);
            } catch (e) { toast(e.message, true); }
          } }, 'Записать выбранные'))));
      document.body.append(bg);
    } catch (e) { toast(e.message, true); }
  }

  async function runTask(kind) {
    const id = requireChapter(); if (!id) return;
    const work = workspace();
    if (kind === 'style_review' && !work.draft.trim()) { toast('Сначала подготовьте черновик', true); return; }
    if (kind === 'draft') {
      const sceneType = await prompt('Тип сцены для подбора образцов стиля', { value: work.sceneType || 'общий' });
      if (sceneType == null) return;
      work.sceneType = sceneType.trim().slice(0, 60) || 'общий';
    }
    try {
      const result = await api.post(`/chapters/${id}/ai/task`, {
        kind, input: kind === 'draft' ? work.plan : work.draft,
        scene_type: work.sceneType || 'общий',
      });
      if (kind === 'plan') {
        const edited = await prompt('План главы — можно поправить перед черновиком', {
          value: result.text, multiline: true, ok: 'Сохранить план',
        });
        if (edited != null) { work.plan = edited; toast('План сохранён для этого сеанса'); }
      } else if (kind === 'draft') {
        work.draft = result.text;
        work.revision = result.revision;
        work.audited = false;
        showDraft(id, work);
      } else if (kind === 'style_review') {
        showText('Редакторский разбор стиля', result.text);
      }
    } catch (e) { toast(e.message, true); }
  }

  function showDraft(id, work) {
    const area = el('textarea', { rows: 20, value: work.draft });
    const accept = el('button.btn.primary', { disabled: true }, 'Принять в главу');
    let reviewedText = null;
    area.addEventListener('input', () => { work.draft = area.value; work.audited = false; reviewedText = null; accept.disabled = true; });
    const bg = el('div.modal-bg', {}, el('div.modal.wide', {},
      el('h3', {}, 'Черновик главы'), area,
      el('p.dim', {}, 'Проверьте текст. Перед записью запустите проверку канона и стиля.'),
      el('div.row', {},
        el('button.btn', { onclick: () => bg.remove() }, 'Закрыть'),
        el('button.btn', { onclick: async () => {
          work.draft = area.value;
          try {
            const submitted = area.value;
            const audit = await api.post(`/chapters/${id}/ai/task`, { kind: 'audit', input: submitted,
              scene_type: work.sceneType || 'общий' });
            if (area.value === submitted) {
              reviewedText = submitted; work.audited = true; accept.disabled = false;
            }
            showText('ИИ-проверка черновика — проверьте замечания самостоятельно', audit.text);
          } catch (e) { toast(e.message, true); }
        } }, 'Проверить'), accept)));
    accept.addEventListener('click', async () => {
      if (reviewedText !== area.value) { toast('Текст изменился после проверки. Запустите её снова.', true); return; }
      if (!(await confirm('Записать черновик в главу? Предыдущая версия сохранится.', { ok: 'Записать' }))) return;
      try {
        await api.req('PUT', `/chapters/${id}/text`, {
          body: area.value, expected_revision: work.revision, reason: 'черновик ИИ',
        });
        state.chapters = await api.get(`/books/${state.book.id}/chapters`);
        bg.remove(); toast('Черновик записан; проверьте факты главы');
        state.openEditor(id);
      } catch (e) { toast(e.message, true); }
    });
    document.body.append(bg);
  }

  async function addStyleExample() {
    const sceneType = await prompt('Тип сцены для образца', { value: 'общий' });
    if (!sceneType) return;
    const body = await prompt('Одобренный автором фрагмент', { multiline: true, ok: 'Сохранить образец' });
    if (!body) return;
    try {
      await api.post(`/books/${state.book.id}/style-examples`, {
        scene_type: sceneType, body, source_chapter_id: selectedChapter(),
      });
      toast('Образец сохранён и будет доступен помощнику');
    } catch (e) { toast(e.message, true); }
  }

  function renderMessages() {
    clear(messages);
    const pending = state.aiPending[key];
    if (historyLoading) messages.append(el('div.empty', {}, 'Загружаю историю переписки…'));
    else if (historyError) messages.append(el('div.ai-message.error', {},
      `Не удалось загрузить переписку: ${historyError}`,
      el('button.btn', { onclick: () => loadHistory(true) }, 'Повторить загрузку')));
    else if (!history.length && !pending) messages.append(el('div.empty', {},
      'Напишите агенту, что сделать в книге. Он сможет создать или изменить главу, персонажа и другие записи. Для обсуждения без изменений выберите «Диалог».'));
    if (hasMore) messages.append(el('p.dim', {}, 'Показаны последние 200 запросов этой главы.'));
    for (const m of history) messages.append(el('div', { class: `ai-message ${m.role}` },
      el('div.ai-message-head', {}, m.role === 'user' ? 'Вы' : m.role === 'error' ? 'Ошибка запроса' : 'Помощник'),
      el('div.ai-message-text', {}, m.content),
      m.actions?.length ? el('div.ai-done', {},
        el('b', {}, `Выполнено действий: ${m.actions.length}`),
        ...m.actions.map((action) => el('div.ai-done-row', {},
          el('span', {}, action.label),
          action.entity_type === 'chapter' && action.id ? el('button.btn', {
            onclick: () => state.openEditor(action.id),
          }, 'Открыть главу') : null))) : null,
      m.role === 'assistant' ? el('button.btn', {
        onclick: async () => {
          try { await navigator.clipboard.writeText(m.content); toast('Ответ скопирован'); }
          catch { toast('Не удалось скопировать', true); }
        },
      }, 'Копировать') : null,
      m.role === 'error' && m.retry ? el('button.btn', { onclick: () => {
        if (state.aiPending[key]) return;
        input.value = m.retry;
        send();
      } }, 'Повторить запрос') : null));
    if (pending) messages.append(el('div.ai-message.ai-waiting', { role: 'status', 'aria-live': 'polite' },
      el('div.ai-message-head', {}, pending.mode === 'agent' ? 'Агент выполняет задание' : 'Помощник отвечает'),
      el('div.ai-message-text', {}, el('span.ai-pulse', { 'aria-hidden': 'true' }),
        `${pending.mode === 'agent' ? 'Читает книгу, вызывает инструменты и сохраняет изменения' : 'Ждём ответ модели'} · ${Math.floor((Date.now() - pending.started) / 1000)} с`)));
    sendBtn.disabled = Boolean(pending) || historyLoading || Boolean(historyError);
    sendBtn.textContent = pending ? 'Выполняю…' : historyLoading ? 'Загружаю…' : mode.value === 'agent' ? 'Поручить агенту' : 'Отправить';
    messages.scrollTop = messages.scrollHeight;
  }

  function setStatus(config) {
    savedConfig = config;
    const activeName = config.provider === 'deepseek' ? 'DeepSeek' : 'OpenRouter';
    connectionStatus.textContent = `Активен: ${activeName} · ${config.model} · ${config.has_key ? 'ключ сохранён' : 'нет ключа'}`;
    const selected = config.profiles?.[provider.value] || { has_key: config.has_key, model: config.model };
    keyStatus.textContent = selected.has_key
      ? `Ключ ${provider.value === 'deepseek' ? 'DeepSeek' : 'OpenRouter'} сохранён. Поле пустое, потому что ключ скрыт. Чтобы заменить его, введите новый и сохраните.`
      : `Для ${provider.value === 'deepseek' ? 'DeepSeek' : 'OpenRouter'} ключ ещё не сохранён.`;
    apiKey.placeholder = selected.has_key ? 'Ключ сохранён · введите новый для замены' : 'Введите API-ключ';
  }
  async function loadSettings() {
    try {
      const config = await api.get('/ai/settings');
      provider.value = config.provider;
      model.value = config.model;
      setStatus(config);
      if (!config.has_key) settings.classList.add('on');
    } catch (e) { toast(e.message, true); }
  }
  provider.addEventListener('change', () => {
    model.value = savedConfig?.profiles?.[provider.value]?.model || DEFAULT_MODEL[provider.value];
    apiKey.value = '';
    setStatus(savedConfig || { provider: 'deepseek', model: DEFAULT_MODEL.deepseek, has_key: false });
    keyStatus.textContent += ' Выбор ещё не применён: нажмите «Сохранить».';
  });
  model.addEventListener('input', () => {
    keyStatus.textContent = 'Имя модели изменено. Нажмите «Сохранить», чтобы применить его.';
  });
  chapter.addEventListener('change', () => {
    state.aiChapter = chapter.value ? Number(chapter.value) : null;
    if (state.aiChapter != null) state.currentChapter = state.aiChapter;
    try { localStorage.setItem(chapterStorageKey, state.aiChapter == null ? 'all' : String(state.aiChapter)); }
    catch { /* история всё равно хранится на сервере */ }
    clear(root);
    aiView(root, state);
  });

  async function loadHistory(force = false) {
    if (!force && state.aiLoaded[key]) return;
    if (historyLoading) return;
    if (state.aiPending[key]) { toast('Дождитесь ответа, затем обновите переписку'); return; }
    historyLoading = true;
    historyError = '';
    renderMessages();
    try {
      const query = state.aiChapter == null ? '' : `?chapter_id=${state.aiChapter}`;
      const result = await api.get(`/books/${state.book.id}/ai/history${query}`);
      history.splice(0, history.length, ...result.messages);
      hasMore = result.has_more;
      state.aiHasMore[key] = hasMore;
      state.aiLoaded[key] = true;
    } catch (e) {
      historyError = e.message;
    } finally {
      historyLoading = false;
      if (messages.isConnected) renderMessages();
    }
  }

  async function saveSettings() {
    try {
      const config = await api.req('PUT', '/ai/settings', {
        provider: provider.value, model: model.value, api_key: apiKey.value,
      });
      apiKey.value = '';
      setStatus(config);
      toast(`Выбран ${config.provider === 'deepseek' ? 'DeepSeek' : 'OpenRouter'} · ${config.has_key ? 'ключ сохранён' : 'ключ не задан'}`);
    } catch (e) { toast(e.message, true); }
  }
  async function removeKey() {
    try {
      const config = await api.req('PUT', '/ai/settings', {
        provider: provider.value, model: model.value, clear_key: true,
      });
      apiKey.value = '';
      setStatus(config);
      toast('API-ключ удалён');
    } catch (e) { toast(e.message, true); }
  }
  async function testConnection() {
    if (savedConfig && (savedConfig.provider !== provider.value || savedConfig.model !== model.value)) {
      keyStatus.textContent = 'Сначала сохраните выбранного провайдера и модель, затем проверьте подключение.';
      return;
    }
    testBtn.disabled = true;
    testBtn.textContent = 'Проверяю…';
    keyStatus.textContent = 'Отправляю короткий запрос выбранной модели…';
    try {
      const r = await api.post('/ai/test', {});
      keyStatus.textContent = `Подключение работает. Ответила модель ${r.model}.`;
      toast(`Подключено: ${r.model}`);
    } catch (e) {
      keyStatus.textContent = `Проверка не прошла: ${e.message}`;
      toast(e.message, true);
    } finally { testBtn.disabled = false; testBtn.textContent = 'Проверить подключение'; }
  }
  async function send() {
    const message = input.value.trim();
    if (!message || state.aiPending[key] || historyLoading || historyError) return;
    const prior = history.filter((m, i) => m.role === 'assistant' ||
      m.role === 'user' && history[i + 1]?.role !== 'error').slice(-8)
      .map(({ role, content }) => ({ role, content: content.slice(-6000) }));
    history.push({ role: 'user', content: message });
    const requestedMode = mode.value;
    state.aiPending[key] = { started: Date.now(), mode: requestedMode };
    input.value = '';
    renderMessages();
    try {
      const result = await api.post(`/books/${state.book.id}/ai/${requestedMode}`, {
        chapter_id: chapter.value ? Number(chapter.value) : null,
        message, history: prior,
      });
      if (result.error) {
        history.push({ role: 'error', content: result.error, retry: message, actions: result.actions || [] });
        toast(result.error, true);
      } else history.push({ role: 'assistant', content: result.text, actions: result.actions || [] });
      if (result.actions?.length) {
        try {
          const refreshed = await Promise.all([
            api.get(`/books/${state.book.id}/chapters`),
            api.get(`/books/${state.book.id}/characters`),
          ]);
          state.chapters = refreshed[0];
          state.characters = refreshed[1];
          const previous = chapter.value;
          clear(chapter);
          chapter.append(el('option', { value: '' }, 'Вся книга'));
          for (const ch of state.chapters) chapter.append(el('option', { value: ch.id }, `${ch.number}. ${ch.title || 'Без названия'}`));
          chapter.value = previous;
        } catch (refreshError) { toast(`Действия сохранены, но список не обновился: ${refreshError.message}`, true); }
      }
      if (result.finish_reason === 'length') toast('Ответ обрезан лимитом модели', true);
    } catch (e) {
      history.push({ role: 'error', content: e.message, retry: message });
      toast(e.message, true);
    } finally {
      delete state.aiPending[key];
      if (messages.isConnected) renderMessages();
    }
  }
  input.addEventListener('keydown', (e) => {
    if (e.key === 'Enter' && (e.ctrlKey || e.metaKey)) { e.preventDefault(); send(); }
  });
  renderMessages();
  loadHistory();
  let shownCount = history.length;
  const ticker = setInterval(() => {
    if (!panel.isConnected) { clearInterval(ticker); return; }
    if (history.length !== shownCount) { shownCount = history.length; renderMessages(); }
    const pending = state.aiPending[key];
    const line = messages.querySelector('.ai-waiting .ai-message-text');
    if (pending && line) line.lastChild.textContent =
      `${pending.mode === 'agent' ? 'Читает книгу, вызывает инструменты и сохраняет изменения' : 'Ждём ответ модели'} · ${Math.floor((Date.now() - pending.started) / 1000)} с`;
    else if (pending && !line || !pending && line) renderMessages();
  }, 1000);
  loadSettings();
}

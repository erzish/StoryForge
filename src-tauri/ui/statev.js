/**
 * Состояние мира на момент времени.
 * Ползунок двигает точку истории — всё на экране пересобирается под неё.
 * Это же содержимое уходит в промпт перед написанием главы.
 */
import { api, el, clear, toast, num, showText, pasteBox, confirm, prompt } from './lib.js';

export function stateView(root, state) {
  let moments = [];
  let idx = 0;
  let data = null;
  let changes = null;
  let filter = '';
  let scope = 'scene';   // кого включать в срез: scene | main | all

  const slider = el('input', {
    type: 'range', min: 0, max: 0, value: 0,
    oninput: (e) => { idx = Number(e.target.value); load(); },
  });
  const label = el('div.moment-label');
  const body = el('div.state-body');

  // Охват среза. На длинной книге «все» даёт сотни килобайт — в промпт
  // такое не помещается, поэтому по умолчанию берём только тех, кто в кадре.
  const scopeSel = el('select', {
    style: { maxWidth: '180px' },
    onchange: (e) => { scope = e.target.value; load(); },
  },
    el('option', { value: 'scene' }, 'участники момента'),
    el('option', { value: 'main' }, 'только главные'),
    el('option', { value: 'all' }, 'все персонажи')
  );

  const bar = el('div.time-bar', {},
    el('button.btn', { onclick: () => step(-1), title: 'Назад' }, '‹'),
    el('div.time-mid', {}, slider, label),
    el('button.btn', { onclick: () => step(1), title: 'Вперёд' }, '›'),
    scopeSel,
    el('input', {
      type: 'text', placeholder: 'фильтр по имени или факту…',
      style: { maxWidth: '220px' },
      oninput: (e) => { filter = e.target.value.toLowerCase(); render(); },
    }),
    el('button.btn', { onclick: buildPrompt, title: 'Собрать срез для модели' }, 'Срез для ИИ'),
    el('button.btn.primary', { onclick: extractFacts, title: 'Разобрать главу на факты' }, 'Извлечь факты')
  );

  root.append(el('div', { id: 'statev' }, bar, body));

  function step(d) {
    idx = Math.max(0, Math.min(moments.length - 1, idx + d));
    slider.value = idx;
    load();
  }

  async function load() {
    const m = moments[idx];
    if (!m) return;
    label.innerHTML =
      `<b>${m.label}</b> <span class="dim">${m.world ?? ''}` +
      `${m.chapters ? ` · главы ${m.chapters}` : ''}</span>`;

    const params = new URLSearchParams({ moment: m.id });
    const ids = await scopeCharacters(m);
    if (ids) params.set('characters', ids.join(','));

    data = await api.get(`/books/${state.book.id}/state?${params}`);
    changes = idx > 0
      ? await api.get(`/books/${state.book.id}/diff?from_moment=${moments[idx - 1].id}&to_moment=${m.id}`)
      : null;
    render();
  }

  /**
   * Кого включать в срез.
   * «Участники момента» — те, кто занят в событиях глав этого момента:
   * ровно то, что нужно перед написанием сцены, и то, что не разрастается
   * с длиной книги.
   */
  async function scopeCharacters(m) {
    if (scope === 'all') return null;
    if (scope === 'main') return state.characters.filter((c) => c.is_main).map((c) => c.id);

    const chapters = String(m.chapters ?? '').split(',').map(Number).filter(Boolean);
    if (!chapters.length) return state.characters.filter((c) => c.is_main).map((c) => c.id);

    const events = await api.get(`/books/${state.book.id}/events`);
    const here = events.filter((e) => chapters.includes(e.chapter_no));
    const names = new Set(here.flatMap((e) => String(e.participants ?? '').split(', ')).filter(Boolean));

    const ids = state.characters.filter((c) => names.has(c.name)).map((c) => c.id);
    // если участников не нашлось — не отдаём пустой срез, показываем главных
    return ids.length ? ids : state.characters.filter((c) => c.is_main).map((c) => c.id);
  }

  const matches = (text) => !filter || String(text ?? '').toLowerCase().includes(filter);

  function render() {
    clear(body);
    if (!data) return;

    // Видно, помещается ли срез в промпт: на длинной книге это главный вопрос
    const t = data.totals;
    const heavy = t.facts_in_slice > 400;
    body.append(el('div.slice-info', { class: 'slice-info' + (heavy ? ' heavy' : '') },
      el('span', {}, `в срезе ${num(t.facts_in_slice)} фактов из ${num(t.facts_total)} в базе`),
      el('span.dim', {}, ` · персонажей ${data.characters.length}`),
      heavy ? el('span.dim', {}, ' · для промпта лучше сузить охват') : null
    ));

    // ── что изменилось на этом шаге
    if (changes && (changes.appeared.length || changes.learned.length || changes.ended.length)) {
      const list = el('div.changes');
      for (const f of changes.appeared) {
        list.append(el('div.change', {},
          el('span.badge.new', {}, '+'),
          el('b', {}, f.subject ?? f.subject_type), ' ',
          el('span.dim', {}, `[${f.kind}] `), f.value));
      }
      for (const k of changes.learned) {
        list.append(el('div.change', {},
          el('span.badge.know', {}, '?'),
          el('b', {}, k.who ?? k.holder_label), ' ',
          el('span.dim', {}, k.stance + ': '), k.secret_title ?? k.topic ?? ''));
      }
      for (const f of changes.ended) {
        list.append(el('div.change', {},
          el('span.badge.end', {}, '−'),
          el('b', {}, f.subject ?? f.subject_type), ' ', el('s', {}, f.value)));
      }
      body.append(el('section.block', {}, el('h4', {}, 'Изменилось здесь'), list));
    }

    // ── персонажи
    const chars = data.characters.filter((c) =>
      matches(c.name) || c.facts.some((f) => matches(f.value)));

    const grid = el('div.state-grid');
    for (const c of chars) {
      const card = el('div.state-card');
      card.append(el('div.sc-head', {},
        el('b', {}, (c.is_main ? '★ ' : '') + c.name),
        el('span.dim', {}, c.group_name ?? '')));

      if (c.misbeliefs.length) {
        card.append(el('div.sc-warn', {},
          '⚠ заблуждается: ',
          c.misbeliefs.map((k) => k.secret_title ?? k.topic).join('; ')));
      }
      if (c.knows.length) {
        card.append(el('div.sc-know', {},
          'знает: ', c.knows.map((k) => k.secret_title ?? k.topic).join('; ')));
      }

      const byKind = {};
      for (const f of c.facts) (byKind[f.kind] ??= []).push(f);
      for (const [kind, list] of Object.entries(byKind)) {
        card.append(el('div.sc-kind', {},
          el('span.kind-tag', {}, kind),
          el('div', {}, ...list.map((f) => el('div.sc-fact', {
            title: `с ${f.from_label ?? '?'}${f.to_label ? ` до ${f.to_label}` : ''}`,
          }, f.value)))));
      }

      if (!c.facts.length && !c.knows.length) {
        card.append(el('div.dim', { style: { fontSize: '12px' } }, 'фактов на этот момент нет'));
      }
      grid.append(card);
    }
    body.append(el('section.block', {},
      el('h4', {}, `Персонажи · ${chars.length}`), grid));

    // ── связи
    const rels = data.relationships.filter((r) => matches(r.from_name) || matches(r.to_name) || matches(r.state));
    if (rels.length) {
      body.append(el('section.block', {},
        el('h4', {}, `Связи · ${rels.length}`),
        ...rels.map((r) => el('div.tl-row', {},
          el('span', { style: { minWidth: '190px' } },
            el('b', {}, r.from_name), ' → ', r.to_name),
          el('span.dim', { style: { flex: 1 } }, r.state ?? '')))));
    }

    // ── сюжет
    if (data.plot.length) {
      body.append(el('section.block', {},
        el('h4', {}, 'Незакрытое'),
        ...data.plot.map((f) => el('div.tl-row', {},
          el('span.kind-tag', {}, f.kind), el('span', { style: { flex: 1 } }, f.value)))));
    }

    if (data.open_promises.length) {
      body.append(el('section.block', {},
        el('h4', {}, `Ружья в ожидании · ${data.open_promises.length}`),
        ...data.open_promises.map((p) => el('div.tl-row', {},
          el('span.ch', {}, p.planted_ch ? `гл.${p.planted_ch}` : '—'),
          el('span', { style: { flex: 1 } }, p.title),
          el('span.dim', { style: { fontSize: '12px' } }, p.status)))));
    }
  }

  // ═══════════ мост к ИИ ═══════════

  /** Срез в текстовом виде — то, что уходит в промпт вместо перечитывания книги. */
  function stateAsText() {
    const m = moments[idx];
    const lines = [`СОСТОЯНИЕ МИРА на момент: ${m.label}${m.world ? ` (${m.world})` : ''}`];
    if (m.chapters) lines.push(`Главы этого момента: ${m.chapters}`);
    lines.push('');

    lines.push('=== ПЕРСОНАЖИ ===');
    for (const c of data.characters) {
      const bits = [`${c.name}${c.group_name ? ` (${c.group_name})` : ''}`];
      if (c.summary) bits.push(`  характер: ${c.summary}`);
      if (c.voice) bits.push(`  голос: ${c.voice}`);
      if (c.atmosphere_desc) bits.push(`  атмосфера: ${c.atmosphere_desc} ${c.atmosphere_level}/5`);
      for (const k of c.knows) bits.push(`  ЗНАЕТ: ${k.secret_title ?? k.topic}`);
      for (const k of c.misbeliefs) bits.push(`  ЗАБЛУЖДАЕТСЯ: ${k.secret_title ?? k.topic}${k.belief ? ` — верит: ${k.belief}` : ''}`);
      for (const k of c.unaware) bits.push(`  НЕ ЗНАЕТ: ${k.secret_title ?? k.topic}`);
      for (const f of c.facts) bits.push(`  [${f.kind}] ${f.value}`);
      lines.push(bits.join('\n'));
      lines.push('');
    }

    lines.push('=== СВЯЗИ ===');
    for (const r of data.relationships) lines.push(`${r.from_name} → ${r.to_name}: ${r.state ?? '—'}`);

    if (data.plot.length) {
      lines.push('', '=== НЕЗАКРЫТОЕ ===');
      for (const f of data.plot) lines.push(`— ${f.value}`);
    }
    if (data.open_promises.length) {
      lines.push('', '=== РУЖЬЯ В ОЖИДАНИИ ===');
      for (const p of data.open_promises) lines.push(`— ${p.title} (посеяно гл.${p.planted_ch ?? '?'})`);
    }
    return lines.join('\n');
  }

  function buildPrompt() {
    if (!data) return;
    showText('Срез мира', stateAsText(), {
      hint: 'Это состояние на выбранный момент. Отдай модели вместо пересказа предыдущих глав.',
    });
  }

  /** Разбор написанной главы на типизированные факты. */
  async function extractFacts() {
    const written = state.chapters.filter((c) => c.status === 'написано');
    if (!written.length) return toast('Нет написанных глав', true);

    const answer = await prompt(
      'Из какой главы извлекать факты?',
      { value: String(written.at(-1).number), ok: 'Собрать промпт' });
    if (!answer) return;

    const chapter = written.find((c) => c.number === Number(answer));
    if (!chapter) return toast('Глава не найдена', true);

    const text = (await api.get(`/chapters/${chapter.id}/text`)).body ?? '';
    const before = await api.get(`/books/${state.book.id}/state?chapter=${chapter.number - 1}`);

    const known = before.characters
      .filter((c) => c.facts.length || c.knows.length)
      .map((c) => `${c.name}: ` + [
        ...c.facts.map((f) => `[${f.kind}] ${f.value}`),
        ...c.knows.map((k) => `ЗНАЕТ ${k.secret_title ?? k.topic}`),
      ].join(' · ')).join('\n');

    const chars = state.characters.map((c) => `${c.id} — ${c.name}`).join('\n');

    const p = [
      `Разбери главу книги «${state.book.title}» на факты для базы состояния мира.`,
      `Глава ${chapter.number}. «${chapter.title ?? ''}»`,
      '',
      'Нужны только НОВЫЕ или ИЗМЕНИВШИЕСЯ факты — то, чего не было в состоянии до главы.',
      '',
      'ФОРМАТ ОТВЕТА — только JSON, без пояснений:',
      '{"facts":[{"character_id":3,"kind":"статус","value":"в шоке после случившегося у оврага"}],',
      ' "knowledge":[{"character_id":5,"stance":"заблуждается","topic":"пекарь неравнодушен к Клио","belief":"он на неё засматривается"}],',
      ' "closed":[{"character_id":3,"was":"старый факт, который перестал быть верным"}]}',
      '',
      'Виды фактов (kind): статус, место, черта, внешность, предмет, имя, эмоция, цель, состав, событие.',
      'Позиции знания (stance): знает, не знает, заблуждается, подозревает.',
      '',
      'Правила:',
      '— один факт = одно утверждение, не абзац;',
      '— не повторяй то, что уже было верно до главы;',
      '— заблуждение фиксируй отдельно от незнания: персонаж действует исходя из ложного;',
      '— предметы при себе тоже факт: они потом выстреливают.',
      '',
      '=== ПЕРСОНАЖИ (id — имя) ===',
      chars,
      '',
      '=== СОСТОЯНИЕ ДО ГЛАВЫ ===',
      known || '(пусто)',
      '',
      '=== ТЕКСТ ГЛАВЫ ===',
      text,
    ].join('\n');

    showText(`Промпт: разбор главы ${chapter.number} на факты`, p, {
      hint: 'Отдай модели, потом вернись и нажми «Извлечь факты» ещё раз — предложит вставить ответ.',
    });

    const raw = await pasteBox('Ответ модели: факты', {
      hint: 'Вставь JSON с полями facts / knowledge / closed. Можно закрыть и вставить позже.',
      ok: 'Разобрать',
    });
    if (!raw) return;

    let payload;
    try {
      payload = JSON.parse(raw.trim().replace(/^```(?:json)?\s*|\s*```$/g, ''));
    } catch { return toast('Не похоже на JSON', true); }

    const facts = payload.facts ?? [];
    const knows = payload.knowledge ?? [];
    if (!facts.length && !knows.length) return toast('В ответе нет фактов', true);

    const preview = [
      ...facts.map((f) => `+ [${f.kind}] ${nameOf(f.character_id)}: ${f.value}`),
      ...knows.map((k) => `? ${nameOf(k.character_id)} — ${k.stance}: ${k.topic ?? ''}`),
    ].join('\n');

    if (!(await confirm(`Добавить ${facts.length + knows.length} записей за главу ${chapter.number}?`))) return;

    let ok = 0;
    for (const f of facts) {
      if (!f.value) continue;
      await api.post(`/books/${state.book.id}/facts`, {
        subject_type: 'character', subject_id: f.character_id ?? null,
        kind: f.kind || 'событие', value: f.value,
        from_chapter: chapter.number, source: 'ИИ',
      });
      ok++;
    }
    for (const k of knows) {
      if (!k.topic && !k.secret_id) continue;
      await api.post(`/books/${state.book.id}/knowledge`, {
        character_id: k.character_id ?? null, secret_id: k.secret_id ?? null,
        topic: k.topic ?? null, stance: k.stance || 'знает', belief: k.belief ?? null,
        from_chapter: chapter.number, source: 'ИИ',
      });
      ok++;
    }

    toast(`Добавлено записей: ${ok}`);
    showText('Что добавлено', preview, { hint: 'Всё можно поправить в кодексе персонажа.' });
    await load();
  }

  const nameOf = (id) => state.characters.find((c) => c.id === id)?.name ?? `#${id}`;

  // ═══════════ запуск ═══════════

  (async () => {
    moments = await api.get(`/books/${state.book.id}/moments`);
    if (!moments.length) {
      body.append(el('div.empty', {}, 'Линии времени нет. Переимпортируй книгу: npm run import'));
      return;
    }
    // по умолчанию — конец написанного
    const lastWritten = Math.max(...state.chapters
      .filter((c) => c.status === 'написано').map((c) => c.number), 1);
    idx = Math.max(0, moments.findLastIndex((m) =>
      String(m.chapters ?? '').split(',').includes(String(lastWritten))));
    slider.max = moments.length - 1;
    slider.value = idx;
    load();
  })();
}

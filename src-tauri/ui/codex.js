/** Кодекс: карточки персонажей, всё редактируется на месте. */
import { api, el, clear, toast, confirm, prompt, editable } from './lib.js';

export function codexView(root, state) {
  let current = null;
  let dict = null;      // дерево видов фактов
  let atChapter = null; // на какую главу смотрим
  const list = el('div.codex-list');
  const body = el('div.codex-body');
  root.append(el('div', { id: 'codex' }, list, body));

  function renderList() {
    clear(list);
    list.append(el('div.item', {
      style: { color: 'var(--accent)' },
      onclick: async () => {
        const name = await prompt('Имя нового персонажа', { ok: 'Создать' });
        if (!name) return;
        const c = await api.post(`/books/${state.book.id}/characters`, { name });
        state.characters = await api.get(`/books/${state.book.id}/characters`);
        renderList();
        open(c.id);
      },
    }, '+ Персонаж'));

    for (const c of state.characters) {
      list.append(el('div.item', {
        class: 'item' + (current?.id === c.id ? ' on' : ''),
        onclick: () => open(c.id),
      },
        el('b', {}, (c.is_main ? '★ ' : '') + c.name),
        el('small.dim', {}, ` ${c.state_count} фактов`)
      ));
    }
  }

  async function open(id) {
    current = await api.get(`/characters/${id}`);
    dict ??= await api.get('/dictionaries');
    current.state = await api.get(
      `/characters/${id}/state${atChapter ? `?chapter=${atChapter}` : ''}`);
    renderList();
    renderBody();
  }

  const save = async (patch) => {
    const updated = await api.patch(`/characters/${current.id}`, patch);
    Object.assign(current, updated);
    state.characters = await api.get(`/books/${state.book.id}/characters`);
    renderList();
  };

  function renderBody() {
    clear(body);
    const c = current;
    if (!c) { body.append(el('div.empty', {}, 'Выбери персонажа')); return; }

    body.append(
      el('h2', {}, c.name),
      el('div.dim', { style: { marginBottom: '18px' } },
        [c.status, c.age, c.gender, c.group_name].filter(Boolean).join(' · ') || '—')
    );

    // Основное
    body.append(el('section', {},
      el('h4', {}, 'Основное'),
      editable('Имя', c.name, (v) => save({ name: v })),
      el('div', { style: { display: 'flex', gap: '10px' } },
        editable('Статус', c.status, (v) => save({ status: v })),
        editable('Возраст', c.age, (v) => save({ age: v })),
        editable('Пол', c.gender, (v) => save({ gender: v }))),
      editable('Группа', c.group_name, (v) => save({ group_name: v })),
      el('label.field', {},
        el('span', {}, 'Главный герой'),
        el('input', {
          type: 'checkbox', style: { width: 'auto' },
          ...(c.is_main ? { checked: 'checked' } : {}),
          onchange: (e) => save({ is_main: e.target.checked ? 1 : 0 }),
        }))
    ));

    // Характер и голос
    body.append(el('section', {},
      el('h4', {}, 'Характер'),
      editable('Текущий характер', c.summary, (v) => save({ summary: v }), { multiline: true, rows: 4 }),
      editable('История изменений', c.character_history, (v) => save({ character_history: v }),
        { multiline: true, rows: 3 })
    ));

    body.append(el('section', {},
      el('h4', {}, 'Голос'),
      editable('Манера речи', c.voice, (v) => save({ voice: v }), { multiline: true, rows: 3 }),
      editable('Ограничения', c.voice_rules, (v) => save({ voice_rules: v }), { multiline: true, rows: 3 })
    ));

    body.append(el('section', {},
      el('h4', {}, 'Атмосфера'),
      el('div.dim', { style: { fontSize: '12px', marginBottom: '8px' } },
        'Пока персонаж в сцене, повествование окрашивается его атмосферой'),
      editable('Описание', c.atmosphere_desc, (v) => save({ atmosphere_desc: v })),
      editable('Влияние (0–5)', c.atmosphere_level,
        (v) => save({ atmosphere_level: Math.max(0, Math.min(5, Number(v) || 0)) }), { type: 'number' })
    ));

    if (c.knows) {
      body.append(el('section', {},
        el('h4', {}, 'Знает / не знает'),
        editable('', c.knows, (v) => save({ knows: v }), { multiline: true, rows: 4 })));
    }

    // Досье: факты по разделам на выбранный момент
    body.append(dossier(c));

    // Эволюция по главам
    body.append(el('section', {},
      el('h4', {}, `Ключевые события · ${c.states.length}`),
      ...c.states.map((s) => el('div.tl-row', {},
        el('span.ch', {}, s.chapter_no ? `гл.${s.chapter_no}` : '—'),
        el('span', { style: { flex: 1 } }, s.text),
        s.hidden ? el('span.pill.hid', {}, 'скрыто') : null,
        el('button.btn.danger', {
          style: { fontSize: '11px', padding: '1px 7px' },
          onclick: async () => {
            if (!(await confirm('Удалить факт?'))) return;
            await api.del(`/character-states/${s.id}`);
            open(c.id);
          },
        }, '×')
      )),
      el('button.btn', {
        style: { marginTop: '8px' },
        onclick: async () => {
          const text = await prompt('Новый факт', {
            multiline: true, placeholder: 'гл.8: что произошло с персонажем', ok: 'Добавить',
          });
          if (!text) return;
          const m = text.match(/гл\.?\s*(\d+)/i);
          await api.post(`/characters/${c.id}/states`, {
            text, chapter_no: m ? Number(m[1]) : null, hidden: /🔒|за кадром/i.test(text) ? 1 : 0,
          });
          open(c.id);
        },
      }, '+ Факт')
    ));

    // Отношения
    body.append(el('section', {},
      el('h4', {}, 'Отношения'),
      ...(c.relationships.length
        ? c.relationships.map((r) => el('div.tl-row', {},
            el('span', { style: { minWidth: '110px', color: 'var(--accent)' } }, r.to_name),
            el('span', { style: { flex: 1 } }, r.current_state),
            el('button.btn.danger', {
              style: { fontSize: '11px', padding: '1px 7px' },
              onclick: async () => {
                if (!(await confirm('Удалить связь?'))) return;
                await api.del(`/relationships/${r.id}`);
                open(c.id);
              },
            }, '×')))
        : [el('div.dim', {}, 'Связей нет')]),
      addRelationship(c)
    ));

    // Где участвует
    if (c.events.length) {
      body.append(el('section', {},
        el('h4', {}, `Появления · ${c.events.length}`),
        ...c.events.map((e) => el('div.tl-row', {},
          el('span.ch', {}, e.chapter_no ? `гл.${e.chapter_no}` : '—'),
          el('div', { style: { flex: 1 } },
            el('div', {}, e.title),
            e.state_snapshot ? el('div.dim', { style: { fontSize: '12px' } }, e.state_snapshot) : null)
        ))));
    }

    body.append(el('div', { style: { margin: '24px 0 40px' } },
      el('button.btn.danger', {
        onclick: async () => {
          if (!(await confirm(`Удалить персонажа «${c.name}»?`))) return;
          await api.del(`/characters/${c.id}`);
          state.characters = await api.get(`/books/${state.book.id}/characters`);
          current = null;
          renderList();
          renderBody();
        },
      }, 'Удалить персонажа')));
  }

  /**
   * Досье персонажа: факты, разложенные по разделам дерева.
   * Показывается состояние на выбранную главу — то, что верно именно тогда.
   */
  function dossier(c) {
    const st = current.state;
    const chapters = state.chapters.map((x) => x.number);

    const chapterSel = el('select', {
      style: { maxWidth: '150px' },
      onchange: async (e) => {
        atChapter = e.target.value ? Number(e.target.value) : null;
        await open(c.id);
      },
    },
      el('option', { value: '' }, 'последняя написанная'),
      ...chapters.map((n) => el('option', {
        value: n, ...(n === atChapter ? { selected: 'selected' } : {}),
      }, `на главу ${n}`))
    );

    const sec = el('section', {},
      el('div', { style: { display: 'flex', alignItems: 'center', gap: '10px', marginBottom: '10px' } },
        el('h4', { style: { margin: 0 } }, 'Досье'),
        chapterSel,
        el('span.dim', { style: { fontSize: '12px' } }, st?.at?.label ?? ''),
        el('span.grow', { style: { flex: 1 } }),
        addFactButton(c)
      )
    );

    if (!st?.branches?.length) {
      sec.append(el('div.dim', { style: { fontSize: '13px' } },
        'Фактов на этот момент нет. Добавь первый или разбери главу на факты во вкладке «Состояние».'));
      return sec;
    }

    for (const b of st.branches) {
      const rows = [];
      for (const k of b.kinds) {
        for (const f of k.items) {
          rows.push(el('div.dos-row', {},
            el('span.dos-slot', {}, k.title + (f.key ? ` · ${f.key}` : '')),
            el('span.dos-val', {}, f.value),
            el('span.dim', { style: { fontSize: '11px', whiteSpace: 'nowrap' } },
              f.from_chapter ? `гл.${f.from_chapter}` : ''),
            el('button.btn.danger', {
              style: { fontSize: '11px', padding: '1px 6px' },
              title: 'Удалить факт',
              onclick: async () => { await api.del(`/facts/${f.id}`); await open(c.id); },
            }, '×')
          ));
        }
      }
      sec.append(el('div.dos-branch', {}, el('div.dos-head', {}, b.title), ...rows));
    }

    // осведомлённость — отдельно от умений, у неё своя позиция
    const a = st.awareness ?? {};
    const groups = [
      ['знает', a.knows, 'ok'],
      ['подозревает', a.suspects, 'warn'],
      ['заблуждается', a.misbeliefs, 'warn'],
      ['не знает', a.unaware, 'plan'],
    ].filter(([, v]) => v?.length);

    if (groups.length) {
      const rows = groups.map(([label, items, cls]) =>
        el('div.dos-row', {},
          el('span.dos-slot', {}, el('span', { class: `pill ${cls}` }, label)),
          el('span.dos-val', {},
            items.map((k) => k.secret_title ?? k.topic ?? '—').join(' · '))
        ));
      sec.append(el('div.dos-branch', {}, el('div.dos-head', {}, 'Осведомлённость'), ...rows));
    }

    return sec;
  }

  /** Добавление факта: выбираем вид из дерева, слот подсказывается словарём. */
  function addFactButton(c) {
    return el('button.btn', {
      onclick: async () => {
        const kinds = dict.fact_kinds.filter((k) => k.parent); // только листья
        const kindSel = el('select', {},
          ...kinds.map((k) => el('option', { value: k.code },
            `${k.title}  (${dict.fact_kinds.find((p) => p.code === k.parent)?.title ?? ''})`)));
        const keyInput = el('input', { type: 'text', placeholder: 'слот — необязательно' });
        const valInput = el('textarea', { rows: 3, placeholder: 'что именно' });
        const chInput = el('input', {
          type: 'number', placeholder: 'с какой главы',
          value: atChapter ?? state.chapters.filter((x) => x.status === 'написано').at(-1)?.number ?? 1,
        });

        const hint = el('div.dim', { style: { fontSize: '11px', marginTop: '-6px' } });
        const syncHint = () => {
          const k = kinds.find((x) => x.code === kindSel.value);
          hint.textContent = k?.key_hint ? `обычные слоты: ${k.key_hint}` : '';
        };
        kindSel.addEventListener('change', syncHint);
        syncHint();

        const close = (ok) => { bg.remove(); if (ok) save(); };
        const save = async () => {
          if (!valInput.value.trim()) return;
          await api.post(`/books/${state.book.id}/facts`, {
            subject_type: 'character', subject_id: c.id,
            kind: kindSel.value, key: keyInput.value.trim() || null,
            value: valInput.value.trim(), from_chapter: Number(chInput.value) || null,
          });
          await open(c.id);
          toast('Факт добавлен');
        };

        const bg = el('div.modal-bg', { onclick: (e) => e.target === bg && close(false) },
          el('div.modal', {},
            el('h3', {}, 'Новый факт'),
            el('label.field', {}, el('span', {}, 'Вид'), kindSel), hint,
            el('label.field', {}, el('span', {}, 'Слот'), keyInput),
            el('label.field', {}, el('span', {}, 'Значение'), valInput),
            el('label.field', {}, el('span', {}, 'С какой главы'), chInput),
            el('div.row', {},
              el('button.btn', { onclick: () => close(false) }, 'Отмена'),
              el('button.btn.primary', { onclick: () => close(true) }, 'Добавить'))
          ));
        document.body.append(bg);
        valInput.focus();
      },
    }, '+ Факт');
  }

  function addRelationship(c) {
    const sel = el('select', {
      style: { maxWidth: '260px', marginTop: '8px' },
      onchange: async (e) => {
        if (!e.target.value) return;
        const state_text = await prompt('Что между ними?', { ok: 'Добавить' });
        if (state_text === null) { e.target.value = ''; return; }
        await api.post(`/books/${state.book.id}/relationships`, {
          from_char: c.id, to_char: Number(e.target.value), current_state: state_text,
        });
        open(c.id);
      },
    }, el('option', { value: '' }, '+ добавить связь'),
       ...state.characters
         .filter((x) => x.id !== c.id && !c.relationships.some((r) => r.to_id === x.id))
         .map((x) => el('option', { value: x.id }, x.name)));
    return sel;
  }

  renderList();
  if (state.characters.length) open(state.characters[0].id);
}

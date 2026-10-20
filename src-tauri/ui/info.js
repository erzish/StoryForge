/** Справочник: то, по чему удобно читать — ружья, тайны, таймлайн, мир. */
import { api, el, clear, num } from './lib.js';

export function infoView(root, state) {
  const tabs = el('div', { style: { display: 'flex', gap: '4px', padding: '12px 20px 0' } });
  const body = el('div', { style: { padding: '14px 22px 40px' } });
  root.append(el('div.scroll', {}, tabs, body));

  const SECTIONS = {
    'Ружья': promises,
    'Тайны': secrets,
    'Таймлайн': timeline,
    'Мир': world,
    'События': events,
  };

  Object.keys(SECTIONS).forEach((name, i) => {
    const b = el('button.btn', {
      onclick: () => {
        [...tabs.children].forEach((x) => x.classList.remove('primary'));
        b.classList.add('primary');
        SECTIONS[name]();
      },
    }, name);
    if (i === 0) b.classList.add('primary');
    tabs.append(b);
  });

  const table = (head, rows) =>
    el('table', {},
      el('thead', {}, el('tr', {}, ...head.map((h) => el('th', {}, h)))),
      el('tbody', {}, ...rows));

  async function promises() {
    const rows = await api.get(`/books/${state.book.id}/promises`);
    clear(body).append(table(['#', 'Что заявлено', 'Гл.', 'Должно выстрелить', 'Статус'],
      rows.map((r) => {
        const fired = /выстрелило/i.test(r.status);
        return el('tr', {},
          el('td.dim', {}, r.ext_no ?? ''),
          el('td', {}, r.title),
          el('td.dim', {}, r.planted_ch ?? '—'),
          el('td.dim', {}, r.expected_payoff ?? ''),
          el('td', {}, el('span', { class: `pill ${fired ? 'ok' : 'warn'}` }, r.status)));
      })));
  }

  async function secrets() {
    const rows = await api.get(`/books/${state.book.id}/secrets`);
    clear(body);
    for (const s of rows) {
      body.append(el('section', { style: {
        background: 'var(--panel)', border: '1px solid var(--line)',
        borderRadius: '8px', padding: '12px 14px', marginBottom: '10px', maxWidth: '860px',
      } },
        el('div', { style: { fontWeight: '600', marginBottom: '8px' } }, s.title),
        el('div', {}, ...s.knowledge.map((k) =>
          el('span', {
            class: `pill ${k.stance === 'знает' ? 'ok' : 'plan'}`,
            style: { marginRight: '5px' },
          }, `${k.who} — ${k.stance}`))),
        el('div.dim', { style: { marginTop: '8px', fontSize: '12px' } },
          `читатель: ${s.reader_knows ?? '—'} · раскроется: ${s.reveal_plan ?? '—'}`)
      ));
    }
  }

  async function timeline() {
    const rows = await api.get(`/books/${state.book.id}/timeline`);
    clear(body);
    for (const r of rows) {
      body.append(el('div.tl-row', {},
        el('span.ch', {}, r.chapter_no ? `гл.${r.chapter_no}` : '—'),
        el('span.dim', { style: { minWidth: '190px', fontSize: '12px' } }, r.story_date ?? ''),
        el('span', { style: { flex: 1 } }, r.description),
        r.hidden ? el('span.pill.hid', {}, 'скрыто') : null,
        el('span.dim', { style: { fontSize: '12px' } }, r.who ?? '')));
    }
  }

  async function world() {
    const w = await api.get(`/books/${state.book.id}/world`);
    clear(body);

    body.append(el('h4', { style: { color: 'var(--dim)', margin: '0 0 8px' } }, 'Правила мира'));
    for (const r of w.rules) {
      body.append(el('div.tl-row', {},
        r.locked ? el('span', { title: 'нерушимое' }, '🔒') : el('span', {}, ' '),
        el('span', { style: { flex: 1 } }, r.text)));
    }

    if (w.hidden.length) {
      body.append(el('h4', { style: { color: 'var(--dim)', margin: '22px 0 8px' } }, 'Скрытые линии'));
      for (const h of w.hidden) {
        body.append(el('section', { style: {
          background: 'var(--panel)', border: '1px solid var(--line)',
          borderRadius: '8px', padding: '12px 14px', marginBottom: '10px', maxWidth: '860px',
        } },
          el('div', { style: { fontWeight: 600 } }, `${h.code} — ${h.name}`),
          el('div.dim', { style: { marginTop: '6px', fontSize: '13px' } }, h.truth ?? ''),
          el('div.dim', { style: { marginTop: '6px', fontSize: '12px' } }, h.status ?? '')));
      }
    }

    if (w.locations.length) {
      body.append(el('h4', { style: { color: 'var(--dim)', margin: '22px 0 8px' } }, 'Локации'));
      for (const l of w.locations) {
        body.append(el('div.tl-row', {},
          el('span', { style: { minWidth: '130px', color: 'var(--accent)' } }, l.name),
          el('span.dim', { style: { flex: 1 } }, (l.description ?? '').slice(0, 160))));
      }
    }

    body.append(el('h4', { style: { color: 'var(--dim)', margin: '22px 0 8px' } },
      `Глоссарий · ${w.glossary.length}`));
    body.append(table(['Термин', 'Значение', 'Раздел'],
      w.glossary.map((g) => el('tr', {},
        el('td', {}, g.term), el('td.dim', {}, g.meaning ?? ''), el('td.dim', {}, g.section ?? '')))));
  }

  async function events() {
    const rows = await api.get(`/books/${state.book.id}/events`);
    clear(body).append(table(['Гл.', 'Событие', 'Участники', 'Статус'],
      rows.map((r) => el('tr', {},
        el('td.dim', {}, r.chapter_no ?? '—'),
        el('td', {},
          el('div', {}, (r.hidden ? '🔒 ' : '') + r.title),
          el('div.dim', { style: { fontSize: '12px' } }, (r.summary ?? '').slice(0, 150))),
        el('td.dim', { style: { fontSize: '12px' } }, r.participants ?? ''),
        el('td', {}, el('span', { class: `pill ${r.status === 'написано' ? 'ok' : 'plan'}` }, r.status))))));
  }

  promises();
}

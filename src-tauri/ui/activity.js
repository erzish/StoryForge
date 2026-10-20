/**
 * Лента активности ИИ над книгой — что реально записано через MCP.
 *
 * Здесь — не переписка, а факт действия через MCP: какой факт записан,
 * какая глава сохранена, какое ружьё выстрелило. Каждая строка — один
 * успешный вызов пишущего MCP-инструмента (server/lib/mcpCore.js).
 */
import { api, el, clear, toast } from './lib.js';

const TOOL_ICON = {
  add_fact: '+',
  close_fact: '–',
  add_knowledge: '?',
  write_chapter_text: '✎',
  add_promise: '☆',
  update_promise: '☆',
  run_import: '↻',
};

function timeAgo(sqliteDatetime) {
  // sqlite datetime('now') отдаёт UTC без 'T'/'Z' — достраиваем перед парсингом
  const then = new Date(String(sqliteDatetime).replace(' ', 'T') + 'Z');
  const diffSec = Math.round((Date.now() - then.getTime()) / 1000);
  if (diffSec < 5) return 'только что';
  if (diffSec < 60) return `${diffSec} сек назад`;
  if (diffSec < 3600) return `${Math.floor(diffSec / 60)} мин назад`;
  if (diffSec < 86400) return `${Math.floor(diffSec / 3600)} ч назад`;
  return then.toLocaleString('ru-RU', { day: 'numeric', month: 'short', hour: '2-digit', minute: '2-digit' });
}

export function activityView(root, state) {
  const list = el('div.activity-list');

  const bar = el('div.time-bar', {},
    el('div', { style: { flex: 1 } },
      el('b', {}, 'Активность ИИ'), ' ',
      el('span.dim', {}, '— что записано через MCP')),
    el('button.btn', { onclick: load }, 'Обновить'));

  root.append(el('div', { id: 'activity' }, bar, list));

  async function load() {
    clear(list);
    let rows;
    try {
      rows = await api.get(`/books/${state.book.id}/activity`);
    } catch (e) {
      toast('Не удалось загрузить: ' + e.message, true);
      return;
    }
    if (!rows.length) {
      list.append(el('div.empty', {},
        'Пока пусто.', el('br'),
        'Как только модель через MCP запишет факт, сохранит главу или отметит ружьё — здесь появится строка.'));
      return;
    }
    for (const r of rows) {
      list.append(el('div.tl-row', { title: r.tool },
        el('span.ch', {}, TOOL_ICON[r.tool] ?? '•'),
        el('span', { style: { flex: 1 } }, r.summary),
        el('span.dim', { style: { fontSize: '12px', minWidth: '110px', textAlign: 'right' } }, timeAgo(r.created_at))));
    }
  }

  load();
}

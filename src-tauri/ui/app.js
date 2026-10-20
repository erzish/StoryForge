import { api, el, clear, num, toast, showText } from './lib.js';
import { graphView } from './graph.js';
import { editorView } from './editor.js';
import { codexView } from './codex.js';
import { infoView } from './info.js';
import { stateView } from './statev.js';
import { activityView } from './activity.js';
import { aiView } from './ai.js';

const view = document.getElementById('view');
const nav = document.getElementById('nav');
const importBtn = document.getElementById('import-btn');

const state = {
  book: null,
  chapters: [],
  characters: [],
  currentChapter: null,
  openEditor(chapterId) {
    show('Текст', chapterId);
  },
};

const VIEWS = {
  'Граф': graphView,
  'Текст': editorView,
  'Состояние': stateView,
  'Персонажи': codexView,
  'Активность': activityView,
  'ИИ': aiView,
  'Справка': infoView,
};

async function show(name, chapterId = null) {
  if (state.beforeLeave && !(await state.beforeLeave())) return;
  state.beforeLeave = null;
  state.editorDirty = null;
  if (chapterId != null) state.currentChapter = chapterId;
  clear(view);
  for (const b of nav.children) b.classList.toggle('on', b.textContent === name);
  try {
    VIEWS[name](view, state);
  } catch (e) {
    view.append(el('div.empty', {}, 'Ошибка: ' + e.message));
    console.error(e);
  }
}

window.addEventListener('beforeunload', (e) => {
  if (!state.editorDirty?.()) return;
  e.preventDefault();
  e.returnValue = '';
});

/** Импорт всех книг рядом со StoryForge — то же, что `npm run import` без аргументов. */
async function runImport() {
  if (state.beforeLeave && !(await state.beforeLeave())) return;
  state.beforeLeave = null;
  state.editorDirty = null;
  const label = importBtn.textContent;
  importBtn.disabled = true;
  importBtn.textContent = 'Импортирую…';
  try {
    const { books } = await api.post('/import-all');
    const lines = books.map((b) => {
      const stats = Object.entries(b.stats).map(([k, v]) => `${k}: ${v}`).join('  ·  ');
      const warn = b.warnings.length ? `\n  ⚠ предупреждений: ${b.warnings.length}\n  ` + b.warnings.slice(0, 20).join('\n  ') : '';
      return `${b.title}\n  ${stats}${warn}`;
    });
    toast(`Импортировано книг: ${books.length}`);
    showText('Импорт из markdown', lines.join('\n\n'));
    await boot();
  } catch (e) {
    toast('Ошибка импорта: ' + e.message, true);
  } finally {
    importBtn.disabled = false;
    importBtn.textContent = label;
  }
}

importBtn.addEventListener('click', runImport);

async function boot() {
  let books;
  try {
    books = await api.get('/books');
  } catch (e) {
    view.append(el('div.empty', {}, 'Сервер не отвечает: ' + e.message));
    return;
  }

  if (!books.length) {
    clear(view);
    view.append(el('div.empty', {},
      'В базе пусто.', el('br'), el('br'),
      el('button.btn.primary', { onclick: runImport }, 'Импортировать книги')));
    return;
  }

  state.book = await api.get('/books/' + books[0].id);
  state.chapters = await api.get(`/books/${state.book.id}/chapters`);
  state.characters = await api.get(`/books/${state.book.id}/characters`);

  document.getElementById('book-title').textContent = state.book.title;
  document.getElementById('book-meta').textContent =
    `${state.book.written.n} глав · ${num(state.book.written.chars)} знаков`;

  if (!nav.children.length) {
    for (const name of Object.keys(VIEWS)) {
      nav.append(el('button', { onclick: () => show(name) }, name));
    }
  }
  show(document.querySelector('#nav button.on')?.textContent || 'Граф');
}

boot();

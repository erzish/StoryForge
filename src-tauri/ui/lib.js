/** Общие мелочи: запросы, создание элементов, уведомления, диалоги. */

export const api = {
  async req(method, path, body) {
    const res = await fetch('/api' + path, {
      method,
      headers: body ? { 'Content-Type': 'application/json' } : undefined,
      body: body ? JSON.stringify(body) : undefined,
    });
    const data = await res.json().catch(() => null);
    if (!res.ok) throw new Error(data?.error || `Ошибка ${res.status}`);
    return data;
  },
  get: (p) => api.req('GET', p),
  post: (p, b) => api.req('POST', p, b),
  patch: (p, b) => api.req('PATCH', p, b),
  del: (p) => api.req('DELETE', p),
};

/** el('div.card', {onclick}, 'текст', child) */
export function el(spec, props = {}, ...children) {
  const [tag, ...classes] = spec.split('.');
  const node = document.createElement(tag || 'div');
  if (classes.length) node.className = classes.join(' ');
  for (const [k, v] of Object.entries(props ?? {})) {
    if (v === null || v === undefined || v === false) continue;
    if (k === 'html') node.innerHTML = v;
    else if (k.startsWith('on') && typeof v === 'function') node.addEventListener(k.slice(2), v);
    else if (k === 'style' && typeof v === 'object') Object.assign(node.style, v);
    else if (k === 'value') node.value = v;
    else node.setAttribute(k, v);
  }
  for (const c of children.flat()) {
    if (c === null || c === undefined || c === false) continue;
    node.append(c instanceof Node ? c : document.createTextNode(String(c)));
  }
  return node;
}

export const clear = (node) => { while (node.firstChild) node.firstChild.remove(); return node; };
export const num = (n) => Number(n ?? 0).toLocaleString('ru-RU');

let toastTimer;
export function toast(text, isError = false) {
  document.querySelector('.toast')?.remove();
  const t = el('div.toast', { style: isError ? { borderColor: 'var(--bad)' } : {} }, text);
  document.body.append(t);
  clearTimeout(toastTimer);
  toastTimer = setTimeout(() => t.remove(), 2600);
}

/** Модальное окно с полем ввода. Возвращает строку или null. */
export function prompt(title, { value = '', multiline = false, placeholder = '', ok = 'Сохранить' } = {}) {
  return new Promise((resolve) => {
    const input = multiline
      ? el('textarea', { rows: 5, placeholder, value })
      : el('input', { type: 'text', placeholder, value });

    const close = (result) => { bg.remove(); document.removeEventListener('keydown', onKey); resolve(result); };
    const onKey = (e) => {
      if (e.key === 'Escape') close(null);
      if (e.key === 'Enter' && (!multiline || e.ctrlKey)) { e.preventDefault(); close(input.value); }
    };

    const bg = el('div.modal-bg', { onclick: (e) => e.target === bg && close(null) },
      el('div.modal', {},
        el('h3', {}, title),
        input,
        el('div.row', {},
          el('button.btn', { onclick: () => close(null) }, 'Отмена'),
          el('button.btn.primary', { onclick: () => close(input.value) }, ok)
        )
      )
    );
    document.body.append(bg);
    document.addEventListener('keydown', onKey);
    input.focus();
    input.select?.();
  });
}

/** Показать длинный текст с кнопкой «Копировать». */
export function showText(title, text, { hint = '' } = {}) {
  const area = el('textarea', { rows: 16, readonly: 'readonly', value: text });
  const close = () => bg.remove();

  const copy = el('button.btn.primary', {
    onclick: async () => {
      try {
        await navigator.clipboard.writeText(text);
      } catch {
        area.select();
        document.execCommand('copy');
      }
      copy.textContent = 'Скопировано ✓';
      setTimeout(() => { copy.textContent = 'Копировать'; }, 1600);
    },
  }, 'Копировать');

  const bg = el('div.modal-bg', { onclick: (e) => e.target === bg && close() },
    el('div.modal.wide', {},
      el('h3', {}, title),
      hint ? el('div.dim', { style: { fontSize: '12px', marginBottom: '8px' } }, hint) : null,
      area,
      el('div.row', {}, el('button.btn', { onclick: close }, 'Закрыть'), copy)
    )
  );
  document.body.append(bg);
  area.focus();
}

/** Модалка для вставки ответа модели. Возвращает строку или null. */
export function pasteBox(title, { hint = '', ok = 'Применить' } = {}) {
  return new Promise((resolve) => {
    const area = el('textarea', { rows: 14, placeholder: '{"edits":[…]}' });
    const close = (r) => { bg.remove(); resolve(r); };
    const bg = el('div.modal-bg', { onclick: (e) => e.target === bg && close(null) },
      el('div.modal.wide', {},
        el('h3', {}, title),
        hint ? el('div.dim', { style: { fontSize: '12px', marginBottom: '8px' } }, hint) : null,
        area,
        el('div.row', {},
          el('button.btn', { onclick: () => close(null) }, 'Отмена'),
          el('button.btn.primary', { onclick: () => close(area.value) }, ok)
        )
      )
    );
    document.body.append(bg);
    area.focus();
  });
}

export function confirm(text, { ok = 'Удалить' } = {}) {
  return new Promise((resolve) => {
    const close = (r) => { bg.remove(); resolve(r); };
    const bg = el('div.modal-bg', { onclick: (e) => e.target === bg && close(false) },
      el('div.modal', {},
        el('h3', {}, text),
        el('div.row', {},
          el('button.btn', { onclick: () => close(false) }, 'Отмена'),
          el('button.btn.danger', { onclick: () => close(true) }, ok)
        )
      )
    );
    document.body.append(bg);
  });
}

/**
 * Контекстное меню у курсора.
 * items: [{ label, hint, icon, action, disabled }] или '-' для разделителя.
 */
export function contextMenu(x, y, items) {
  closeContextMenu();

  const menu = el('div.ctx-menu');
  for (const it of items) {
    if (it === '-') { menu.append(el('div.ctx-sep')); continue; }
    if (!it) continue;
    menu.append(el('button.ctx-item', {
      disabled: it.disabled === true,
      onclick: (e) => { e.stopPropagation(); closeContextMenu(); it.action?.(); },
    },
      el('span.ctx-ico', {}, it.icon ?? ''),
      el('span.ctx-label', {}, it.label),
      it.hint ? el('span.ctx-hint', {}, it.hint) : null
    ));
  }

  document.body.append(menu);

  // не даём меню вылезти за край экрана
  const r = menu.getBoundingClientRect();
  menu.style.left = Math.min(x, window.innerWidth - r.width - 8) + 'px';
  menu.style.top = Math.min(y, window.innerHeight - r.height - 8) + 'px';

  setTimeout(() => {
    document.addEventListener('mousedown', closeContextMenu, { once: true });
    document.addEventListener('keydown', onCtxKey);
  }, 0);
  return menu;
}

function onCtxKey(e) {
  if (e.key === 'Escape') closeContextMenu();
}

export function closeContextMenu() {
  document.querySelector('.ctx-menu')?.remove();
  document.removeEventListener('keydown', onCtxKey);
}

/** Поле, сохраняющееся при потере фокуса. */
export function editable(label, value, onSave, { multiline = false, rows = 3, type = 'text' } = {}) {
  const input = multiline
    ? el('textarea', { rows, value: value ?? '' })
    : el('input', { type, value: value ?? '' });

  let last = value ?? '';
  const commit = async () => {
    if (input.value === last) return;
    last = input.value;
    try { await onSave(input.value); } catch (e) { toast(e.message, true); }
  };
  input.addEventListener('blur', commit);
  input.addEventListener('keydown', (e) => {
    if (e.key === 'Enter' && !multiline) input.blur();
    if (e.key === 'Enter' && multiline && e.ctrlKey) input.blur();
  });

  return el('label.field', {}, el('span', {}, label), input);
}

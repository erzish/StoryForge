/**
 * Мост между markdown на диске и форматированным текстом в редакторе.
 * Поддерживается ровно то, что встречается в главах: заголовки, жирный,
 * курсив, разделители сцен и абзацы. Ничего лишнего — чтобы обратное
 * преобразование было точным и файл не «распухал» мусорной разметкой.
 */

const escapeHtml = (s) =>
  s.replace(/&/g, '&amp;').replace(/</g, '&lt;').replace(/>/g, '&gt;');

/** Инлайновая разметка внутри абзаца. */
function inlineToHtml(s) {
  return escapeHtml(s)
    .replace(/\*\*(.+?)\*\*/g, '<strong>$1</strong>')
    .replace(/(^|[\s(«—])\*(?!\s)(.+?)(?<!\s)\*/g, '$1<em>$2</em>')
    .replace(/(^|[\s(«—])_(?!\s)(.+?)(?<!\s)_/g, '$1<em>$2</em>');
}

export function mdToHtml(md) {
  const lines = (md ?? '').replace(/\r\n/g, '\n').split('\n');
  const out = [];
  let para = [];

  const flush = () => {
    if (!para.length) return;
    out.push(`<p>${inlineToHtml(para.join(' '))}</p>`);
    para = [];
  };

  for (const raw of lines) {
    const line = raw.trimEnd();

    if (!line.trim()) { flush(); continue; }

    if (/^(---|\*\*\*|___)\s*$/.test(line)) { flush(); out.push('<hr>'); continue; }

    const h = line.match(/^(#{1,6})\s+(.*)$/);
    if (h) {
      flush();
      const level = Math.min(3, h[1].length);
      out.push(`<h${level}>${inlineToHtml(h[2])}</h${level}>`);
      continue;
    }

    para.push(line.trim());
  }
  flush();

  return out.join('\n') || '<p><br></p>';
}

/** Инлайновое содержимое узла → markdown. */
function inlineToMd(node) {
  let out = '';
  for (const child of node.childNodes) {
    if (child.nodeType === Node.TEXT_NODE) {
      out += child.data.replace(/ /g, ' ');
      continue;
    }
    if (child.nodeType !== Node.ELEMENT_NODE) continue;

    const tag = child.tagName;
    if (tag === 'BR') { out += '\n'; continue; }

    const inner = inlineToMd(child);
    if (!inner.trim()) { out += inner; continue; }

    if (tag === 'STRONG' || tag === 'B') out += `**${inner}**`;
    else if (tag === 'EM' || tag === 'I') out += `*${inner}*`;
    else out += inner; // подсветка заметок и прочие обёртки просто снимаются
  }
  return out;
}

export function htmlToMd(root) {
  const blocks = [];

  for (const node of root.childNodes) {
    if (node.nodeType === Node.TEXT_NODE) {
      const t = node.data.trim();
      if (t) blocks.push(t);
      continue;
    }
    if (node.nodeType !== Node.ELEMENT_NODE) continue;

    const tag = node.tagName;
    if (tag === 'HR') { blocks.push('---'); continue; }

    const h = tag.match(/^H([1-6])$/);
    if (h) {
      const text = inlineToMd(node).trim();
      if (text) blocks.push('#'.repeat(Number(h[1])) + ' ' + text);
      continue;
    }

    const text = inlineToMd(node).trim();
    if (text) blocks.push(text);
  }

  return blocks.join('\n\n').replace(/\n{3,}/g, '\n\n').trim() + '\n';
}

/**
 * Плоский текст редактора вместе с картой «где какой текстовый узел».
 *
 * Абзацы разделяются пустой строкой — ровно как в markdown на диске.
 * Это важно: цитата заметки ищется и в редакторе, и в файле, и если
 * разделители не совпадут, выделение через несколько абзацев потеряется.
 */
export function textMap(root) {
  const parts = [];
  let text = '';
  const BLOCK = /^(P|H1|H2|H3|H4|H5|H6|DIV|HR|BLOCKQUOTE|LI)$/;

  const gap = () => {
    if (text && !text.endsWith('\n\n')) text += text.endsWith('\n') ? '\n' : '\n\n';
  };

  const walk = (node) => {
    for (const child of node.childNodes) {
      if (child.nodeType === Node.TEXT_NODE) {
        parts.push({ node: child, start: text.length, end: text.length + child.data.length });
        text += child.data;
        continue;
      }
      if (child.nodeType !== Node.ELEMENT_NODE) continue;

      const tag = child.tagName;

      if (tag === 'HR') { gap(); text += '---'; gap(); continue; }
      if (tag === 'BR') { text += '\n'; continue; }

      const isBlock = BLOCK.test(tag);
      if (isBlock) gap();

      // Проекция повторяет markdown, а не голый текст: иначе цитата с жирным
      // словом выглядела бы как «Клио:», а в файле лежит «**Клио:**»,
      // и заметка теряла бы привязку.
      const heading = tag.match(/^H([1-6])$/);
      if (heading) text += '#'.repeat(Number(heading[1])) + ' ';

      const mark = (tag === 'STRONG' || tag === 'B') ? '**'
        : (tag === 'EM' || tag === 'I') ? '*' : '';
      if (mark) text += mark;

      walk(child);

      if (mark) text += mark;
      if (isBlock) gap();
    }
  };

  // Ничего не обрезаем: смещения в parts должны точно соответствовать text.
  walk(root);
  return { text, parts };
}

/**
 * Диапазон, разбитый по текстовым узлам.
 * Оборачивать в один элемент выделение через несколько абзацев нельзя —
 * это разрушит структуру документа, поэтому подсветка вешается кусочками.
 */
export function rangesForOffsets(root, start, end) {
  const { parts } = textMap(root);
  const out = [];
  for (const p of parts) {
    const s = Math.max(start, p.start);
    const e = Math.min(end, p.end);
    if (s >= e) continue;
    const range = document.createRange();
    range.setStart(p.node, s - p.start);
    range.setEnd(p.node, e - p.start);
    out.push(range);
  }
  return out;
}

/**
 * Поиск фрагмента, терпимый к разнице в пробелах и переносах.
 * Возвращает позиции в исходном тексте и число совпадений.
 */
export function findFlexible(text, needle) {
  if (!needle) return { start: -1, end: -1, count: 0 };

  let count = 0, i = 0, first = -1;
  while ((i = text.indexOf(needle, i)) !== -1) {
    if (first === -1) first = i;
    count++;
    i += needle.length;
  }
  if (count > 0) return { start: first, end: first + needle.length, count };

  const pattern = needle
    .trim()
    .replace(/[.*+?^${}()|[\]\\]/g, '\\$&')
    .replace(/\s+/g, '\\s+');
  try {
    const re = new RegExp(pattern, 'g');
    let m, firstMatch = null, n = 0;
    while ((m = re.exec(text)) !== null) {
      if (!firstMatch) firstMatch = m;
      n++;
      if (m.index === re.lastIndex) re.lastIndex++;
    }
    if (firstMatch) {
      return { start: firstMatch.index, end: firstMatch.index + firstMatch[0].length, count: n };
    }
  } catch { /* слишком экзотическая цитата — сдаёмся */ }

  return { start: -1, end: -1, count: 0 };
}

/** Позиция в плоском тексте → точка в DOM. */
function pointAt(parts, offset) {
  for (const p of parts) {
    if (offset >= p.start && offset <= p.end) {
      return { node: p.node, offset: offset - p.start };
    }
  }
  const last = parts[parts.length - 1];
  return last ? { node: last.node, offset: last.node.data.length } : null;
}

export function rangeFromOffsets(root, start, end) {
  const { parts } = textMap(root);
  const a = pointAt(parts, start);
  const b = pointAt(parts, end);
  if (!a || !b) return null;
  const range = document.createRange();
  range.setStart(a.node, Math.min(a.offset, a.node.data.length));
  range.setEnd(b.node, Math.min(b.offset, b.node.data.length));
  return range;
}

/** Текущее выделение как смещения в плоском тексте редактора. */
export function selectionOffsets(root) {
  const sel = window.getSelection();
  if (!sel || sel.rangeCount === 0 || sel.isCollapsed) return null;
  const range = sel.getRangeAt(0);
  if (!root.contains(range.commonAncestorContainer)) return null;

  const { parts } = textMap(root);
  const offsetOf = (node, off) => {
    const p = parts.find((x) => x.node === node);
    if (p) return p.start + off;
    // выделение упирается в элемент, а не в текст — берём ближайшую границу
    const walkerParts = parts.filter((x) => node.contains?.(x.node));
    if (walkerParts.length) return off === 0 ? walkerParts[0].start : walkerParts.at(-1).end;
    return null;
  };

  const a = offsetOf(range.startContainer, range.startOffset);
  const b = offsetOf(range.endContainer, range.endOffset);
  if (a === null || b === null || a === b) return null;

  const start = Math.min(a, b);
  const end = Math.max(a, b);
  // Берём текст из той же проекции, что и карта: range.toString() склеивает
  // абзацы без переносов, и цитата потом не находится в файле.
  return { start, end, text: textMap(root).text.slice(start, end) };
}

/** Знаки без пробелов — метрика площадки. */
export function charsNoSpace(text) {
  return (text ?? '').split(/\s+/).join('').length;
}

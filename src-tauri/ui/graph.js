/**
 * Граф-блюпринт: главы и события как ноды, связи между ними.
 * Ноды — обычные HTML-элементы поверх SVG-слоя со связями:
 * так проще верстать содержимое, а кривые остаются векторными.
 *
 * Управление как в Figma: клик выбирает, протяжка по пустому месту —
 * рамка выделения, два пальца — прокрутка, щипок — масштаб.
 */
import {
  api, el, clear, toast, confirm, editable, prompt, contextMenu, closeContextMenu,
} from './lib.js';

const NODE_W = 250;
let clipboard = null; // общий буфер между открытиями вкладки

export function graphView(root, state) {
  let graph = { nodes: [], edges: [] };
  let view = { x: 40, y: 40, k: 1 };
  let selection = new Set();
  let linking = null;
  let spaceHeld = false;

  const zoomLevel = el('span.lvl', {}, '100%');
  const layer = el('div.layer');
  const svg = document.createElementNS('http://www.w3.org/2000/svg', 'svg');
  svg.classList.add('edges');
  const edgeG = document.createElementNS('http://www.w3.org/2000/svg', 'g');
  svg.append(edgeG);

  const canvas = el('div.canvas', {}, svg, layer,
    el('div.toolbar', {},
      el('button.btn', { onclick: () => addNode('chapter') }, '+ Глава'),
      el('button.btn', { onclick: () => addNode('event') }, '+ Событие'),
      el('button.btn', { onclick: () => addNode('plan') }, '+ План'),
      el('div.zoom-box', {},
        el('button.btn', { onclick: () => zoomBy(1 / 1.25), title: 'Отдалить' }, '−'),
        zoomLevel,
        el('button.btn', { onclick: () => zoomBy(1.25), title: 'Приблизить' }, '+'),
        el('button.btn', { onclick: fit, title: 'Вместить всё' }, 'Вместить')
      )
    ),
    el('div.hint-bar', {},
      'Клик — выбрать · протянуть по пустому — рамка · два пальца — прокрутка · щипок — масштаб')
  );

  const details = el('div.details');
  root.append(el('div', { id: 'graph' }, canvas, details));

  // ═════════════ выделение ═════════════

  const isSel = (id) => selection.has(id);

  function setSelection(ids, { silent = false } = {}) {
    selection = new Set(ids);
    for (const box of layer.querySelectorAll('.node')) {
      box.classList.toggle('sel', selection.has(Number(box.dataset.node)));
    }
    if (!silent) renderDetails();
  }

  const clearSelection = () => setSelection([]);

  function selectedNodes() {
    return graph.nodes.filter((n) => selection.has(n.id));
  }

  // ═════════════ отрисовка ═════════════

  function applyTransform() {
    layer.style.transform = `translate(${view.x}px, ${view.y}px) scale(${view.k})`;
    edgeG.setAttribute('transform', `translate(${view.x}, ${view.y}) scale(${view.k})`);
    zoomLevel.textContent = Math.round(view.k * 100) + '%';
  }

  function zoomAt(factor, px, py) {
    const rect = canvas.getBoundingClientRect();
    const mx = px ?? rect.width / 2;
    const my = py ?? rect.height / 2;
    const k = Math.min(2.5, Math.max(0.15, view.k * factor));
    view.x = mx - (mx - view.x) * (k / view.k);
    view.y = my - (my - view.y) * (k / view.k);
    view.k = k;
    applyTransform();
  }

  const zoomBy = (f) => zoomAt(f);

  function nodeSize(n) {
    const dom = layer.querySelector(`[data-node="${n.id}"]`);
    return { w: NODE_W, h: dom?.offsetHeight ?? 90 };
  }

  function drawEdges() {
    clear(edgeG);
    const byId = new Map(graph.nodes.map((n) => [n.id, n]));

    for (const e of graph.edges) {
      const a = byId.get(e.from_node);
      const b = byId.get(e.to_node);
      if (!a || !b) continue;

      const as = nodeSize(a);
      const x1 = a.x + as.w, y1 = a.y + 21;
      const x2 = b.x, y2 = b.y + 21;
      const dx = Math.max(50, Math.abs(x2 - x1) * 0.5);

      const path = document.createElementNS('http://www.w3.org/2000/svg', 'path');
      path.setAttribute('d', `M${x1},${y1} C${x1 + dx},${y1} ${x2 - dx},${y2} ${x2},${y2}`);
      path.setAttribute('stroke', e.kind === 'flow' ? '#7aa2f7' : '#bb9af7');
      path.setAttribute('stroke-width', e.kind === 'flow' ? 2.5 : 1.8);
      if (e.kind !== 'flow') path.setAttribute('stroke-dasharray', '5 4');
      path.setAttribute('opacity', '.75');

      const removeEdge = async () => {
        await api.del(`/edges/${e.id}`);
        graph.edges = graph.edges.filter((x) => x.id !== e.id);
        drawEdges();
        toast('Связь удалена');
      };

      path.addEventListener('click', async (ev) => {
        ev.stopPropagation();
        if (await confirm('Удалить связь?')) removeEdge();
      });
      path.addEventListener('contextmenu', (ev) => {
        ev.preventDefault();
        ev.stopPropagation();
        contextMenu(ev.clientX, ev.clientY, [
          {
            icon: '↔',
            label: e.kind === 'flow' ? 'Сделать причинной' : 'Сделать порядком глав',
            action: async () => {
              const kind = e.kind === 'flow' ? 'data' : 'flow';
              await api.del(`/edges/${e.id}`);
              const fresh = await api.post(`/books/${state.book.id}/edges`,
                { from_node: e.from_node, to_node: e.to_node, kind });
              graph.edges = graph.edges.filter((x) => x.id !== e.id).concat(fresh);
              drawEdges();
            },
          },
          '-',
          { icon: '🗑', label: 'Удалить связь', action: removeEdge },
        ]);
      });
      edgeG.append(path);
    }

    if (linking?.ghost) edgeG.append(linking.ghost);
  }

  function nodeEl(n) {
    const ent = n.entity ?? {};
    const kind = n.type;
    const box = el(`div.node.${kind}`, {
      'data-node': n.id,
      style: { left: n.x + 'px', top: n.y + 'px' },
    });
    if (kind === 'event' && ent.hidden) box.classList.add('hidden-ev');
    if (kind === 'plan') box.classList.add('plan-node');
    if (isSel(n.id)) box.classList.add('sel');

    let head;
    if (kind === 'chapter') {
      head = el('div.head', {},
        el('span.no', {}, `${ent.number ?? '?'}.`),
        el('span', {}, ent.title || 'Без названия'));
    } else if (kind === 'event') {
      head = el('div.head', {},
        ent.hidden ? el('span', { title: 'скрытая линия' }, '🔒') : null,
        el('span', {}, ent.title || 'Событие'));
    } else {
      head = el('div.head', {}, el('span', {}, n.data?.title || 'План'));
    }
    box.append(head);

    if (kind === 'chapter') {
      const low = ent.status === 'написано' && ent.chars_no_space < state.book.min_chars;
      box.append(el('div.body', {},
        el('div', {},
          el('span', { class: `pill ${ent.status === 'написано' ? 'ok' : 'plan'}` }, ent.status ?? '—'),
          ' ',
          el('span', { class: low ? 'pill warn' : 'pill' },
            `${(ent.chars_no_space ?? 0).toLocaleString('ru-RU')} зн.`)
        ),
        ent.summary ? el('div', { style: { marginTop: '6px' } }, ent.summary.slice(0, 110) + '…') : null
      ));
      box.append(el('div.foot', {},
        el('button.btn', {
          onmousedown: (e) => e.stopPropagation(),
          onclick: (e) => { e.stopPropagation(); state.openEditor(ent.id); },
        }, 'Текст')
      ));
    } else if (kind === 'event') {
      box.append(el('div.body', {},
        ent.summary ? el('div.t', {}, ent.summary.slice(0, 130) + (ent.summary.length > 130 ? '…' : '')) : null,
        (ent.participants?.length
          ? el('div.chips', {}, ent.participants.map((p) => el('span.chip', {}, p.name)))
          : null)
      ));
    } else {
      box.append(el('div.body', {}, el('div.t', {}, n.data?.text || 'Опиши, что здесь произойдёт')));
    }

    box.append(el('div.port.in'), el('div.port.out', { onmousedown: (e) => startLink(e, n) }));

    // Выбор происходит на нажатии, чтобы перетаскивание группы работало сразу
    box.addEventListener('mousedown', (e) => {
      if (e.button !== 0) return;
      if (e.target.classList.contains('port')) return;
      const additive = e.ctrlKey || e.metaKey || e.shiftKey;
      if (additive) {
        const next = new Set(selection);
        next.has(n.id) ? next.delete(n.id) : next.add(n.id);
        setSelection(next);
      } else if (!isSel(n.id)) {
        setSelection([n.id]);
      }
      startDrag(e, n);
    });

    return box;
  }

  function render() {
    clear(layer);
    for (const n of graph.nodes) layer.append(nodeEl(n));
    drawEdges();
    applyTransform();
    renderDetails();
  }

  // ═════════════ перетаскивание нод ═════════════

  function startDrag(e, n) {
    if (e.button !== 0 || spaceHeld) return;
    e.stopPropagation();
    const sx = e.clientX, sy = e.clientY;
    let group = null; // снимок берём при первом движении: к тому моменту выбор уже обновлён

    const move = (ev) => {
      if (!group) {
        const ids = isSel(n.id) && selection.size > 1 ? selectedNodes() : [n];
        group = ids.map((node) => ({ node, ox: node.x, oy: node.y }));
      }
      const dx = (ev.clientX - sx) / view.k;
      const dy = (ev.clientY - sy) / view.k;
      for (const g of group) {
        g.node.x = g.ox + dx;
        g.node.y = g.oy + dy;
        const box = layer.querySelector(`[data-node="${g.node.id}"]`);
        if (box) { box.style.left = g.node.x + 'px'; box.style.top = g.node.y + 'px'; }
      }
      drawEdges();
    };

    const up = async () => {
      document.removeEventListener('mousemove', move);
      document.removeEventListener('mouseup', up);
      if (!group) return;
      const moved = group.filter((g) => g.node.x !== g.ox || g.node.y !== g.oy);
      if (!moved.length) return;
      try {
        await api.post('/nodes/move',
          { items: moved.map((g) => ({ id: g.node.id, x: g.node.x, y: g.node.y })) });
      } catch (err) { toast(err.message, true); }
    };

    document.addEventListener('mousemove', move);
    document.addEventListener('mouseup', up);
  }

  // ═════════════ создание связей ═════════════

  function startLink(e, from) {
    e.stopPropagation();
    e.preventDefault();
    canvas.classList.add('linking');

    const ghost = document.createElementNS('http://www.w3.org/2000/svg', 'path');
    ghost.setAttribute('stroke', '#7aa2f7');
    ghost.setAttribute('stroke-width', '2');
    ghost.setAttribute('stroke-dasharray', '4 4');
    ghost.setAttribute('fill', 'none');
    linking = { from, ghost };

    const rect = canvas.getBoundingClientRect();
    const move = (ev) => {
      const x1 = from.x + NODE_W, y1 = from.y + 21;
      const x2 = (ev.clientX - rect.left - view.x) / view.k;
      const y2 = (ev.clientY - rect.top - view.y) / view.k;
      const dx = Math.max(50, Math.abs(x2 - x1) * .5);
      ghost.setAttribute('d', `M${x1},${y1} C${x1 + dx},${y1} ${x2 - dx},${y2} ${x2},${y2}`);
      drawEdges();
    };
    const up = async (ev) => {
      document.removeEventListener('mousemove', move);
      document.removeEventListener('mouseup', up);
      canvas.classList.remove('linking');
      linking = null;

      const target = ev.target.closest?.('[data-node]');
      const toId = target ? Number(target.dataset.node) : null;
      if (toId && toId !== from.id) {
        const to = graph.nodes.find((n) => n.id === toId);
        const kind = from.type === 'chapter' && to.type === 'chapter' ? 'flow' : 'data';
        try {
          const edge = await api.post(`/books/${state.book.id}/edges`,
            { from_node: from.id, to_node: toId, kind });
          graph.edges.push(edge);
        } catch (err) { toast(err.message, true); }
      }
      drawEdges();
    };
    document.addEventListener('mousemove', move);
    document.addEventListener('mouseup', up);
  }

  // ═════════════ рамка выделения и панорама ═════════════

  canvas.addEventListener('mousedown', (e) => {
    closeContextMenu();
    if (e.target.closest?.('[data-node]') || e.target.closest?.('.toolbar')) return;

    // Средняя кнопка или пробел — панорама. Левая по пустому — рамка выделения.
    if (e.button === 1 || (e.button === 0 && spaceHeld)) return startPan(e);
    if (e.button !== 0) return;
    startMarquee(e);
  });

  function startPan(e) {
    e.preventDefault();
    canvas.classList.add('panning');
    const sx = e.clientX, sy = e.clientY, ox = view.x, oy = view.y;
    const move = (ev) => {
      view.x = ox + ev.clientX - sx;
      view.y = oy + ev.clientY - sy;
      applyTransform();
    };
    const up = () => {
      canvas.classList.remove('panning');
      document.removeEventListener('mousemove', move);
      document.removeEventListener('mouseup', up);
    };
    document.addEventListener('mousemove', move);
    document.addEventListener('mouseup', up);
  }

  function startMarquee(e) {
    const rect = canvas.getBoundingClientRect();
    const sx = e.clientX - rect.left, sy = e.clientY - rect.top;
    const additive = e.ctrlKey || e.metaKey || e.shiftKey;
    const base = additive ? new Set(selection) : new Set();

    let box = null;
    let moved = false;

    const move = (ev) => {
      const cx = ev.clientX - rect.left, cy = ev.clientY - rect.top;
      if (!moved && Math.hypot(cx - sx, cy - sy) < 4) return; // это ещё клик, не протяжка
      if (!moved) {
        moved = true;
        canvas.classList.add('selecting');
        box = el('div.marquee');
        canvas.append(box);
      }
      const x = Math.min(sx, cx), y = Math.min(sy, cy);
      const w = Math.abs(cx - sx), h = Math.abs(cy - sy);
      Object.assign(box.style, { left: x + 'px', top: y + 'px', width: w + 'px', height: h + 'px' });

      const hit = graph.nodes.filter((n) => {
        const size = nodeSize(n);
        const nx = view.x + n.x * view.k;
        const ny = view.y + n.y * view.k;
        return nx < x + w && nx + size.w * view.k > x && ny < y + h && ny + size.h * view.k > y;
      }).map((n) => n.id);

      setSelection([...base, ...hit], { silent: true });
    };

    const up = () => {
      document.removeEventListener('mousemove', move);
      document.removeEventListener('mouseup', up);
      canvas.classList.remove('selecting');
      box?.remove();
      if (!moved && !additive) clearSelection();
      else renderDetails();
    };

    document.addEventListener('mousemove', move);
    document.addEventListener('mouseup', up);
  }

  canvas.addEventListener('wheel', (e) => {
    e.preventDefault();
    closeContextMenu();
    const rect = canvas.getBoundingClientRect();

    if (e.ctrlKey || e.metaKey) {
      zoomAt(Math.exp(-e.deltaY * 0.01), e.clientX - rect.left, e.clientY - rect.top);
      return;
    }
    if (e.shiftKey && e.deltaX === 0) view.x -= e.deltaY;
    else { view.x -= e.deltaX; view.y -= e.deltaY; }
    applyTransform();
  }, { passive: false });

  function fit() {
    if (!graph.nodes.length) return;
    const xs = graph.nodes.map((n) => n.x);
    const ys = graph.nodes.map((n) => n.y);
    const minX = Math.min(...xs), maxX = Math.max(...xs) + NODE_W;
    const minY = Math.min(...ys), maxY = Math.max(...ys) + 160;
    const rect = canvas.getBoundingClientRect();
    const k = Math.min(1, Math.min(rect.width / (maxX - minX + 80), rect.height / (maxY - minY + 80)));
    view.k = Math.max(0.15, k);
    view.x = 40 - minX * view.k;
    view.y = 40 - minY * view.k;
    applyTransform();
  }

  // ═════════════ контекстное меню ═════════════

  function toCanvas(clientX, clientY) {
    const rect = canvas.getBoundingClientRect();
    return {
      x: (clientX - rect.left - view.x) / view.k,
      y: (clientY - rect.top - view.y) / view.k,
    };
  }

  canvas.addEventListener('contextmenu', (e) => {
    e.preventDefault();
    const nodeEl = e.target.closest?.('[data-node]');
    if (nodeEl) {
      const n = graph.nodes.find((x) => x.id === Number(nodeEl.dataset.node));
      if (n) {
        if (!isSel(n.id)) setSelection([n.id]);
        return selection.size > 1 ? multiMenu(e) : nodeMenu(e, n);
      }
    }
    clearSelection();
    emptyMenu(e);
  });

  function emptyMenu(e) {
    const pos = toCanvas(e.clientX, e.clientY);
    contextMenu(e.clientX, e.clientY, [
      { icon: '📖', label: 'Добавить главу', action: () => addNode('chapter', pos) },
      { icon: '✦', label: 'Добавить событие', action: () => addNode('event', pos) },
      { icon: '📝', label: 'Добавить план', action: () => addNode('plan', pos) },
      '-',
      { icon: '📋', label: 'Вставить', hint: 'Ctrl+V', disabled: !clipboard, action: () => pasteNode(pos) },
      { icon: '⬚', label: 'Выделить всё', hint: 'Ctrl+A',
        action: () => setSelection(graph.nodes.map((n) => n.id)) },
      '-',
      { icon: '⤢', label: 'Вместить всё', action: fit },
      { icon: '1:1', label: 'Сбросить масштаб', action: () => { view.k = 1; applyTransform(); } },
    ]);
  }

  function nodeMenu(e, n) {
    const ent = n.entity ?? {};
    contextMenu(e.clientX, e.clientY, [
      n.type === 'chapter'
        ? { icon: '✎', label: 'Открыть текст', action: () => state.openEditor(ent.id) } : null,
      { icon: '⧉', label: 'Копировать', hint: 'Ctrl+C', action: () => copyNodes([n]) },
      { icon: '⎘', label: 'Дублировать', hint: 'Ctrl+D',
        action: () => { copyNodes([n], true); pasteNode({ x: n.x + 40, y: n.y + 40 }); } },
      '-',
      n.type === 'event'
        ? {
            icon: '🔒',
            label: ent.hidden ? 'Убрать из скрытой линии' : 'Пометить скрытой',
            action: async () => {
              const upd = await api.patch(`/events/${ent.id}`, { hidden: ent.hidden ? 0 : 1 });
              Object.assign(n.entity, upd);
              render();
            },
          } : null,
      { icon: '↔', label: 'Убрать все связи',
        disabled: !graph.edges.some((x) => x.from_node === n.id || x.to_node === n.id),
        action: () => unlink([n]) },
      '-',
      { icon: '🗑', label: 'Убрать с холста', hint: 'Delete', action: () => removeNodes([n]) },
    ]);
  }

  function multiMenu(e) {
    const nodes = selectedNodes();
    contextMenu(e.clientX, e.clientY, [
      { icon: '⧉', label: `Копировать (${nodes.length})`, hint: 'Ctrl+C', action: () => copyNodes(nodes) },
      '-',
      { icon: '⇥', label: 'Выровнять по левому краю', action: () => align('left') },
      { icon: '⇧', label: 'Выровнять по верхнему краю', action: () => align('top') },
      { icon: '⇹', label: 'Разложить в ряд', action: () => distribute('h') },
      { icon: '⇳', label: 'Разложить в столбец', action: () => distribute('v') },
      '-',
      { icon: '↔', label: 'Убрать связи', action: () => unlink(nodes) },
      { icon: '🗑', label: `Убрать с холста (${nodes.length})`, hint: 'Delete',
        action: () => removeNodes(nodes) },
    ]);
  }

  // ═════════════ групповые операции ═════════════

  async function persistPositions(nodes) {
    await api.post('/nodes/move', { items: nodes.map((n) => ({ id: n.id, x: n.x, y: n.y })) });
    render();
  }

  function align(edge) {
    const nodes = selectedNodes();
    if (nodes.length < 2) return;
    if (edge === 'left') {
      const x = Math.min(...nodes.map((n) => n.x));
      nodes.forEach((n) => { n.x = x; });
    } else {
      const y = Math.min(...nodes.map((n) => n.y));
      nodes.forEach((n) => { n.y = y; });
    }
    persistPositions(nodes);
  }

  function distribute(axis) {
    const nodes = selectedNodes();
    if (nodes.length < 2) return;
    if (axis === 'h') {
      nodes.sort((a, b) => a.x - b.x);
      const x0 = nodes[0].x, y0 = nodes[0].y;
      nodes.forEach((n, i) => { n.x = x0 + i * (NODE_W + 40); n.y = y0; });
    } else {
      nodes.sort((a, b) => a.y - b.y);
      const x0 = nodes[0].x, y0 = nodes[0].y;
      nodes.forEach((n, i) => { n.x = x0; n.y = y0 + i * 150; });
    }
    persistPositions(nodes);
  }

  async function unlink(nodes) {
    const ids = new Set(nodes.map((n) => n.id));
    const mine = graph.edges.filter((x) => ids.has(x.from_node) || ids.has(x.to_node));
    if (!mine.length) return;
    for (const edge of mine) await api.del(`/edges/${edge.id}`);
    graph.edges = graph.edges.filter((x) => !mine.includes(x));
    drawEdges();
    toast(`Убрано связей: ${mine.length}`);
  }

  async function removeNodes(nodes) {
    if (!nodes.length) return;
    const what = nodes.length === 1 ? 'Убрать ноду с холста?' : `Убрать ${nodes.length} нод с холста?`;
    if (!(await confirm(what))) return;
    for (const n of nodes) await api.del(`/nodes/${n.id}`);
    const ids = new Set(nodes.map((n) => n.id));
    graph.nodes = graph.nodes.filter((n) => !ids.has(n.id));
    graph.edges = graph.edges.filter((e) => !ids.has(e.from_node) && !ids.has(e.to_node));
    selection = new Set();
    render();
  }

  function copyNodes(nodes, silent = false) {
    clipboard = nodes.map((n) => ({
      type: n.type,
      data: n.data ?? null,
      entity: n.entity ? { ...n.entity } : null,
      dx: n.x - nodes[0].x,
      dy: n.y - nodes[0].y,
    }));
    if (!silent) toast(nodes.length === 1 ? 'Скопировано' : `Скопировано: ${nodes.length}`);
  }

  async function pasteNode(pos) {
    if (!clipboard?.length) return;
    const created = [];

    for (const item of clipboard) {
      const node = await api.post(`/books/${state.book.id}/nodes`, {
        type: item.type,
        x: pos.x + (item.dx ?? 0),
        y: pos.y + (item.dy ?? 0),
        data: item.data,
      });
      const ent = item.entity;
      if (ent && node.ref_table === 'event') {
        await api.patch(`/events/${node.ref_id}`, {
          title: ent.title ? ent.title + ' (копия)' : 'Событие',
          summary: ent.summary ?? null,
          hidden: ent.hidden ?? 0,
          chapter_id: ent.chapter_id ?? null,
        });
      } else if (ent && node.ref_table === 'chapter') {
        await api.patch(`/chapters/${node.ref_id}`, {
          title: ent.title ? ent.title + ' (копия)' : 'Глава',
          plan: ent.plan ?? null,
          pov: ent.pov ?? null,
        });
      }
      created.push(node.id);
    }

    await reload();
    setSelection(created, { silent: true });
    render();
    toast(created.length === 1 ? 'Вставлено' : `Вставлено: ${created.length}`);
  }

  async function reload() {
    graph = await api.get(`/books/${state.book.id}/graph`);
    state.chapters = await api.get(`/books/${state.book.id}/chapters`);
  }

  // ═════════════ клавиатура ═════════════

  const onKey = (e) => {
    // Слушатель живёт на document и не снимается при уходе с вкладки «Граф»
    // (у view-модулей нет lifecycle/unmount) — поэтому обязательно проверяем
    // фокус на каждое нажатие, а не только на маунте.
    if (!root.isConnected) return;
    const editing = /INPUT|TEXTAREA|SELECT/.test(document.activeElement?.tagName)
      || document.activeElement?.isContentEditable;
    if (e.code === 'Space' && !spaceHeld) {
      if (editing) return;
      spaceHeld = true;
      canvas.classList.add('space');
      e.preventDefault();
      return;
    }
    if (editing) return;

    const nodes = selectedNodes();
    const mod = e.ctrlKey || e.metaKey;

    if (e.key === 'Escape') { clearSelection(); closeContextMenu(); return; }
    if (e.key === 'Delete' && nodes.length) { removeNodes(nodes); return; }
    if (mod && e.key.toLowerCase() === 'a') {
      e.preventDefault(); setSelection(graph.nodes.map((n) => n.id)); return;
    }
    if (mod && e.key.toLowerCase() === 'c' && nodes.length) { e.preventDefault(); copyNodes(nodes); return; }
    if (mod && e.key.toLowerCase() === 'd' && nodes.length) {
      e.preventDefault();
      copyNodes(nodes, true);
      pasteNode({ x: nodes[0].x + 40, y: nodes[0].y + 40 });
      return;
    }
    if (mod && e.key.toLowerCase() === 'v' && clipboard) {
      e.preventDefault();
      const rect = canvas.getBoundingClientRect();
      pasteNode(toCanvas(rect.left + rect.width / 2, rect.top + rect.height / 3));
      return;
    }
    if (mod && (e.key === '=' || e.key === '+')) { e.preventDefault(); zoomBy(1.25); }
    if (mod && e.key === '-') { e.preventDefault(); zoomBy(1 / 1.25); }
    if (mod && e.key === '0') { e.preventDefault(); view.k = 1; applyTransform(); }
  };

  const onKeyUp = (e) => {
    if (e.code === 'Space') { spaceHeld = false; canvas.classList.remove('space'); }
  };

  document.addEventListener('keydown', onKey);
  document.addEventListener('keyup', onKeyUp);

  // ═════════════ панель свойств ═════════════

  function renderDetails() {
    clear(details);
    const nodes = selectedNodes();

    if (nodes.length === 0) {
      details.append(el('div.empty', {}, 'Выбери ноду, чтобы увидеть свойства'));
      return;
    }
    if (nodes.length > 1) return renderMultiDetails(nodes);

    const n = nodes[0];
    const ent = n.entity ?? {};
    const save = async (patch) => {
      const table = n.ref_table === 'chapter' ? 'chapters' : 'events';
      const updated = await api.patch(`/${table}/${ent.id}`, patch);
      Object.assign(n.entity, updated);
      render();
    };

    details.append(el('h3', {}, n.type === 'chapter' ? 'Глава' : n.type === 'event' ? 'Событие' : 'План'));

    if (n.type === 'chapter') {
      details.append(
        editable('Номер', ent.number, (v) => save({ number: Number(v) }), { type: 'number' }),
        editable('Название', ent.title, (v) => save({ title: v })),
        editable('POV', ent.pov, (v) => save({ pov: v })),
        statusSelect(ent.status, (v) => save({ status: v }), ['план', 'черновик', 'написано']),
        editable('Замысел', ent.plan, (v) => save({ plan: v }), { multiline: true, rows: 4 }),
        editable('Выжимка', ent.summary, (v) => save({ summary: v }), { multiline: true, rows: 5 }),
        el('div', { style: { marginTop: '10px' } },
          el('button.btn.primary', { onclick: () => state.openEditor(ent.id) }, 'Открыть текст')),
        el('div.dim', { style: { marginTop: '10px', fontSize: '12px' } },
          `${(ent.chars_no_space ?? 0).toLocaleString('ru-RU')} знаков без пробелов`)
      );
    } else if (n.type === 'event') {
      details.append(
        editable('Заголовок', ent.title, (v) => save({ title: v })),
        editable('Что происходит', ent.summary, (v) => save({ summary: v }), { multiline: true, rows: 6 }),
        statusSelect(ent.status, (v) => save({ status: v }), ['план', 'написано']),
        chapterSelect(ent.chapter_id, (v) => save({ chapter_id: v ? Number(v) : null })),
        el('label.field', {},
          el('span', {}, 'Скрытая линия'),
          el('input', {
            type: 'checkbox', style: { width: 'auto' },
            ...(ent.hidden ? { checked: 'checked' } : {}),
            onchange: (e) => save({ hidden: e.target.checked ? 1 : 0 }),
          })),
        participantsBlock(ent)
      );
    } else {
      details.append(
        editable('Заголовок', n.data?.title, async (v) => {
          n.data = { ...(n.data ?? {}), title: v };
          await api.patch(`/nodes/${n.id}`, { data: n.data });
          render();
        }),
        editable('Текст плана', n.data?.text, async (v) => {
          n.data = { ...(n.data ?? {}), text: v };
          await api.patch(`/nodes/${n.id}`, { data: n.data });
          render();
        }, { multiline: true, rows: 8 })
      );
    }

    details.append(el('div', { style: { marginTop: '16px' } },
      el('button.btn.danger', { onclick: () => removeNodes([n]) }, 'Убрать с холста')));
  }

  function renderMultiDetails(nodes) {
    const chapters = nodes.filter((n) => n.type === 'chapter');
    const events = nodes.filter((n) => n.type === 'event');

    details.append(
      el('h3', {}, `Выбрано: ${nodes.length}`),
      el('div.sel-summary', {},
        ...nodes.slice(0, 12).map((n) =>
          el('span.chip', {}, n.entity?.title ?? n.data?.title ?? 'Нода')),
        nodes.length > 12 ? el('span.dim', {}, ` и ещё ${nodes.length - 12}`) : null
      )
    );

    details.append(el('h3', { style: { marginTop: '16px' } }, 'Разложить'));
    details.append(el('div', { style: { display: 'flex', flexWrap: 'wrap', gap: '6px' } },
      el('button.btn', { onclick: () => align('left') }, 'По левому краю'),
      el('button.btn', { onclick: () => align('top') }, 'По верхнему краю'),
      el('button.btn', { onclick: () => distribute('h') }, 'В ряд'),
      el('button.btn', { onclick: () => distribute('v') }, 'В столбец')
    ));

    if (events.length) {
      details.append(el('h3', { style: { marginTop: '18px' } }, `События · ${events.length}`));
      details.append(
        chapterSelect(null, async (v) => {
          const chapter_id = v ? Number(v) : null;
          for (const n of events) await api.patch(`/events/${n.entity.id}`, { chapter_id });
          await reload();
          render();
          toast('Глава назначена');
        }, 'Перенести в главу'),
        el('div', { style: { display: 'flex', gap: '6px', marginTop: '4px' } },
          el('button.btn', {
            onclick: async () => {
              for (const n of events) await api.patch(`/events/${n.entity.id}`, { hidden: 1 });
              await reload(); render(); toast('Помечены скрытыми');
            },
          }, 'Скрытая линия'),
          el('button.btn', {
            onclick: async () => {
              for (const n of events) await api.patch(`/events/${n.entity.id}`, { hidden: 0 });
              await reload(); render(); toast('Убраны из скрытой линии');
            },
          }, 'Обычные')
        )
      );
    }

    if (chapters.length) {
      details.append(el('h3', { style: { marginTop: '18px' } }, `Главы · ${chapters.length}`));
      details.append(statusSelect(null, async (v) => {
        for (const n of chapters) await api.patch(`/chapters/${n.entity.id}`, { status: v });
        await reload(); render(); toast('Статус изменён');
      }, ['план', 'черновик', 'написано'], 'Сменить статус'));
    }

    details.append(el('div', { style: { marginTop: '20px', display: 'flex', gap: '6px' } },
      el('button.btn', { onclick: () => copyNodes(nodes) }, 'Копировать'),
      el('button.btn.danger', { onclick: () => removeNodes(nodes) }, 'Убрать все')
    ));
  }

  function statusSelect(value, onSave, options, label = 'Статус') {
    const sel = el('select', { onchange: (e) => e.target.value && onSave(e.target.value) },
      value === null ? el('option', { value: '' }, '— не менять —') : null,
      ...options.map((o) => el('option', { value: o, ...(o === value ? { selected: 'selected' } : {}) }, o)));
    return el('label.field', {}, el('span', {}, label), sel);
  }

  function chapterSelect(value, onSave, label = 'Глава') {
    const sel = el('select', { onchange: (e) => onSave(e.target.value) },
      el('option', { value: '' }, value === null ? '— выбрать главу —' : '— вне главы —'),
      ...state.chapters.map((c) => el('option', {
        value: c.id, ...(c.id === value ? { selected: 'selected' } : {}),
      }, `${c.number}. ${c.title ?? ''}`)));
    return el('label.field', {}, el('span', {}, label), sel);
  }

  function participantsBlock(ent) {
    const wrap = el('div.field');
    wrap.append(el('span.dim', { style: { fontSize: '12px' } }, 'Участники'));

    const chips = el('div.chips', { style: { margin: '6px 0' } },
      ...(ent.participants ?? []).map((p) =>
        el('span.chip', {
          title: 'Убрать', style: { cursor: 'pointer' },
          onclick: async () => {
            await api.del(`/events/${ent.id}/characters/${p.id}`);
            ent.participants = ent.participants.filter((x) => x.id !== p.id);
            render();
          },
        }, p.name + ' ×')));

    const add = el('select', {
      onchange: async (e) => {
        if (!e.target.value) return;
        const id = Number(e.target.value);
        await api.post(`/events/${ent.id}/characters`, { character_id: id });
        const ch = state.characters.find((c) => c.id === id);
        ent.participants = [...(ent.participants ?? []), { id, name: ch.name }];
        render();
      },
    }, el('option', { value: '' }, '+ добавить участника'),
       ...state.characters
         .filter((c) => !(ent.participants ?? []).some((p) => p.id === c.id))
         .map((c) => el('option', { value: c.id }, c.name)));

    wrap.append(chips, add);
    return wrap;
  }

  // ═════════════ создание нод ═════════════

  async function addNode(type, pos) {
    const rect = canvas.getBoundingClientRect();
    const x = pos ? pos.x : (rect.width / 2 - view.x) / view.k - NODE_W / 2;
    const y = pos ? pos.y : (rect.height / 3 - view.y) / view.k;

    let extra = {};
    if (type === 'plan') {
      const text = await prompt('Новая нода плана', { multiline: true, placeholder: 'Что здесь произойдёт…' });
      if (text === null) return;
      extra = { data: { title: 'План', text } };
    }

    const node = await api.post(`/books/${state.book.id}/nodes`, { type, x, y, ...extra });
    graph.nodes.push(node);
    if (node.ref_table === 'chapter') state.chapters = await api.get(`/books/${state.book.id}/chapters`);
    setSelection([node.id], { silent: true });
    render();
    toast(type === 'chapter' ? 'Глава создана' : type === 'event' ? 'Событие создано' : 'Нода добавлена');
  }

  // ═════════════ запуск ═════════════

  (async () => {
    graph = await api.get(`/books/${state.book.id}/graph`);
    render();
    requestAnimationFrame(() => { fit(); drawEdges(); });
  })();
}

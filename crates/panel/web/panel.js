// SPDX-License-Identifier: GPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Andrea Mattera
// The browser panel's page: drawn from what the body says (its words, the settings' schema,
// the widget's pages as the screen draws them, the shopping lists, the devices). Text is only
// ever set as text (textContent), never as HTML; icons are constant SVG paths.
'use strict';

const app = document.getElementById('app');
const nav = document.getElementById('tabs');
const NS = 'http://www.w3.org/2000/svg';
let T = {};
let lang = 'en';
let name = '';

function el(tag, cls, text) {
  const e = document.createElement(tag);
  if (cls) e.className = cls;
  if (text !== undefined && text !== null) e.textContent = String(text);
  return e;
}

function t(key, vars) {
  let s = T[key] || key;
  for (const [k, v] of Object.entries(vars || {})) s = s.split('{' + k + '}').join(v);
  return s;
}

async function api(path, body) {
  const opt = { credentials: 'same-origin', cache: 'no-store' };
  if (body !== undefined) {
    opt.method = 'POST';
    opt.headers = { 'Content-Type': 'application/json' };
    opt.body = JSON.stringify(body);
  }
  let r;
  try {
    r = await fetch(path, opt);
  } catch (e) {
    return { ok: false, error: 'network' };
  }
  let j = {};
  try { j = await r.json(); } catch (e) { j = { ok: false, error: 'network' }; }
  if (r.status === 401 && !path.startsWith('/api/pair')) {
    login();
    return { ok: false, error: 'no_session' };
  }
  if (r.status === 503) { off(); return { ok: false, error: 'off' }; }
  return j;
}

function errorText(code) {
  return T['panel_' + code] ? t('panel_' + code) : t('panel_error');
}

// ---- Icons (constant paths, drawn with createElementNS).

const IC = {
  next: 'M5 6h14v14H5z M5 10h14 M9 4v4 M15 4v4', quotes: 'M4 4v16h16 M7 15l4-5 3 3 5-7',
  weather: 'M7 19h10a3.5 3.5 0 000-7 5 5 0 00-9.6-1A4 4 0 007 19z M12 3v1.5 M5.5 5.5l1 1 M18.5 5.5l-1 1',
  cities: 'M12 4a8 8 0 100 16 8 8 0 000-16z M4 12h16 M12 4c3 3 3 13 0 16 M12 4c-3 3-3 13 0 16',
  shopping: 'M3 5h2l2.5 10h10L20 8H7 M9 19a.6.6 0 100 .1 M17 19a.6.6 0 100 .1', notes: 'M6 4h9l4 4v12H6z M9 11h7 M9 15h5',
  sensors: 'M12 4a8 8 0 100 16 8 8 0 000-16z M12 12l4-3', check: 'M6 20V5l8-1v16 M14 6h4v14 M4 20h16 M11 12v1',
  playing: 'M10 17V5l8-2v12 M10 17a2.5 2.5 0 11-5 0 2.5 2.5 0 015 0z M18 15a2.5 2.5 0 11-5 0 2.5 2.5 0 015 0z',
  home: 'M9 12a3 3 0 100-6 3 3 0 000 6z M17 12.5a2.5 2.5 0 100-5 2.5 2.5 0 000 5z M3 19c0-3 3-5 6-5s6 2 6 5 M16 14.5c3 0 5 1.5 5 4.5',
  trash: 'M5 7h14 M10 7V5h4v2 M7 7l1 13h8l1-13 M10 11v6 M14 11v6', plus: 'M12 5v14 M5 12h14', back: 'M15 5l-7 7 7 7',
  search: 'M11 4a7 7 0 100 14 7 7 0 000-14z M16 16l4 4', pencil: 'M4 20l1-4L16 5l3 3L8 19z', left: 'M14 6l-6 6 6 6', right: 'M10 6l6 6-6 6',
  grip: 'M9 6v.1 M15 6v.1 M9 12v.1 M15 12v.1 M9 18v.1 M15 18v.1', pause: 'M9 6v12 M15 6v12', play: 'M8 5l11 7-11 7z',
  grid: 'M4 4h7v7H4z M13 4h7v4h-7z M13 10h7v10h-7z M4 13h7v7H4z', sliders: 'M4 7h10 M18 7h2 M4 17h2 M10 17h10 M16 9a2 2 0 100-4 2 2 0 000 4z M8 19a2 2 0 100-4 2 2 0 000 4z',
  phone: 'M8 3h8v18H8z M11 18h2', house: 'M4 11l8-7 8 7 M6 10v10h12V10',
  frame: 'M4 6h16v12H4z M7 10h6 M7 13h10', bolt: 'M13 3L5 14h6l-1 7 8-11h-6z', ok: 'M5 12l5 5 9-10',
  sun: 'M12 8a4 4 0 100 8 4 4 0 000-8z M12 2v2 M12 20v2 M4.9 4.9l1.4 1.4 M17.7 17.7l1.4 1.4 M2 12h2 M20 12h2 M4.9 19.1l1.4-1.4 M17.7 6.3l1.4-1.4',
  cloud: 'M7 18h10a3.5 3.5 0 000-7 5 5 0 00-9.6-1A4 4 0 007 18z', rain: 'M7 15h10a3.5 3.5 0 000-7 5 5 0 00-9.6-1A4 4 0 007 15z M9 18l-1 2 M13 18l-1 2 M17 18l-1 2',
  snow: 'M7 15h10a3.5 3.5 0 000-7 5 5 0 00-9.6-1A4 4 0 007 15z M9 19h.1 M13 19h.1 M17 19h.1',
  moon: 'M19 14A7 7 0 0110 5a7 7 0 109 9z', partly: 'M7 20h10a3.5 3.5 0 000-7 5 5 0 00-9.6-1A4 4 0 007 20z M12 4v1.5 M6 6.5l1 1 M18 6.5l-1 1',
};
// Home Assistant's conditions → an icon.
const SKY = { 'sunny': 'sun', 'clear-night': 'moon', 'partlycloudy': 'partly', 'cloudy': 'cloud', 'fog': 'cloud', 'windy': 'cloud',
  'windy-variant': 'cloud', 'rainy': 'rain', 'pouring': 'rain', 'lightning': 'rain', 'lightning-rainy': 'rain', 'hail': 'rain',
  'snowy': 'snow', 'snowy-rainy': 'snow', 'exceptional': 'cloud' };

function ico(key, cls) {
  const s = document.createElementNS(NS, 'svg');
  s.setAttribute('viewBox', '0 0 24 24');
  s.setAttribute('class', cls === undefined ? 'i' : cls);
  s.setAttribute('aria-hidden', 'true');
  const p = document.createElementNS(NS, 'path');
  p.setAttribute('d', IC[key] || '');
  s.append(p);
  return s;
}

function ibtn(icon, label, fn, cls) {
  const b = el('button', cls || 'iconbtn');
  b.type = 'button';
  b.setAttribute('aria-label', label);
  b.title = label;
  b.append(ico(icon));
  if (fn) b.addEventListener('click', fn);
  return b;
}

// A row's curve as the screen draws it (widgets.rs curve_path_in): straight segments.
function curve(points, cls) {
  const s = document.createElementNS(NS, 'svg');
  s.setAttribute('viewBox', '0 0 150 32');
  s.setAttribute('preserveAspectRatio', 'none');
  s.setAttribute('class', cls || 'k');
  s.setAttribute('aria-hidden', 'true');
  if (points && points.length > 1) {
    const last = points.length - 1;
    const d = points.map((v, i) => (i ? 'L' : 'M') + (i / last * 150).toFixed(1) + ' ' + (2 + (1 - Math.min(1, Math.max(0, v))) * 28).toFixed(1)).join(' ');
    const p = document.createElementNS(NS, 'path');
    p.setAttribute('d', d);
    s.append(p);
  }
  return s;
}

function say(text) { document.getElementById('live').textContent = text; }

let toastTimer = 0;
// A line at the bottom; with `undo` a button that calls it (7 s to touch it).
function toast(text, opt) {
  const o = opt === true ? { bad: true } : (opt || {});
  document.querySelectorAll('.toast').forEach(x => x.remove());
  clearTimeout(toastTimer);
  const b = el('div', 'toast' + (o.bad ? ' bad' : '') + (o.undo ? ' undo' : ''));
  b.setAttribute('role', 'status');
  b.append(el('span', null, text));
  if (o.undo) {
    const u = el('button', null, t('panel_undo'));
    u.type = 'button';
    u.addEventListener('click', () => { b.remove(); o.undo(); });
    b.append(u);
  }
  document.body.append(b);
  toastTimer = setTimeout(() => b.remove(), o.undo ? 7000 : 2200);
}

function clear() { app.replaceChildren(); }

function note() { return el('p', 'note', t('panel_lan_note')); }

function chrome(inside) {
  document.body.classList.toggle('in', inside);
  nav.hidden = !inside;
  if (!inside) document.querySelectorAll('.scrim').forEach(x => x.remove());
}

// ---- Without access: ask for it, or follow the QR code's answer.

function off() {
  chrome(false);
  clear();
  const c = el('section', 'card center');
  c.append(el('p', null, t('panel_off')));
  app.append(c, note());
}

let polling = 0;

function login(message) {
  clearInterval(polling);
  chrome(false);
  clear();
  const c = el('section', 'card center');
  if (message) c.append(el('p', 'warn', message));
  const b = el('button', 'primary', t('panel_ask'));
  b.addEventListener('click', ask);
  c.append(b, el('p', 'hint', t('panel_ask_hint')));
  app.append(c, note());
}

async function ask() {
  const r = await api('/api/pair', {});
  if (!r.ok) { login(errorText(r.error)); return; }
  clear();
  const c = el('section', 'card center');
  c.append(el('p', null, t('panel_check_code')), el('p', 'code', r.code), el('p', 'hint', t('panel_waiting')));
  app.append(c, note());
  let busy = false;
  polling = setInterval(async () => {
    if (busy) return;
    busy = true;
    const s = await api('/api/pair/' + encodeURIComponent(r.id));
    busy = false;
    if (!s.ok) { login(errorText(s.error)); return; }
    if (s.status === 'allowed') { clearInterval(polling); main(); }
    else if (s.status !== 'waiting') login(errorText(s.status === 'unknown' ? 'expired' : s.status));
  }, 1000);
}

// ---- What the body said, kept while the page is open.

const D = { schema: null, values: {}, widgets: null, shopping: null, notes: null, devices: null, samples: null };

function item(key) {
  for (const g of (D.schema && D.schema.groups) || []) for (const i of g.items) if (i.key === key) return i;
  return null;
}

// Takes an answer about the widget: its pages and lists, the settings' values.
function took(r) {
  if (!r || !r.ok) return false;
  if (r.values) Object.assign(D.values, r.values);
  if (r.order && D.widgets) Object.assign(D.widgets, r);
  else if (r.pages && D.widgets) D.widgets.pages = r.pages;
  if (r.pages) waitData();
  return true;
}

// A row still waiting for its data ("…"): the pages again in a few seconds, while it waits
// (a minute at most).
let waitTimer = 0;
let waitTries = 0;
function waitData() {
  const waiting = ((D.widgets && D.widgets.pages) || []).some(p => p.model && p.model.windows.some(w => w.rows.some(r => r.wait)));
  clearTimeout(waitTimer);
  if (!waiting) { waitTries = 0; return; }
  if (++waitTries > 20) return;
  waitTimer = setTimeout(async () => {
    if (document.hidden) { waitTries = 0; return; }
    // Never under a finger or a cursor: drawing again would take the field or the drag away.
    const a = document.activeElement;
    if (document.querySelector('.dragging') || (a && ['INPUT', 'TEXTAREA', 'SELECT'].includes(a.tagName) && a.type !== 'checkbox')) {
      waitTries--;
      waitData();
      return;
    }
    const r = await api('/api/widgets');
    if (took(r)) redraw();
  }, 3000);
}

// A name in a short line (a toast).
function short(s) { return s.length > 40 ? s.slice(0, 39) + '…' : s; }

function page(kind) { return ((D.widgets && D.widgets.pages) || []).find(p => p.kind === kind); }

// ---- A control from the schema: a switch, a choice, a number, some of a list.

function control(it, value, onChange) {
  if (it.kind === 'bool') {
    const i = el('input', 'sw');
    i.type = 'checkbox';
    i.setAttribute('role', 'switch');
    i.checked = value === true;
    i.addEventListener('change', () => onChange(i.checked));
    return i;
  }
  if (it.kind === 'choice' || (it.kind === 'int' && it.max - it.min <= 24)) {
    const s = el('select');
    const opts = it.kind === 'choice' ? it.options
      : Array.from({ length: (it.max - it.min) / it.step + 1 }, (_, k) => {
        const v = it.min + k * it.step;
        return { value: v, label: String(v).padStart(2, '0') + ':00' };
      });
    for (const o of opts) {
      const op = el('option', null, o.label);
      op.value = String(o.value);
      if (String(o.value) === String(value)) op.selected = true;
      s.append(op);
    }
    s.addEventListener('change', () => onChange(it.kind === 'int' ? Number(s.value) : s.value));
    return s;
  }
  if (it.kind === 'int') {
    const w = el('span', 'range');
    const i = el('input');
    i.type = 'range';
    i.min = it.min; i.max = it.max; i.step = it.step;
    i.value = value;
    i.setAttribute('aria-label', it.label);
    const out = el('output', null, value);
    i.addEventListener('input', () => { out.textContent = i.value; });
    i.addEventListener('change', () => onChange(Number(i.value)));
    w.append(i, out);
    return w;
  }
  // multi: for the lists and the calendars an empty choice is all of them.
  const box = el('div', 'multi');
  box.setAttribute('role', 'group');
  box.setAttribute('aria-label', it.label);
  const all = Array.isArray(value) && value.length === 0;
  for (const o of it.options) {
    const l = el('label', 'chk');
    const c = el('input');
    c.type = 'checkbox';
    c.value = o.value;
    c.checked = all || (Array.isArray(value) && value.includes(o.value));
    c.addEventListener('change', () => {
      onChange(Array.from(box.querySelectorAll('input:checked')).map(x => x.value));
    });
    l.append(c, el('span', null, o.label));
    box.append(l);
  }
  if (it.options.length === 0) box.append(el('span', 'hint', '–'));
  return box;
}

// A setting's row: its label, its line of help, its control; saved at once.
function settingRow(it, after) {
  const row = el('div', it.kind === 'multi' ? 'row column' : 'row');
  const id = 'set-' + it.key;
  const label = el('label', null, it.label);
  if (it.help) label.append(el('small', null, it.help));
  let c;
  const changed = async v => {
    const r = await api('/api/set', { key: it.key, value: v });
    if (took(r)) {
      toast(t('panel_saved'));
      if (it.key === 'language') location.reload();
      if (after) after();
    } else {
      toast(errorText(r.error), true);
      const fresh = control(it, D.values[it.key], changed);
      if (it.kind !== 'multi') fresh.id = id;
      c.replaceWith(fresh);
      c = fresh;
    }
  };
  c = control(it, D.values[it.key], changed);
  if (it.kind !== 'multi') { c.id = id; label.htmlFor = id; }
  row.append(label, c);
  return row;
}

function settingsCard(keys, after) {
  const c = el('div', 'card');
  for (const k of keys) { const it = item(k); if (it) c.append(settingRow(it, after)); }
  return c;
}

// ---- The widget's card as the screen draws it: header (icon, title, the right side), then
// the rows of one window (curve rows, timed rows, plain rows) or the weather's columns.

function drawCard(kind, model, win, empty) {
  const c = el('div', 'wc');
  const hd = el('div', 'hd');
  const w = model && model.windows && model.windows.length ? model.windows[(win || 0) % model.windows.length] : null;
  hd.append(ico(kind), el('span', 't', model ? model.title : empty.title));
  if (w && w.aside) hd.append(el('span', 'a', w.aside));
  c.append(hd);
  if (model && model.layout === 'image') { c.append(el('div', 'ph', empty.text)); return wrapCard(c); }
  if (!w || !w.rows.length) { c.append(el('div', 'empty', empty.text)); return wrapCard(c); }
  if (model.layout === 'hours') {
    const h = el('div', 'hours');
    for (const r of w.rows) {
      const x = el('div');
      x.append(el('span', 'h', r.left), ico(SKY[r.note] || 'cloud', ''), el('span', 'd', r.text));
      h.append(x);
    }
    c.append(h);
    return wrapCard(c);
  }
  const curves = kind === 'quotes' || kind === 'sensors';
  for (const r of w.rows) {
    const form = curves ? 'cv' : r.left ? 'tm' : 'pl';
    const row = el('div', 'rw ' + form);
    if (form === 'tm') row.append(el('span', 'l', r.left));
    row.append(el('span', null, r.text));
    if (form === 'cv') {
      row.append(r.curve ? curve(r.curve) : el('span', 'k'));
      row.append(el('span', 'v' + (r.wait ? ' wait' : ''), r.right || ''));
      const dir = kind === 'quotes' && r.trend ? (r.trend > 0 ? ' up' : ' dn') : '';
      row.append(el('span', 'c' + dir, r.change || ''));
    } else {
      if (form === 'tm') row.append(r.note ? ico(SKY[r.note] || 'cloud', 'w') : el('span'));
      row.append(el('span', 'v' + (r.wait ? ' wait' : ' dim'), r.right || ''));
    }
    c.append(row);
  }
  return wrapCard(c);
}

function wrapCard(c) { const p = el('div', 'wp'); p.append(c); return p; }

function preview(p, win) {
  return drawCard(p.kind, p.model, win, { title: p.label, text: t('panel_empty_' + p.kind) });
}

// ---- Reordering: drag by the handle (mouse, pen or finger), or arrows on the focused handle.
// While dragging, the item goes before the item under the pointer if the pointer is before
// that item's middle (across a row of the grid: its horizontal middle), else after it.

function sortable(box, sel, done) {
  const items = () => Array.from(box.querySelectorAll(':scope > ' + sel));
  for (const it of items()) {
    const g = it.querySelector('.grip');
    if (!g) continue;
    g.addEventListener('pointerdown', ev => {
      if (ev.button !== 0) return;
      ev.preventDefault();
      it.classList.add('dragging');
      const before = items().map(x => x.dataset.id).join();
      let y = ev.clientY;
      let x = ev.clientX;
      let scrolling = 0;
      const id = ev.pointerId;
      const place = () => {
        // Over the item itself: it is where it should be.
        const own = it.getBoundingClientRect();
        if (x >= own.left && x <= own.right && y >= own.top && y <= own.bottom) return;
        let best = null;
        let dist = Infinity;
        for (const o of items()) {
          if (o === it) continue;
          const r = o.getBoundingClientRect();
          const dx = x < r.left ? r.left - x : x > r.right ? x - r.right : 0;
          const dy = y < r.top ? r.top - y : y > r.bottom ? y - r.bottom : 0;
          const d = dx * dx + dy * dy;
          if (d < dist) { dist = d; best = o; }
        }
        if (!best) return;
        const r = best.getBoundingClientRect();
        // Side by side (the grid on a wide screen): left or right of its middle; else above or below.
        const cols = getComputedStyle(box).gridTemplateColumns.split(' ').length > 1;
        const sameRow = cols && y >= r.top && y <= r.bottom;
        const after = sameRow ? x > r.left + r.width / 2 : y > r.top + r.height / 2;
        const target = after ? best.nextElementSibling : best;
        if (target === it || target === it.nextElementSibling) return;
        if (target) box.insertBefore(it, target); else box.append(it);
      };
      const edge = () => {
        const m = 64;
        const v = y < m ? -12 : y > innerHeight - m ? 12 : 0;
        const sheet = box.closest('.body');
        if (v) { if (sheet) sheet.scrollTop += v; else scrollBy(0, v); place(); }
        scrolling = v ? requestAnimationFrame(edge) : 0;
      };
      // On the window: the item moves in the page while dragged, and would lose the pointer.
      const move = e => {
        if (e.pointerId !== id) return;
        e.preventDefault();
        x = e.clientX; y = e.clientY;
        place();
        if (!scrolling) scrolling = requestAnimationFrame(edge);
      };
      const up = e => {
        if (e.pointerId !== id) return;
        removeEventListener('pointermove', move);
        removeEventListener('pointerup', up);
        removeEventListener('pointercancel', up);
        cancelAnimationFrame(scrolling);
        it.classList.remove('dragging');
        const ids = items().map(o => o.dataset.id);
        if (ids.join() !== before) done(ids, it.dataset.id, false);
      };
      addEventListener('pointermove', move, { passive: false });
      addEventListener('pointerup', up);
      addEventListener('pointercancel', up);
    });
    g.addEventListener('keydown', ev => {
      const d = ev.key === 'ArrowUp' || ev.key === 'ArrowLeft' ? -1 : ev.key === 'ArrowDown' || ev.key === 'ArrowRight' ? 1 : 0;
      if (!d) return;
      ev.preventDefault();
      const a = items();
      const from = a.indexOf(it);
      const to = from + d;
      if (to < 0 || to >= a.length) return;
      box.insertBefore(it, d < 0 ? a[to] : a[to].nextSibling);
      say(t('panel_moved', { a: to + 1, n: a.length }));
      done(items().map(o => o.dataset.id), it.dataset.id, true);
    });
  }
}

function grip(label) {
  return ibtn('grip', t('panel_move', { w: label }), null, 'iconbtn grip');
}

function refocus(box, id) {
  const it = box && Array.from(box.children).find(x => x.dataset.id === id);
  const g = it && it.querySelector('.grip');
  if (g) g.focus();
}

// ---- Sections.

const TABS = [['widgets', 'panel_tab_widgets', 'grid'], ['shopping', 'panel_tab_shopping', 'shopping'],
  ['settings', 'panel_tab_settings', 'sliders'], ['access', 'panel_tab_access', 'phone']];
let tab = 'widgets';

function setTab(id, keepScroll) {
  if (!TABS.some(x => x[0] === id)) id = 'widgets';
  tab = id;
  if (location.hash.slice(1) !== id) history.replaceState(null, '', id === 'widgets' ? '/' : '#' + id);
  nav.replaceChildren();
  nav.setAttribute('aria-label', t('panel_tabs'));
  for (const [k, word, icon] of TABS) {
    const b = el('button');
    b.type = 'button';
    b.append(ico(icon), el('span', null, t(word)));
    if (k === id) b.setAttribute('aria-current', 'page');
    b.addEventListener('click', () => { setTab(k); scrollTo(0, 0); });
    nav.append(b);
  }
  draw(keepScroll);
}

function draw(keepScroll) {
  const y = scrollY;
  clearInterval(heroTimer);
  clear();
  ({ widgets: drawWidgets, shopping: drawShopping, settings: drawSettings, access: drawAccess })[tab]();
  if (keepScroll) scrollTo(0, y);
}

// ---- Widgets: the turn on the home, the pages that are on, those switched off, add.

let heroAt = 0;
let heroTimer = 0;
let heroPaused = false;

function drawWidgets() {
  if (!D.widgets || !D.widgets.ok) {
    app.append(settingsCard(['widgets_every']));
    return;
  }
  const pages = D.widgets.pages || [];
  const cols = el('div', 'cols');
  const hero = el('section', 'hero');
  const right = el('div');
  // The turn: the pages that are on and have something to show now, as the screen skips the others.
  const turn = pages.filter(p => p.on && p.model && !p.model.sample);
  if (heroAt >= turn.length) heroAt = 0;
  const stage = el('div');
  const dots = el('div', 'dots');
  const bar = el('div', 'bar');
  const nm = el('div', 'nm');
  const show = () => {
    stage.replaceChildren(); dots.replaceChildren(); nm.replaceChildren();
    if (!turn.length) { stage.append(el('p', 'hint', t('panel_turn_none'))); bar.hidden = true; return; }
    const p = turn[heroAt];
    const b = el('button', 'tap');
    b.type = 'button';
    b.setAttribute('aria-label', t('panel_edit_widget', { w: p.label }));
    b.append(preview(p));
    b.addEventListener('click', () => openEditor(p.kind));
    stage.append(b);
    turn.forEach((q, i) => {
      const d = el('button');
      d.type = 'button';
      d.setAttribute('aria-label', q.label);
      d.setAttribute('aria-current', String(i === heroAt));
      d.addEventListener('click', () => { heroAt = i; show(); });
      dots.append(d);
    });
    nm.append(el('b', null, p.label), el('span', 'hint', t('panel_turn_line', { a: heroAt + 1, n: turn.length })));
  };
  const step = d => { heroAt = (heroAt + d + turn.length) % turn.length; show(); };
  const pp = ibtn(heroPaused ? 'play' : 'pause', t('panel_turn_pause'), () => { heroPaused = !heroPaused; draw(true); });
  bar.append(ibtn('left', t('panel_turn_prev'), () => step(-1)), nm, pp, ibtn('right', t('panel_turn_next'), () => step(1)));
  hero.append(el('div', 'cap', t('panel_now_on_home')), stage, dots, bar);
  const every = item('widgets_every');
  if (every) {
    const l = el('label', 'every', t('panel_turn_every'));
    const c = control(every, D.values.widgets_every, async v => {
      const r = await api('/api/set', { key: 'widgets_every', value: v });
      if (took(r)) toast(t('panel_saved')); else toast(errorText(r.error), true);
    });
    l.append(c);
    hero.append(l);
  }
  hero.append(el('p', 'hint', t('panel_turn_skip')));
  show();
  if (!heroPaused && turn.length > 1 && !matchMedia('(prefers-reduced-motion: reduce)').matches) {
    heroTimer = setInterval(() => { if (!document.querySelector('.scrim') && !document.hidden) step(1); }, 4500);
  }
  // The pages that are on, in the turn's order.
  const sec = el('div', 'sec');
  sec.append(el('h2', null, t('panel_your_widgets')), el('span', 'hint', t('panel_drag_hint')));
  const grid = el('div', 'grid');
  grid.id = 'wlist';
  for (const p of pages.filter(p => p.on)) grid.append(widgetItem(p));
  sortable(grid, '.wi', async (ids, id, keys) => {
    const order = ids.concat(pages.filter(p => !p.on).map(p => p.kind));
    const r = await api('/api/widgets/order', { order });
    if (took(r)) { if (!keys) toast(t('panel_order_saved')); }
    else toast(errorText(r.error), true);
    draw(true);
    if (keys) refocus(document.getElementById('wlist'), id);
  });
  const add = el('button', 'addtile');
  add.type = 'button';
  add.append(ico('plus'), el('span', null, t('panel_add_widget')));
  add.addEventListener('click', openGallery);
  right.append(sec, grid, add);
  const offs = pages.filter(p => !p.on);
  if (offs.length) {
    const s2 = el('div', 'sec');
    s2.append(el('h2', null, t('panel_off_widgets')), el('span', 'hint', t('panel_off_hint')));
    const g2 = el('div', 'grid');
    for (const p of offs) g2.append(widgetItem(p));
    right.append(s2, g2);
  }
  cols.append(hero, right);
  app.append(cols);
}

function widgetItem(p) {
  const w = el('article', 'wi' + (p.on ? '' : ' off'));
  w.dataset.id = p.kind;
  const open = el('button', 'open');
  open.type = 'button';
  open.setAttribute('aria-label', t('panel_edit_widget', { w: p.label }));
  open.append(preview(p));
  open.addEventListener('click', () => openEditor(p.kind));
  const foot = el('div', 'foot');
  const nm = el('div', 'nm');
  nm.append(el('b', null, p.label), el('span', null, p.status));
  nm.addEventListener('click', () => openEditor(p.kind));
  if (p.on) foot.append(grip(p.label));
  foot.append(nm, onoff(p.kind));
  w.append(open, foot);
  return w;
}

// The switch of a page: on puts it in the turn (where it was), off moves it to «Switched off».
function onoff(kind, after) {
  const p = page(kind);
  const s = el('input', 'sw');
  s.type = 'checkbox';
  s.setAttribute('role', 'switch');
  s.checked = !!p.on;
  s.setAttribute('aria-label', t('panel_show_on_home', { w: p.label }));
  s.addEventListener('change', async () => {
    const on = D.widgets.pages.filter(x => x.kind === kind ? s.checked : x.on).map(x => x.kind);
    const r = await api('/api/set', { key: 'widgets', value: on });
    if (took(r)) toast(t(s.checked ? 'panel_widget_on' : 'panel_widget_off', { w: p.label }));
    else { toast(errorText(r.error), true); s.checked = !s.checked; }
    if (after) after(); else draw(true);
  });
  return s;
}

// ---- Sheets: the whole screen on a phone, a box on a desk; Esc and the back arrow close.

function sheet(title, extra, onClose) {
  const scrim = el('div', 'scrim');
  const sh = el('div', 'sheet');
  sh.setAttribute('role', 'dialog');
  sh.setAttribute('aria-modal', 'true');
  sh.setAttribute('aria-label', title);
  const head = el('div', 'sh');
  const back = document.activeElement;
  const close = () => {
    scrim.remove();
    document.removeEventListener('keydown', esc);
    if (onClose) onClose();
    if (back && back.isConnected) back.focus();
  };
  const esc = e => { if (e.key === 'Escape') close(); };
  head.append(ibtn('back', t('panel_close'), close), el('h2', null, title));
  if (extra) head.append(extra);
  const fix = el('div', 'fix');
  const body = el('div', 'body');
  sh.append(head, fix, body);
  scrim.append(sh);
  scrim.addEventListener('click', e => { if (e.target === scrim) close(); });
  document.addEventListener('keydown', esc);
  document.body.append(scrim);
  head.firstChild.focus();
  return { fix, body, close };
}

function privacy(text, net) {
  const p = el('div', 'priv');
  p.append(ico(net ? 'cities' : 'house'), el('span', null, text));
  return p;
}

const NET = { quotes: 1, cities: 1 };

async function openGallery() {
  if (!D.samples) {
    const r = await api('/api/widgets/samples');
    if (!r.ok) { toast(errorText(r.error), true); return; }
    D.samples = r.samples;
  }
  const { fix, body, close } = sheet(t('panel_add_widget'));
  fix.remove();
  body.append(el('p', 'hint', t('panel_gallery_intro')));
  const gal = el('div', 'gal');
  for (const p of D.widgets.pages) {
    const s = D.samples.find(x => x.kind === p.kind);
    const g = el('div', 'g');
    const tl = el('div', 'tl');
    tl.append(el('b', null, p.label));
    if (p.on) tl.append(el('span', 'badge', t('panel_on_home_badge')));
    else {
      const b = el('button', 'primary', t('panel_add'));
      b.type = 'button';
      b.setAttribute('aria-label', t('panel_add') + ': ' + p.label);
      b.addEventListener('click', async () => {
        b.disabled = true;
        const pages = D.widgets.pages;
        const on = pages.filter(x => x.on).map(x => x.kind).concat(p.kind);
        const order = on.concat(pages.filter(x => !x.on && x.kind !== p.kind).map(x => x.kind));
        const r1 = await api('/api/widgets/order', { order });
        const r2 = r1.ok ? await api('/api/set', { key: 'widgets', value: on }) : r1;
        took(r1); took(r2);
        if (!r2.ok) { b.disabled = false; toast(errorText(r2.error), true); return; }
        close();
        draw(true);
        openEditor(p.kind);
        toast(t('panel_added_end', { w: p.label }));
      });
      tl.append(b);
    }
    const src = el('div', 'src');
    src.append(ico(NET[p.kind] ? 'cities' : 'house'), el('span', null, t('panel_src_' + p.kind)));
    g.append(drawCard(p.kind, s ? s.model : null, 0, { title: p.label, text: '' }), tl, el('p', null, t('panel_what_' + p.kind)), src);
    gal.append(g);
  }
  for (const [k, icon, net] of [['trmnl', 'frame', 1], ['energy', 'bolt', 0]]) {
    const g = el('div', 'g soon');
    const tl = el('div', 'tl');
    tl.append(el('b', null, t('panel_name_' + k)), el('span', 'badge soon', t('panel_soon')));
    const card = drawCard(icon, { title: t('panel_name_' + k), layout: 'image', windows: [{ aside: '', rows: [] }] }, 0, { text: '' });
    const src = el('div', 'src');
    src.append(ico(net ? 'cities' : 'house'), el('span', null, t('panel_src_' + k)));
    g.append(card, tl, el('p', null, t('panel_what_' + k)), src);
    gal.append(g);
  }
  body.append(gal);
}

// ---- The editor of a page: its preview on top (it changes with every change), the page's
// own choices below.

let editorFill = null;

function openEditor(kind) {
  document.querySelectorAll('.scrim').forEach(x => x.remove());
  const p = page(kind);
  if (!p) return;
  let win = 0;
  const holder = el('span');
  const { fix, body } = sheet(p.label, holder, () => { editorFill = null; draw(true); });
  const fill = () => {
    const y = body.scrollTop;
    const searching = document.activeElement && document.activeElement.type === 'search';
    const q = page(kind);
    holder.replaceChildren(onoff(kind, fill));
    fix.replaceChildren();
    const m = q.model;
    const n = m && m.windows ? m.windows.length : 1;
    if (win >= n) win = n - 1;
    const line = el('div', 'hint');
    line.append(el('span', null, t(m && m.sample ? 'panel_preview_sample' : 'panel_preview')));
    if (n > 1) {
      const pg = el('span');
      pg.append(ibtn('left', t('panel_rows_prev'), () => { win = (win + n - 1) % n; fill(); }),
        ibtn('right', t('panel_rows_next'), () => { win = (win + 1) % n; fill(); }));
      line.append(pg);
    }
    fix.append(preview(q, win), line);
    body.replaceChildren();
    (EDIT[kind] || (() => {}))(body, fill, q, w => { win = w; });
    body.scrollTop = y;
    const again = searching && body.querySelector('input[type=search]');
    if (again) again.focus();
  };
  editorFill = fill;
  fill();
}

// Redraws the open editor (or the section) after an answer about the widget.
function redraw() { if (editorFill) editorFill(); else draw(true); }

const LIST = { quotes: 'quotes', cities: 'cities', sensors: 'sensors' };

// A list of chosen things: handle, name and detail, its value now (from the preview), bin.
function chosen(kind, q) {
  const items = D.widgets[kind] || [];
  const ul = el('ul', 'list');
  if (!items.length) ul.append(el('li', 'empty', t('panel_' + kind + '_empty')));
  const rows = (q.model && q.model.windows || []).flatMap(w => w.rows);
  for (const it of items) {
    const li = el('li');
    li.dataset.id = it.ref;
    const nm = el('div', 'nm');
    nm.append(el('b', null, it.name), el('span', null, it.detail));
    const r = rows.find(x => x.text === it.name);
    const val = el('div', 'val');
    if (r) {
      if (kind === 'cities') val.textContent = [r.left, r.right].filter(Boolean).join(' · ');
      else {
        val.append(el('div', null, r.right || ''));
        if (r.change) val.append(el('div', kind === 'quotes' && r.trend ? (r.trend > 0 ? 'up' : 'dn') : '', r.change));
      }
    }
    li.append(grip(it.name), nm, val, ibtn('trash', t('panel_remove_x', { w: it.name }), async () => {
      const a = await api('/api/' + kind + '/remove', { ref: it.ref });
      if (!took(a)) { toast(errorText(a.error), true); return; }
      redraw();
      toast(t('panel_removed', { w: short(it.name) }), a.undo ? { undo: undo } : {});
    }));
    ul.append(li);
  }
  sortable(ul, 'li', async (ids, id, keys) => {
    const a = await api('/api/' + kind + '/move', { ref: id, to: ids.indexOf(id) });
    if (!took(a)) toast(errorText(a.error), true);
    redraw();
    if (keys) refocus(document.querySelector('.sheet .list'), id);
  });
  return ul;
}

async function undo() {
  const a = await api('/api/undo', {});
  if (took(a)) redraw(); else toast(errorText(a.error), true);
}

function count(kind, word) {
  const s = el('div', 'sec');
  s.append(el('h2', null, t(word)), el('span', 'hint', t('panel_count', { a: (D.widgets[kind] || []).length, n: D.widgets.limits[kind] })));
  return s;
}

// Search as you type: 400 ms after the last key, at most one search every 2 s (the body's
// limit); the names first, then (markets, cities) each hit drawn as the row it will become.
// A tap adds it; the hits stay, the chosen one marked.
const finds = {};

function finder(what, kind, has, add) {
  const box = el('div');
  const f = el('div', 'find');
  const i = el('input');
  i.type = 'search';
  i.maxLength = 40;
  i.placeholder = t('panel_search_' + what);
  i.setAttribute('aria-label', t('panel_search_' + what));
  const st = finds[what] || (finds[what] = { q: '', hits: null, id: '', last: 0, timer: 0, seq: 0 });
  i.value = st.q;
  f.append(ico('search'), i);
  const out = el('ul', 'list hits');
  out.setAttribute('aria-live', 'polite');
  const show = () => {
    out.replaceChildren();
    out.hidden = !st.q;
    if (!st.q) return;
    if (st.hits === null) { out.append(el('li', 'empty', t('panel_searching'))); return; }
    if (typeof st.hits === 'string') { out.append(el('li', 'empty', st.hits)); return; }
    if (!st.hits.length) { out.append(el('li', 'empty', t('panel_search_none'))); return; }
    for (const h of st.hits) {
      const li = el('li');
      const nm = el('div', 'nm');
      nm.append(el('b', null, h.name), el('span', null, h.detail));
      li.append(nm);
      if (what === 'quote' || what === 'city') {
        const val = el('div', 'val');
        const r = h.row;
        if (!r) val.textContent = st.rows ? '' : '…';
        else if (what === 'city') {
          if (r.note) li.append(ico(SKY[r.note] || 'cloud'));
          val.textContent = [r.left, r.right].filter(Boolean).join(' · ');
        } else {
          if (r.curve) li.append(curve(r.curve));
          val.append(el('div', null, r.right || ''));
          if (r.change) val.append(el('div', r.trend ? (r.trend > 0 ? 'up' : 'dn') : '', r.change));
        }
        li.append(val);
      }
      const a = el('span', 'add');
      if (has(h)) {
        a.classList.add('have');
        a.append(ico('ok'), el('span', null, t('panel_already')));
      } else {
        a.append(ico('plus'), el('span', null, t(what === 'player' ? 'panel_choose' : 'panel_add')));
        li.tabIndex = 0;
        li.setAttribute('role', 'button');
        li.setAttribute('aria-label', t(what === 'player' ? 'panel_choose' : 'panel_add') + ': ' + h.name);
        const go = async () => {
          if (li.dataset.busy) return;
          li.dataset.busy = '1';
          const r = await add(h, st.id);
          delete li.dataset.busy;
          if (r && !r.ok) toast(errorText(r.error), true);
        };
        li.addEventListener('click', go);
        li.addEventListener('keydown', e => { if (e.key === 'Enter' || e.key === ' ') { e.preventDefault(); go(); } });
      }
      li.append(a);
      out.append(li);
    }
  };
  const run = async () => {
    const seq = ++st.seq;
    const wait = st.last + 2100 - Date.now();
    if (wait > 0) { st.timer = setTimeout(run, wait); return; }
    st.last = Date.now();
    const q = st.q;
    const r = await api('/api/search', { what, q });
    if (seq !== st.seq) return;
    if (!r.ok) { st.hits = errorText(r.error); if (i.isConnected) show(); return; }
    st.id = r.id;
    let s = { status: 'waiting' };
    // Names within 30 s (a search may wait for Yahoo's pause and two sources), then the rows
    // a few seconds more.
    for (let n = 0; n < 76 && s.ok !== false; n++) {
      await new Promise(res => setTimeout(res, 400));
      if (seq !== st.seq) return;
      s = await api('/api/search/' + encodeURIComponent(r.id));
      if (s.status === 'done') {
        st.hits = s.hits;
        st.rows = s.rows;
        const live = document.querySelector('.sheet') && box.isConnected;
        if (live) show();
        if (s.rows || (what !== 'quote' && what !== 'city') || n > 70) { st.rows = true; if (live) show(); return; }
      } else if (s.status !== 'waiting') break;
    }
    if (st.hits === null) { st.hits = t('panel_search_failed'); show(); }
  };
  i.addEventListener('input', () => {
    clearTimeout(st.timer);
    st.seq++;
    st.q = i.value.trim();
    st.hits = null;
    st.rows = false;
    show();
    if (st.q) st.timer = setTimeout(run, 400);
  });
  show();
  box.append(f, out);
  return box;
}

// Adds a hit to a list; the answer's pages show it in the preview at once.
function adder(kind, onWin) {
  return async (h, search) => {
    const a = await api(kind === 'player' ? '/api/player' : '/api/' + kind + '/add', { search, ref: h.ref });
    if (!took(a)) return a;
    if (kind !== 'player') {
      const n = (D.widgets[kind] || []).length;
      onWin(Math.floor(Math.max(0, n - 1) / 4));
      toast(t('panel_added', { w: short(h.name) }), {
        undo: async () => { const r = await api('/api/' + kind + '/remove', { ref: h.ref }); if (took(r)) redraw(); },
      });
    } else toast(t('panel_saved'));
    redraw();
    return a;
  };
}

function listEditor(kind, what, word, privacyKey) {
  return (b, fill, q, onWin) => {
    const items = D.widgets[kind] || [];
    b.append(count(kind, word), chosen(kind, q));
    if (items.length > 4) b.append(el('p', 'hint', t('panel_four_rows')));
    if (items.length < D.widgets.limits[kind]) {
      b.append(finder(what, kind, h => items.some(x => x.ref === h.ref), adder(kind, onWin)));
    } else b.append(el('p', 'hint', t('panel_list_full')));
    if (kind === 'quotes') b.append(settingsCard(['widgets_currency'], redraw));
    b.append(privacy(t(privacyKey), NET[kind]));
    if (NET[kind]) b.append(el('p', 'hint', t('panel_feeds_credit')));
  };
}

const EDIT = {
  quotes: listEditor('quotes', 'quote', 'panel_shown_quotes', 'panel_feeds_privacy'),
  cities: listEditor('cities', 'city', 'panel_shown_cities', 'panel_src_cities'),
  sensors: (b, fill, q, onWin) => {
    if (!D.widgets.ha) { b.append(el('p', 'hint', t('panel_no_ha'))); return; }
    listEditor('sensors', 'sensor', 'panel_shown_sensors', 'panel_ha_privacy')(b, fill, q, onWin);
  },
  notes: b => notesEditor(b),
  check: b => {
    const s = el('div', 'sec');
    s.append(el('h2', null, t('panel_check_what')));
    b.append(s, settingsCard(['check_openings', 'check_batteries'], redraw), el('p', 'hint', t('panel_sample_note')), privacy(t('panel_src_check')));
  },
  playing: b => {
    if (!D.widgets.ha) { b.append(el('p', 'hint', t('panel_no_ha'))); return; }
    const s = el('div', 'sec');
    s.append(el('h2', null, t('panel_which_player')));
    const ul = el('ul', 'list');
    const pl = D.widgets.player;
    if (pl) {
      const li = el('li');
      const nm = el('div', 'nm');
      nm.append(el('b', null, pl.name), el('span', null, t('panel_player_chosen')));
      li.append(nm, ibtn('trash', t('panel_remove_x', { w: pl.name }), async () => {
        const a = await api('/api/player', { ref: '' });
        if (!took(a)) { toast(errorText(a.error), true); return; }
        redraw();
        toast(t('panel_removed', { w: pl.name }), a.undo ? { undo: undo } : {});
      }));
      ul.append(li);
    } else ul.append(el('li', 'empty', t('panel_player_none_long')));
    b.append(s, ul, finder('player', 'player', h => pl && pl.ref === h.ref, adder('player', () => {})),
      el('p', 'hint', t('panel_playing_note')), privacy(t('panel_src_playing')));
  },
  home: b => {
    if (!D.widgets.ha) { b.append(el('p', 'hint', t('panel_no_ha'))); return; }
    const s = el('div', 'sec');
    s.append(el('h2', null, t('panel_who')), el('span', 'hint', t('panel_who_hint')));
    const c = el('div', 'card');
    const people = D.widgets.people || { all: [], chosen: [] };
    for (const p of people.all) {
      const l = el('label', 'chk');
      const x = el('input');
      x.type = 'checkbox';
      x.value = p.ref;
      x.checked = people.chosen.includes(p.ref);
      x.addEventListener('change', async () => {
        const refs = Array.from(c.querySelectorAll('input:checked')).map(y => y.value);
        const a = await api('/api/people', { refs });
        if (took(a)) redraw(); else toast(errorText(a.error), true);
      });
      l.append(x, el('span', null, p.name));
      c.append(l);
    }
    b.append(s, c, privacy(t('panel_src_home')));
  },
  next: b => {
    const s = el('div', 'sec');
    const it = item('calendars');
    s.append(el('h2', null, it ? it.label : ''), el('span', 'hint', t('panel_calendars_hint')));
    b.append(s, settingsCard(['calendars'], redraw), el('p', 'hint', t('panel_next_note')), privacy(t('panel_src_next')));
  },
  shopping: (b, fill) => {
    const s = el('div', 'sec');
    const it = item('lists');
    s.append(el('h2', null, it ? it.label : ''));
    const go = el('button', 'ghost', t('panel_open_shopping'));
    go.type = 'button';
    go.addEventListener('click', () => { document.querySelectorAll('.scrim').forEach(x => x.remove()); editorFill = null; setTab('shopping'); });
    b.append(s, settingsCard(['lists'], redraw), go, privacy(t('panel_src_shopping')));
  },
  weather: b => {
    b.append(el('p', 'hint', t('panel_weather_note')), privacy(t('panel_src_weather')));
  },
};

// The board: a note per line, the pencil puts it back in the box, the bin removes it (Undo
// writes it again); 280 characters at most.
function notesEditor(b) {
  const s = el('div', 'sec');
  s.append(el('h2', null, t('panel_notes')));
  b.append(s);
  const data = D.notes;
  if (!data || !data.ok) { b.append(el('p', 'hint', t('panel_no_brain'))); return; }
  const ul = el('ul', 'list');
  if (data.notes.length === 0) ul.append(el('li', 'empty', t('panel_notes_empty')));
  let editing = null;
  const box = el('textarea');
  box.maxLength = 280;
  box.rows = 2;
  box.placeholder = t('panel_notes_placeholder');
  box.setAttribute('aria-label', t('panel_notes_placeholder'));
  const cnt = el('span', 'hint', '0 / 280');
  box.addEventListener('input', () => { cnt.textContent = box.value.length + ' / 280'; });
  const go = el('button', 'primary', t('panel_add'));
  go.type = 'button';
  const after = async () => {
    await new Promise(res => setTimeout(res, 700));
    D.notes = await api('/api/notes');
    took(await api('/api/widgets'));
    redraw();
  };
  for (const n of data.notes) {
    const li = el('li', 'wrap');
    const nm = el('div', 'nm');
    nm.append(el('b', null, n.text));
    li.append(nm, ibtn('pencil', t('panel_edit'), () => {
      editing = n.id; box.value = n.text; cnt.textContent = box.value.length + ' / 280';
      go.textContent = t('panel_save'); box.focus();
    }), ibtn('trash', t('panel_remove_x', { w: n.text.slice(0, 40) }), async () => {
      const r = await api('/api/notes/remove', { id: n.id });
      if (!r.ok) { toast(errorText(r.error), true); return; }
      li.remove();
      toast(t('panel_removed', { w: t('panel_notes') }), {
        undo: async () => { const a = await api('/api/notes/add', { text: n.text }); if (a.ok) after(); else toast(errorText(a.error), true); },
      });
      after();
    }));
    ul.append(li);
  }
  go.addEventListener('click', async () => {
    const text = box.value.trim();
    if (!text) { box.focus(); return; }
    go.disabled = true;
    const r = editing === null ? await api('/api/notes/add', { text }) : await api('/api/notes/edit', { id: editing, text });
    go.disabled = false;
    if (!r.ok) { toast(errorText(r.error), true); return; }
    toast(t('panel_saved'));
    after();
  });
  const row = el('div', 'row');
  const gap = el('span');
  gap.className = 'grow';
  row.append(cnt, gap, go);
  b.append(ul, box, row, privacy(t('panel_notes_hint')));
}

// ---- Shopping: tick = bought, bin = removed (Undo adds it again), a box to add.

function drawShopping() {
  const lists = (D.shopping && D.shopping.lists) || [];
  if (!lists.length) { app.append(el('p', 'hint', t('panel_shopping_none'))); return; }
  const fresh = async () => {
    await new Promise(res => setTimeout(res, 1500));
    const r = await api('/api/shopping');
    if (r.ok) { D.shopping = r; if (tab === 'shopping') draw(true); }
  };
  lists.forEach((l, li) => {
    const s = el('div', 'sec');
    s.append(el('h2', null, l.name), el('span', 'hint', t('panel_to_buy', { n: l.items.length })));
    const ul = el('ul', 'list');
    if (!l.items.length) ul.append(el('li', 'empty', t('panel_shopping_empty')));
    for (const it of l.items) {
      const row = el('li');
      const x = el('input');
      x.type = 'checkbox';
      x.setAttribute('aria-label', t('panel_bought_label', { w: it.text }));
      x.addEventListener('change', async () => {
        x.disabled = true;
        const r = await api('/api/shopping/done', { list: l.entity, uid: it.uid });
        if (r.ok) { D.shopping = r; toast(t('panel_bought', { w: short(it.text) })); draw(true); }
        else { x.checked = false; x.disabled = false; toast(errorText(r.error), true); }
      });
      const nm = el('div', 'nm');
      nm.append(el('b', null, it.text));
      row.append(x, nm, ibtn('trash', t('panel_remove_x', { w: it.text }), async () => {
        const r = await api('/api/shopping/remove', { list: l.entity, uid: it.uid });
        if (!r.ok) { toast(errorText(r.error), true); return; }
        D.shopping = r;
        draw(true);
        toast(t('panel_removed', { w: short(it.text) }), {
          undo: async () => { const a = await api('/api/shopping/add', { list: l.entity, text: it.text }); if (a.ok) fresh(); else toast(errorText(a.error), true); },
        });
      }));
      ul.append(row);
    }
    const f = el('form', 'add');
    const i = el('input');
    i.type = 'text';
    i.maxLength = 200;
    i.required = true;
    i.placeholder = t('panel_add_to', { w: l.name });
    i.setAttribute('aria-label', i.placeholder);
    const b = el('button', 'primary', t('panel_shopping_add'));
    f.append(i, b);
    f.addEventListener('submit', async ev => {
      ev.preventDefault();
      const text = i.value.trim();
      if (!text) return;
      b.disabled = true;
      const r = await api('/api/shopping/add', { list: l.entity, text });
      b.disabled = false;
      if (!r.ok) { toast(errorText(r.error), true); return; }
      i.value = '';
      const shown = el('li', 'pending');
      const nm = el('div', 'nm');
      nm.append(el('b', null, text));
      shown.append(nm);
      ul.querySelectorAll('li.empty').forEach(x => x.remove());
      ul.append(shown);
      i.focus();
      // Home Assistant sends the list again in a moment.
      fresh().then(() => { const again = app.querySelectorAll('form.add input')[li]; if (again) again.focus(); });
    });
    app.append(s, ul, f);
  });
  app.append(el('p', 'hint', t('panel_shopping_hint')));
}

// ---- Settings: every group of the schema but the widget's (that is in its section).

function drawSettings() {
  for (const g of ((D.schema && D.schema.groups) || []).filter(g => g.key !== 'widgets')) {
    const s = el('div', 'sec');
    s.append(el('h2', null, g.label));
    app.append(s, settingsCard(g.items.map(i => i.key), () => draw(true)));
  }
}

// ---- Access: the devices let in; revoke asks twice; log out of this one.

function when(sec) {
  try {
    return new Date(sec * 1000).toLocaleString(lang, { day: 'numeric', month: 'short', hour: '2-digit', minute: '2-digit' });
  } catch (e) {
    return '';
  }
}

function drawAccess() {
  const s = el('div', 'sec');
  s.append(el('h2', null, t('panel_section_devices')));
  const ul = el('ul', 'list');
  for (const d of (D.devices && D.devices.sessions) || []) {
    const li = el('li');
    const nm = el('div', 'nm');
    const line = t('panel_device_line', { browser: d.browser, os: d.os });
    nm.append(el('b', null, line), el('span', null, d.peer + ' · ' + t('panel_last_used', { w: when(d.last_used) })));
    li.append(nm);
    if (d.current) li.append(el('span', 'badge', t('panel_devices_this')));
    else {
      const b = ibtn('trash', t('panel_revoke_x', { w: line }), null);
      let armed = 0;
      b.addEventListener('click', async () => {
        // Asks twice: a second touch within 3 s.
        if (!armed) {
          b.classList.add('armed');
          say(t('panel_revoke_sure_x', { w: line }));
          toast(t('panel_revoke_sure_x', { w: line }));
          armed = setTimeout(() => { armed = 0; b.classList.remove('armed'); }, 3000);
          return;
        }
        clearTimeout(armed);
        const r = await api('/api/sessions/revoke', { id: d.id });
        if (!r.ok) toast(errorText(r.error), true);
        const fresh = await api('/api/sessions');
        if (fresh.ok) { D.devices = fresh; draw(true); }
      });
      li.append(b);
    }
    ul.append(li);
  }
  const out = el('button', 'ghost', t('panel_logout'));
  out.type = 'button';
  out.addEventListener('click', async () => { await api('/api/logout', {}); login(); });
  app.append(s, ul, el('p', 'hint', t('panel_access_new_hint')), out, note());
}

// ---- Start.

async function main() {
  // One after the other: the panel takes few connections from one address at once.
  const schema = await api('/api/schema');
  const state = await api('/api/state');
  if (!schema.ok || !state.ok) return;
  D.schema = schema;
  D.values = state.values || {};
  D.widgets = await api('/api/widgets');
  waitData();
  D.shopping = await api('/api/shopping');
  D.notes = await api('/api/notes');
  D.devices = await api('/api/sessions');
  if (location.search) history.replaceState(null, '', '/' + location.hash);
  chrome(true);
  setTab(location.hash.slice(1) || tab, true);
}

async function start() {
  const s = await api('/api/strings');
  if (s.ok) { T = s.strings; lang = s.lang; document.documentElement.lang = lang; }
  const h = await api('/api/hello');
  if (!h.ok) {
    clear();
    app.append(el('section', 'card center', t('panel_error', { name: '' })));
    return;
  }
  name = h.name;
  document.title = name;
  document.getElementById('name').textContent = name;
  for (const k of Object.keys(T)) T[k] = T[k].split('{name}').join(name);
  const qr = new URLSearchParams(location.search).get('qr');
  if (!h.on) off();
  else if (h.session) main();
  else login(qr ? errorText(qr) : '');
}

// Back to the front: the data again (an open sheet stays as it is).
document.addEventListener('visibilitychange', () => {
  if (document.visibilityState === 'visible' && !nav.hidden && !document.querySelector('.scrim')) main();
});

start();

// SPDX-License-Identifier: GPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Andrea Mattera
// The browser panel's page: drawn from what the body says (its words, the settings' schema,
// the widget's pages as the screen draws them, the shopping lists, the devices). Text is only
// ever set as text (textContent), never as HTML; icons are constant SVG paths.
//
// Panel v5: on a desk (from 900 px) a column of sections on the left, and the widgets as a
// list with the chosen one's page beside it; on a phone one thing per page. The address says
// which page (#home, #w/<kind>, #aggiungi, #ordine, #spesa, #impostazioni[/<group>],
// #accessi, #menu): the browser's back key works.
'use strict';

const app = document.getElementById('app');
const side = document.getElementById('side');
const head = document.getElementById('top');
const NS = 'http://www.w3.org/2000/svg';
const wideQ = matchMedia('(min-width: 900px)');
let T = {};
let lang = 'en';
let name = '';
// With access: the sections are drawn (else a pairing page is).
let inside = false;

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
  // The panel serves four connections from one address at once and turns away the fifth
  // (while the fonts and pictures load, say): that one is asked again, twice at most.
  for (let n = 0; !r; n++) {
    try {
      r = await fetch(path, opt);
    } catch (e) {
      if (n >= 2) return { ok: false, error: 'network' };
      await new Promise(res => setTimeout(res, 300 * (n + 1)));
    }
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
  trash: 'M5 7h14 M10 7V5h4v2 M7 7l1 13h8l1-13 M10 11v6 M14 11v6', plus: 'M12 5v14 M5 12h14',
  search: 'M11 4a7 7 0 100 14 7 7 0 000-14z M16 16l4 4', pencil: 'M4 20l1-4L16 5l3 3L8 19z', left: 'M14 6l-6 6 6 6', right: 'M10 6l6 6-6 6',
  bars: 'M5 9h14 M5 15h14', menu: 'M4 7h16 M4 12h16 M4 17h16', x: 'M6 6l12 12 M18 6L6 18',
  grid: 'M4 4h7v7H4z M13 4h7v4h-7z M13 10h7v10h-7z M4 13h7v7H4z', sliders: 'M4 7h10 M18 7h2 M4 17h2 M10 17h10 M16 9a2 2 0 100-4 2 2 0 000 4z M8 19a2 2 0 100-4 2 2 0 000 4z',
  phone: 'M8 3h8v18H8z M11 18h2', ok: 'M5 12l5 5 9-10',
  sun: 'M12 8a4 4 0 100 8 4 4 0 000-8z M12 2v2 M12 20v2 M4.9 4.9l1.4 1.4 M17.7 17.7l1.4 1.4 M2 12h2 M20 12h2 M4.9 19.1l1.4-1.4 M17.7 6.3l1.4-1.4',
  cloud: 'M7 18h10a3.5 3.5 0 000-7 5 5 0 00-9.6-1A4 4 0 007 18z', rain: 'M7 15h10a3.5 3.5 0 000-7 5 5 0 00-9.6-1A4 4 0 007 15z M9 18l-1 2 M13 18l-1 2 M17 18l-1 2',
  snow: 'M7 15h10a3.5 3.5 0 000-7 5 5 0 00-9.6-1A4 4 0 007 15z M9 19h.1 M13 19h.1 M17 19h.1',
  rise: 'M3 18h18 M7 18a5 5 0 0110 0 M12 3v6 M9 6l3-3 3 3', set: 'M3 18h18 M7 18a5 5 0 0110 0 M12 3v6 M9 6l3 3 3-3',
  arrow: 'M12 3l6 17-6-4-6 4z',
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

function button(cls, text, fn) {
  const b = el('button', cls, text);
  b.type = 'button';
  if (fn) b.addEventListener('click', fn);
  return b;
}

// A link to a page of the panel (an address after #).
function link(cls, to, text) {
  const a = el('a', cls, text);
  a.href = '#' + to;
  return a;
}

function frag(...nodes) {
  const f = document.createDocumentFragment();
  for (const n of nodes) if (n) f.append(n);
  return f;
}

// One of Alfred's pictures (tools/make-art.py).
function art(pose, cls) {
  const i = el('img', cls);
  i.src = '/art/' + pose + '.webp';
  i.alt = '';
  i.decoding = 'async';
  return i;
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
    b.append(button(null, t('panel_undo'), () => { b.remove(); o.undo(); }));
  }
  document.body.append(b);
  toastTimer = setTimeout(() => b.remove(), o.undo ? 7000 : 2200);
}

// A name in a short line (a toast).
function short(s) { return s.length > 40 ? s.slice(0, 39) + '…' : s; }

// ---- Without access: ask for it, or follow the QR code's answer. A column in the middle:
// Alfred, a title, a line, one button.

function chrome(within) {
  inside = within;
  document.body.className = within ? 'in' : 'out';
  if (!within) {
    side.replaceChildren();
    const face = art('face');
    face.width = 38; face.height = 38;
    head.replaceChildren(face, el('span', 'name', name));
  }
}

function pairPage(pose, title, text, extra) {
  clearInterval(polling);
  chrome(false);
  const d = el('div', 'pair');
  d.append(art(pose), el('h1', null, title));
  if (text) d.append(el('p', null, text));
  for (const x of extra || []) d.append(x);
  app.replaceChildren(d);
}

function off() {
  pairPage('sleeping', t('panel_off_title'), t('panel_off'));
}

let polling = 0;

// `message`: why the last try did not let this device in; `title` instead of the usual one.
function login(message, title) {
  pairPage(message ? 'error' : 'idle', title || t('panel_pair_title'), message || t('panel_ask_line'),
    [button('primary', t('panel_ask'), ask)]);
}

async function ask() {
  const r = await api('/api/pair', {});
  if (!r.ok) { login(errorText(r.error)); return; }
  // The screen shows the same code, and Allow to touch.
  const cancel = el('div', 'center');
  cancel.append(button('link', t('panel_cancel'), () => login()));
  pairPage('listening', t('panel_code_title'), t('panel_code_text'), [el('div', 'code', r.code), cancel]);
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
// What only this page remembers: a list being reordered, the window of rows each preview shows.
const S = { sorting: {}, win: {} };

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
  const waiting = pages().some(p => p.model && p.model.windows.some(w => w.rows.some(r => r.wait)));
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
    if (took(r)) render(true);
  }, 3000);
}

function pages() { return (D.widgets && D.widgets.pages) || []; }
function page(kind) { return pages().find(p => p.kind === kind); }
function onPages() { return pages().filter(p => p.on); }

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
    const out = el('output', null, value);
    i.addEventListener('input', () => { out.textContent = i.value; });
    i.addEventListener('change', () => onChange(Number(i.value)));
    w.append(i, out);
    return w;
  }
  return checkRows(it.options.map(o => ({ ref: o.value, name: o.label })), value, onChange, it.label, true);
}

// Some of a list as rows with a tick: `all` (the lists, the calendars) is an empty choice
// meaning all of them, drawn all ticked.
function checkRows(options, value, onChange, label, all) {
  const ul = el('ul', 'rows');
  ul.setAttribute('role', 'group');
  ul.setAttribute('aria-label', label);
  const every = all && Array.isArray(value) && value.length === 0;
  for (const o of options) {
    const li = el('li');
    const l = el('label');
    const c = el('input');
    c.type = 'checkbox';
    c.value = o.ref;
    c.checked = every || (Array.isArray(value) && value.includes(o.ref));
    c.addEventListener('change', () => onChange(Array.from(ul.querySelectorAll('input:checked')).map(x => x.value)));
    l.append(c, el('span', null, o.name));
    li.append(l);
    ul.append(li);
  }
  if (!options.length) ul.append(el('li', 'empty', t('panel_nothing_yet')));
  return ul;
}

// Saves a setting at once; the page is drawn again with the body's answer.
async function save(it, v) {
  const r = await api('/api/set', { key: it.key, value: v });
  if (took(r)) {
    toast(t('panel_saved'));
    if (it.key === 'language') { location.reload(); return; }
  } else toast(errorText(r.error), true);
  render(true);
}

// A setting: its label over the control (a switch: the label in its row), its line of help.
function setting(it, noHelp) {
  const id = 'set-' + it.key;
  const c = control(it, D.values[it.key], v => save(it, v));
  let f;
  if (it.kind === 'bool') {
    const ul = el('ul', 'rows swrow');
    const li = el('li');
    const l = el('label');
    const nm = el('span', 'nm');
    nm.append(el('b', 'plain', it.label));
    l.append(nm, c);
    li.append(l);
    ul.append(li);
    f = frag(ul);
  } else {
    const l = el('label', 'label', it.label);
    if (it.kind !== 'multi') { (c.tagName === 'SELECT' ? c : c.querySelector('input')).id = id; l.htmlFor = id; }
    f = frag(l, c);
  }
  if (it.help && !noHelp) f.append(el('p', 'help small', it.help));
  return f;
}

// ---- The widget's card as the screen draws it: header (icon, title, the right side), then
// the rows of one window (curve rows, timed rows, plain rows) or the weather's own drawing.

function drawCard(kind, model, win, emptyText) {
  const c = el('div', 'wc');
  const hd = el('div', 'hd');
  const w = model && model.windows && model.windows.length ? model.windows[(win || 0) % model.windows.length] : null;
  hd.append(ico(kind), el('span', 't', model ? model.title : (page(kind) || {}).label || ''));
  if (w && w.aside) hd.append(el('span', 'a', w.aside));
  c.append(hd);
  if (model && model.layout === 'weather' && model.hero) return wrapCard(weatherCard(c, model.hero, w ? w.rows : []));
  if (!w || !w.rows.length) { c.append(el('div', 'empty', emptyText)); return wrapCard(c); }
  if (model.layout === 'hours') { drawWeather(c, w); return wrapCard(c); }
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

// The weather as the screen draws it with three slots: the sky big on the left, a dotted line,
// what it is, the temperature big with its range beside it, sunrise, sunset and the wind's arrow;
// the next hours small under it. No header: the block says what it is.
function weatherCard(c, h, rows) {
  c.replaceChildren();
  c.classList.add('wx');
  const top = el('div', 'top');
  const sky = el('div', 'sky');
  sky.append(ico(SKY[h.kind] || 'cloud', ''));
  const right = el('div', 'main');
  const temp = el('div', 'tr');
  temp.append(el('span', 'tt', h.temp));
  const range = el('span', 'rg');
  for (const x of [h.high, h.low]) if (x) range.append(el('span', null, x));
  temp.append(range);
  const facts = el('div', 'fx');
  if (h.sunrise) facts.append(ico('rise', 'g'), el('span', null, h.sunrise));
  if (h.sunset) facts.append(ico('set', 'g'), el('span', null, h.sunset));
  if (h.wind) {
    if (h.wind_to >= 0) {
      // Where the wind blows to: the path turned about the box's centre (an SVG attribute).
      const a = ico('arrow', 'a');
      a.firstChild.setAttribute('transform', 'rotate(' + Number(h.wind_to) + ' 12 12)');
      facts.append(a);
    }
    facts.append(el('span', null, h.wind));
  }
  right.append(el('div', 'lb', h.label), temp, facts);
  top.append(sky, el('div', 'dv'), right);
  c.append(top);
  if (rows.length) {
    const hs = el('div', 'hrs');
    for (const r of rows) {
      const x = el('div');
      x.append(el('span', 'h', r.left), ico(SKY[r.note] || 'cloud', ''), el('span', 'd', r.text));
      hs.append(x);
    }
    c.append(hs);
  }
  return c;
}

// The weather's card (the model's `hours` layout): its own function, so that its drawing can
// change alone.
function drawWeather(c, w) {
  const h = el('div', 'hours');
  for (const r of w.rows) {
    const x = el('div');
    x.append(el('span', 'h', r.left), ico(SKY[r.note] || 'cloud', ''), el('span', 'd', r.text));
    h.append(x);
  }
  c.append(h);
}

function wrapCard(c) { const p = el('div', 'wp'); p.append(c); return p; }

// The preview of a page: the card (as large as the screen draws it at most), under it what it
// is and, with more rows than fit, ‹ › for the other windows.
function preview(kind, model, capKey) {
  const box = el('div', 'prev');
  const n = model && model.windows ? model.windows.length : 1;
  const win = Math.min(S.win[kind] || 0, Math.max(0, n - 1));
  const big = el('div', 'big');
  big.append(drawCard(kind, model, win, t('panel_empty_' + kind)));
  const cap = el('div', 'cap');
  cap.append(el('span', null, t(capKey)));
  if (n > 1) {
    cap.append(ibtn('left', t('panel_rows_prev'), () => { S.win[kind] = (win + n - 1) % n; render(true); }),
      ibtn('right', t('panel_rows_next'), () => { S.win[kind] = (win + 1) % n; render(true); }));
  }
  box.append(big, cap);
  return box;
}

// ---- Reordering: drag by the handle (mouse, pen or finger), or arrows on the focused handle.
// While dragging, the item goes before the item under the pointer if the pointer is above that
// item's middle, else after it.

function sortable(box, done) {
  const items = () => Array.from(box.children).filter(x => x.dataset.id);
  for (const it of items()) {
    const g = it.querySelector('.grip');
    if (!g) continue;
    g.addEventListener('pointerdown', ev => {
      if (ev.button !== 0) return;
      ev.preventDefault();
      it.classList.add('dragging');
      const before = items().map(x => x.dataset.id).join();
      let y = ev.clientY;
      let scrolling = 0;
      const id = ev.pointerId;
      const place = () => {
        const own = it.getBoundingClientRect();
        if (y >= own.top && y <= own.bottom) return;
        let best = null;
        let dist = Infinity;
        for (const o of items()) {
          if (o === it) continue;
          const r = o.getBoundingClientRect();
          const d = y < r.top ? r.top - y : y > r.bottom ? y - r.bottom : 0;
          if (d < dist) { dist = d; best = o; }
        }
        if (!best) return;
        const r = best.getBoundingClientRect();
        const target = y > r.top + r.height / 2 ? best.nextElementSibling : best;
        if (target === it || target === it.nextElementSibling) return;
        if (target) box.insertBefore(it, target); else box.append(it);
      };
      const edge = () => {
        const m = 64;
        const v = y < m ? -12 : y > innerHeight - m ? 12 : 0;
        if (v) { scrollBy(0, v); place(); }
        scrolling = v ? requestAnimationFrame(edge) : 0;
      };
      // On the window: the item moves in the page while dragged, and would lose the pointer.
      const move = e => {
        if (e.pointerId !== id) return;
        e.preventDefault();
        y = e.clientY;
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
      const d = ev.key === 'ArrowUp' ? -1 : ev.key === 'ArrowDown' ? 1 : 0;
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
  return ibtn('bars', t('panel_move', { w: label }), null, 'grip');
}

function refocus(id) {
  const it = Array.from(document.querySelectorAll('[data-id]')).find(x => x.dataset.id === id);
  const g = it && it.querySelector('.grip');
  if (g) g.focus();
}

// ---- The page drawn from the address.

const SECTIONS = [['home', 'panel_tab_widgets', 'grid'], ['spesa', 'panel_tab_shopping', 'shopping'],
  ['impostazioni', 'panel_tab_settings', 'sliders'], ['accessi', 'panel_tab_access', 'phone']];
const GROUPS = ['screen', 'voice', 'lists'];
// The addresses of the panel before v5.
const OLD = { widgets: 'home', shopping: 'spesa', settings: 'impostazioni', access: 'accessi' };

function route() {
  const [p, k] = (location.hash.slice(1) || 'home').split('/');
  return { p: OLD[p] || p || 'home', k: k ? decodeURIComponent(k) : '' };
}

function go(to) {
  if (location.hash === '#' + to) render(); else location.hash = to;
}

function render(keep) {
  if (!inside) return;
  const y = scrollY;
  const focused = document.activeElement && document.activeElement.id;
  const r = route();
  const W = wideQ.matches;
  let back = null;
  let body;
  if (W && ['home', 'menu', 'ordine', 'aggiungi', 'w'].includes(r.p)) body = desk(r);
  else if (r.p === 'menu') body = menu();
  else if (r.p === 'ordine') { back = ['home', t('panel_tab_widgets')]; body = order('h1'); }
  else if (r.p === 'aggiungi') { back = ['home', t('panel_tab_widgets')]; body = chooser(); }
  else if (r.p === 'w' && page(r.k)) {
    back = page(r.k).on ? ['home', t('panel_tab_widgets')] : ['aggiungi', t('panel_add_widget')];
    body = widgetPage(r.k);
  }
  else if (r.p === 'spesa') body = shopping();
  else if (r.p === 'impostazioni' && W) body = settingsAll();
  else if (r.p === 'impostazioni' && GROUPS.includes(r.k)) { back = ['impostazioni', t('panel_tab_settings')]; body = settingsGroup(r.k, 'h1'); }
  else if (r.p === 'impostazioni') body = settingsMenu();
  else if (r.p === 'accessi') body = access();
  else body = home();
  drawSide(r);
  drawHead(r, back);
  app.replaceChildren(body);
  if (keep) {
    scrollTo(0, y);
    const f = focused && document.getElementById(focused);
    if (f) f.focus();
  }
}

// The column on the left (a desk): Alfred, then the four sections.
function drawSide(r) {
  const sec = ['spesa', 'impostazioni', 'accessi'].includes(r.p) ? r.p : 'home';
  const who = el('div', 'who');
  const face = art('face');
  face.width = 40; face.height = 40;
  who.append(face, el('span', null, name));
  side.replaceChildren(who);
  side.setAttribute('aria-label', t('panel_tabs'));
  for (const [k, word, icon] of SECTIONS) {
    const a = link(null, k);
    a.append(ico(icon), el('span', null, t(word)));
    if (k === sec) a.setAttribute('aria-current', 'page');
    side.append(a);
  }
}

// The line on top (a phone): Alfred and ☰, or the way back.
function drawHead(r, back) {
  if (back) {
    const a = link('back', back[0]);
    a.append(ico('left'), el('span', null, back[1]));
    head.replaceChildren(a);
    return;
  }
  const face = art('face');
  face.width = 38; face.height = 38;
  const open = r.p === 'menu';
  const m = link('menubtn', open ? 'home' : 'menu');
  m.setAttribute('aria-label', t(open ? 'panel_close' : 'panel_menu'));
  m.append(ico(open ? 'x' : 'menu'));
  head.replaceChildren(face, el('span', 'name', name), m);
}

function menu() {
  const ul = el('ul', 'rows');
  for (const [k, word] of SECTIONS) ul.append(navRow(k, t(word)));
  const d = el('div', 'after');
  d.append(ul);
  return d;
}

function navRow(to, title, sub, icon, sel) {
  const li = el('li', 'nav' + (sel ? ' sel' : ''));
  const a = link(sub ? 'tall' : null, to);
  if (sel) a.setAttribute('aria-current', 'true');
  if (icon) { const w = el('span', 'wic'); w.append(ico(icon)); a.append(w); }
  const nm = el('div', 'nm');
  nm.append(el('b', null, title));
  if (sub) nm.append(el('span', null, sub));
  a.append(nm, ico('right', 'i chev'));
  li.append(a);
  return li;
}

// ---- Widgets: the list of what turns on the home, the chosen one's page beside it.

function desk(r) {
  const on = onPages();
  let k = r.p === 'w' && page(r.k) ? r.k : null;
  if (!D.widgets || !D.widgets.ok) return el('p', 'help', t('panel_error'));
  if (!on.length && r.p !== 'aggiungi' && !k) return home();
  const md = el('div', 'md');
  const master = el('section', 'master');
  const pane = el('section', 'pane');
  if (r.p === 'ordine') master.append(order('h2'));
  else {
    if (r.p !== 'aggiungi' && !k) k = on[0].kind;
    const adding = r.p === 'aggiungi' || !page(k).on;
    master.append(el('h2', 'q', t('panel_on_home_title')), list(k, adding));
    if (r.p === 'aggiungi') pane.append(chooser());
    else {
      if (!page(k).on) {
        const b = link('back', 'aggiungi');
        b.append(ico('left'), el('span', null, t('panel_add_widget')));
        pane.append(b);
      }
      pane.append(widgetPage(k));
    }
  }
  md.append(master, pane);
  return md;
}

// The widgets on the home in the turn's order; the gold button adds one (only an outline while
// the page beside it is adding one), «Change order» below.
function list(sel, adding) {
  const ul = el('ul', 'rows');
  for (const p of onPages()) ul.append(navRow('w/' + p.kind, p.label, null, p.kind, p.kind === sel));
  const f = frag(ul, link(adding ? 'ghost cur' : 'primary', 'aggiungi', t('panel_add_widget')));
  if (onPages().length > 1) {
    const c = el('div', 'center');
    c.append(link('link', 'ordine', t('panel_change_order')));
    f.append(c);
  }
  return f;
}

function home() {
  if (!D.widgets || !D.widgets.ok) return el('p', 'help', t('panel_error'));
  if (!onPages().length) {
    const d = el('div', 'emptyhome');
    d.append(art('sleeping'), el('h1', 'q', t('panel_no_widgets')), link('primary', 'aggiungi', t('panel_add_widget')));
    return d;
  }
  return frag(el('h1', 'q', t('panel_on_home_title')), list(null, false));
}

// «Change order»: the list with a handle on every row, «Done» back to the list.
function order(h) {
  const ul = el('ul', 'rows');
  for (const p of onPages()) {
    const li = el('li');
    li.dataset.id = p.kind;
    const w = el('span', 'wic');
    w.append(ico(p.kind));
    const nm = el('div', 'nm');
    nm.append(el('b', null, p.label));
    li.append(w, nm, grip(p.label));
    ul.append(li);
  }
  sortable(ul, async (ids, id, keys) => {
    const all = ids.concat(pages().filter(p => !p.on).map(p => p.kind));
    const r = await api('/api/widgets/order', { order: all });
    if (took(r)) { if (!keys) toast(t('panel_order_saved')); }
    else toast(errorText(r.error), true);
    render(true);
    if (keys) refocus(id);
  });
  return frag(el(h, h === 'h2' ? 'q' : null, t('panel_change_order')), ul, el('p', 'help', t('panel_drag_hint')),
    link('primary after', 'home', t('panel_finish')));
}

// «Add a widget»: the kinds not on the home, each with its line.
function chooser() {
  const ul = el('ul', 'rows');
  const offs = pages().filter(p => !p.on);
  for (const p of offs) ul.append(navRow('w/' + p.kind, p.label, t('panel_what_' + p.kind), p.kind));
  if (!offs.length) ul.append(el('li', 'empty', t('panel_all_on')));
  return frag(el('h1', null, t('panel_add_widget')), ul);
}

let samplesAsked = false;
async function samples() {
  if (D.samples || samplesAsked) return;
  samplesAsked = true;
  const r = await api('/api/widgets/samples');
  D.samples = r.ok ? r.samples : [];
  render(true);
}

// A widget's page: its card, its few controls, «Remove from the home». Not on the home yet:
// an example, a line, «Add to the home».
function widgetPage(kind) {
  const p = page(kind);
  const wp = el('div', 'wpage');
  const ctl = el('div', 'ctl');
  if (!p.on) {
    const m = p.model;
    const real = m && !m.sample && m.windows.some(w => w.rows.length);
    let ex = m;
    if (!real) {
      if (D.samples) ex = (D.samples.find(x => x.kind === kind) || {}).model || m;
      else samples();
    }
    ctl.append(el('p', 'help lead', t('panel_what_' + kind)), button('primary', t('panel_add_home'), ev => addToHome(kind, ev.currentTarget)));
    wp.append(preview(kind, ex, real ? 'panel_preview' : 'panel_preview_example'), ctl);
  } else {
    (EDIT[kind] || (() => {}))(ctl);
    const rm = el('div', 'remove');
    rm.append(button('ghost', t('panel_remove_home'), () => removeFromHome(kind)));
    ctl.append(rm);
    wp.append(preview(kind, p.model, p.model && p.model.sample ? 'panel_preview_sample' : 'panel_preview'), ctl);
  }
  return frag(el('h1', null, p.label), wp);
}

// On: at the end of the turn. The same page then shows its controls.
async function addToHome(kind, b) {
  b.disabled = true;
  const on = onPages().map(x => x.kind).concat(kind);
  const all = on.concat(pages().filter(x => !x.on && x.kind !== kind).map(x => x.kind));
  const r1 = await api('/api/widgets/order', { order: all });
  const r2 = r1.ok ? await api('/api/set', { key: 'widgets', value: on }) : r1;
  took(r1); took(r2);
  if (!r2.ok) { b.disabled = false; toast(errorText(r2.error), true); return; }
  toast(t('panel_now_on', { w: page(kind).label }));
  if (location.hash !== '#w/' + kind) location.hash = 'w/' + kind; else { render(); scrollTo(0, 0); }
}

// Off: back to the list; Undo puts it back where it was.
async function removeFromHome(kind) {
  const was = pages().map(x => x.kind);
  const wasOn = onPages().map(x => x.kind);
  const label = page(kind).label;
  const r = await api('/api/set', { key: 'widgets', value: wasOn.filter(k => k !== kind) });
  if (!took(r)) { toast(errorText(r.error), true); return; }
  go('home');
  toast(t('panel_removed_home', { w: label }), {
    undo: async () => {
      const a = await api('/api/widgets/order', { order: was });
      const b = a.ok ? await api('/api/set', { key: 'widgets', value: wasOn }) : a;
      took(a); took(b);
      if (!b.ok) toast(errorText(b.error), true);
      render(true);
    },
  });
}

// ---- The controls of each kind, each with its label over it, and one line of help.

function label(text, forId) {
  const l = el(forId ? 'label' : 'span', 'label', text);
  if (forId) l.htmlFor = forId;
  return l;
}

function help(key) { return el('p', 'help', t(key)); }

// The lists or the calendars shown: the same setting as in Settings.
function multiBlock(c, key) {
  const it = item(key);
  if (!it) return;
  c.append(label(it.label), control(it, D.values[key], v => save(it, v)));
  if (it.help) c.append(el('p', 'help', it.help));
}

// A list of chosen things: name and detail, its value now (from the preview), bin; in «Change
// order» a handle instead of the bin.
function chosen(kind) {
  const items = D.widgets[kind] || [];
  const sorting = !!S.sorting[kind];
  const ul = el('ul', 'rows');
  if (!items.length) ul.append(el('li', 'empty', t('panel_' + kind + '_empty')));
  const q = page(kind);
  const rows = ((q && q.model && q.model.windows) || []).flatMap(w => w.rows);
  for (const it of items) {
    const li = el('li');
    li.dataset.id = it.ref;
    const nm = el('div', 'nm');
    nm.append(el('b', null, it.name), el('span', null, it.detail));
    li.append(nm);
    const r = rows.find(x => x.text === it.name);
    if (r && !sorting) {
      const val = el('div', 'val');
      if (kind === 'cities') val.textContent = [r.left, r.right].filter(Boolean).join(' · ');
      else {
        val.append(el('div', null, r.right || ''));
        if (r.change) val.append(el('div', kind === 'quotes' && r.trend ? (r.trend > 0 ? 'up' : 'dn') : '', r.change));
      }
      li.append(val);
    }
    if (sorting) li.append(grip(it.name));
    else {
      li.append(ibtn('trash', t('panel_remove_x', { w: it.name }), async () => {
        const a = await api('/api/' + kind + '/remove', { ref: it.ref });
        if (!took(a)) { toast(errorText(a.error), true); return; }
        render(true);
        toast(t('panel_removed', { w: short(it.name) }), a.undo ? { undo: undo } : {});
      }));
    }
    ul.append(li);
  }
  if (sorting) {
    sortable(ul, async (ids, id, keys) => {
      const a = await api('/api/' + kind + '/move', { ref: id, to: ids.indexOf(id) });
      if (!took(a)) toast(errorText(a.error), true);
      render(true);
      if (keys) refocus(id);
    });
  }
  const f = frag(ul);
  if (items.length > 1) {
    const c = el('div', 'center end');
    c.append(button('link', t(sorting ? 'panel_finish' : 'panel_change_order'), () => { S.sorting[kind] = !sorting; render(true); }));
    f.append(c);
  }
  return f;
}

async function undo() {
  const a = await api('/api/undo', {});
  if (took(a)) render(true); else toast(errorText(a.error), true);
}

// Search as you type: 400 ms after the last key, at most one search every 2 s (the body's
// limit); the names first, then (markets, cities) each hit drawn as the row it will become.
// A tap adds it; the hits stay, the chosen one marked. The state outlives the page's drawing.
const finds = {};

function finder(what, labelKey, has, add) {
  const id = 'find-' + what;
  const f = el('div', 'find');
  const i = el('input');
  i.type = 'search';
  i.id = id;
  i.maxLength = 40;
  i.placeholder = t('panel_search_' + what);
  const st = finds[what] || (finds[what] = { q: '', hits: null, id: '', last: 0, timer: 0, seq: 0 });
  i.value = st.q;
  f.append(ico('search'), i);
  const out = el('ul', 'rows hits');
  out.setAttribute('aria-live', 'polite');
  const show = () => {
    out.replaceChildren();
    out.hidden = !st.q;
    if (!st.q) return;
    if (st.hits === null) {
      const li = el('li', 'empty');
      li.append(art('thinking', 'think'), el('span', null, t('panel_searching')));
      out.append(li);
      return;
    }
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
        const word = t(what === 'player' ? 'panel_choose' : 'panel_add');
        a.append(ico('plus'), el('span', null, word));
        li.tabIndex = 0;
        li.setAttribute('role', 'button');
        li.setAttribute('aria-label', word + ': ' + h.name);
        const pick = async () => {
          if (li.dataset.busy) return;
          li.dataset.busy = '1';
          const r = await add(h, st.id);
          delete li.dataset.busy;
          if (r && !r.ok) toast(errorText(r.error), true);
        };
        li.addEventListener('click', pick);
        li.addEventListener('keydown', e => { if (e.key === 'Enter' || e.key === ' ') { e.preventDefault(); pick(); } });
      }
      li.append(a);
      out.append(li);
    }
  };
  // The last drawing of this search is the one to update.
  st.show = () => { if (out.isConnected) show(); };
  const run = async () => {
    const seq = ++st.seq;
    const wait = st.last + 2100 - Date.now();
    if (wait > 0) { st.timer = setTimeout(run, wait); return; }
    st.last = Date.now();
    const q = st.q;
    const r = await api('/api/search', { what, q });
    if (seq !== st.seq) return;
    if (!r.ok) { st.hits = errorText(r.error); st.show(); return; }
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
        st.show();
        if (s.rows || (what !== 'quote' && what !== 'city') || n > 70) { st.rows = true; st.show(); return; }
      } else if (s.status !== 'waiting') break;
    }
    if (st.hits === null) { st.hits = t('panel_search_failed'); st.show(); }
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
  return frag(label(t(labelKey), id), f, out);
}

// Adds a hit to a list; the answer's pages show it in the preview at once.
function adder(kind) {
  return async (h, search) => {
    const a = await api(kind === 'player' ? '/api/player' : '/api/' + kind + '/add', { search, ref: h.ref });
    if (!took(a)) return a;
    if (kind !== 'player') {
      const n = (D.widgets[kind] || []).length;
      S.win[kind] = Math.floor(Math.max(0, n - 1) / 4);
      toast(t('panel_added', { w: short(h.name) }), {
        undo: async () => { const r = await api('/api/' + kind + '/remove', { ref: h.ref }); if (took(r)) render(true); },
      });
    } else {
      finds.player = null;
      toast(t('panel_saved'));
    }
    render(true);
    return a;
  };
}

// Markets, other cities, sensors: the chosen rows, a search to add one, the help.
function listBlock(kind, what, shownKey, addKey, helpKey) {
  return c => {
    const items = D.widgets[kind] || [];
    c.append(label(t(shownKey)), chosen(kind));
    if (items.length < D.widgets.limits[kind]) {
      c.append(finder(what, addKey, h => items.some(x => x.ref === h.ref), adder(kind)));
    } else c.append(help('panel_list_full'));
    if (kind === 'quotes') {
      const it = item('widgets_currency');
      if (it) c.append(setting(it, true));
    }
    c.append(help(helpKey));
    if (kind === 'quotes' || kind === 'cities') c.append(el('p', 'credit', t('panel_feeds_credit')));
  };
}

const EDIT = {
  next: c => multiBlock(c, 'calendars'),
  shopping: c => multiBlock(c, 'lists'),
  weather: c => c.append(el('p', 'help lead', t('panel_help_weather'))),
  quotes: listBlock('quotes', 'quote', 'panel_shown_quotes', 'panel_add_quote', 'panel_src_quotes'),
  cities: listBlock('cities', 'city', 'panel_shown_cities', 'panel_add_city', 'panel_src_cities'),
  sensors: c => {
    if (!D.widgets.ha) { c.append(el('p', 'help lead', t('panel_no_ha'))); return; }
    listBlock('sensors', 'sensor', 'panel_shown_sensors', 'panel_add_sensor', 'panel_src_sensors')(c);
  },
  notes: c => notesBlock(c),
  check: c => {
    const ul = el('ul', 'rows');
    for (const key of ['check_openings', 'check_batteries']) {
      const it = item(key);
      if (!it) continue;
      const li = el('li');
      const l = el('label');
      const nm = el('span', 'nm');
      nm.append(el('b', 'plain', t('panel_' + key)));
      l.append(nm, control(it, D.values[key], v => save(it, v)));
      li.append(l);
      ul.append(li);
    }
    c.append(label(t('panel_check_what')), ul, help('panel_help_check'));
  },
  playing: c => {
    if (!D.widgets.ha) { c.append(el('p', 'help lead', t('panel_no_ha'))); return; }
    const pl = D.widgets.player;
    if (pl) {
      const ul = el('ul', 'rows');
      const li = el('li');
      const nm = el('div', 'nm');
      nm.append(el('b', null, pl.name), el('span', null, t('panel_player_chosen')));
      li.append(nm, ibtn('trash', t('panel_remove_x', { w: pl.name }), async () => {
        const a = await api('/api/player', { ref: '' });
        if (!took(a)) { toast(errorText(a.error), true); return; }
        render(true);
        toast(t('panel_removed', { w: pl.name }), a.undo ? { undo: undo } : {});
      }));
      ul.append(li);
      c.append(label(t('panel_which_player')), ul);
    } else c.append(finder('player', 'panel_choose_player', () => false, adder('player')));
    c.append(help('panel_help_playing'));
  },
  home: c => {
    if (!D.widgets.ha) { c.append(el('p', 'help lead', t('panel_no_ha'))); return; }
    const people = D.widgets.people || { all: [], chosen: [] };
    c.append(label(t('panel_who')), checkRows(people.all, people.chosen, async refs => {
      const a = await api('/api/people', { refs });
      if (took(a)) render(true); else toast(errorText(a.error), true);
    }, t('panel_who'), false), help('panel_people_hint'));
  },
};

// The board: a note per line, the pencil puts it back in the box, the bin removes it (Undo
// writes it again); 280 characters at most.
let noteEditing = null;

function notesBlock(c) {
  const data = D.notes;
  if (!data || !data.ok) { c.append(el('p', 'help lead', t('panel_no_brain'))); return; }
  const ul = el('ul', 'rows');
  if (data.notes.length === 0) ul.append(el('li', 'empty', t('panel_notes_empty')));
  if (noteEditing !== null && !data.notes.some(n => n.id === noteEditing)) noteEditing = null;
  const box = el('textarea');
  box.id = 'note-text';
  box.maxLength = 280;
  box.rows = 2;
  box.placeholder = t('panel_notes_placeholder');
  const go = button('ghost full', t(noteEditing === null ? 'panel_note_add' : 'panel_save'));
  const after = async () => {
    await new Promise(res => setTimeout(res, 700));
    D.notes = await api('/api/notes');
    took(await api('/api/widgets'));
    render(true);
  };
  for (const n of data.notes) {
    const li = el('li');
    const nm = el('div', 'nm');
    nm.append(el('b', 'plain', n.text));
    li.append(nm, ibtn('pencil', t('panel_edit'), () => {
      noteEditing = n.id; box.value = n.text;
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
    const r = noteEditing === null ? await api('/api/notes/add', { text }) : await api('/api/notes/edit', { id: noteEditing, text });
    go.disabled = false;
    if (!r.ok) { toast(errorText(r.error), true); return; }
    noteEditing = null;
    box.value = '';
    toast(t('panel_saved'));
    after();
  });
  c.append(label(t('panel_notes')), ul, label(t('panel_note_new'), box.id), box, go, help('panel_notes_hint'));
}

// ---- Shopping: the lists side by side; tick = bought, bin = removed (Undo adds it again), a
// box under each list to add.

function shopping() {
  const lists = (D.shopping && D.shopping.lists) || [];
  const h = el('h1', null, t('panel_tab_shopping'));
  if (!lists.length) return frag(h, help('panel_shopping_none'));
  const fresh = async () => {
    await new Promise(res => setTimeout(res, 1500));
    const r = await api('/api/shopping');
    if (r.ok) { D.shopping = r; if (route().p === 'spesa') render(true); }
  };
  const cols = el('div', 'cols c2');
  lists.forEach((l, n) => {
    const s = el('section');
    const ul = el('ul', 'rows');
    if (!l.items.length) ul.append(el('li', 'empty', t('panel_shopping_empty')));
    for (const it of l.items) {
      const row = el('li');
      const lb = el('label');
      const x = el('input');
      x.type = 'checkbox';
      x.setAttribute('aria-label', t('panel_bought_label', { w: it.text }));
      x.addEventListener('change', async () => {
        x.disabled = true;
        const r = await api('/api/shopping/done', { list: l.entity, uid: it.uid });
        if (r.ok) { D.shopping = r; toast(t('panel_bought', { w: short(it.text) })); render(true); }
        else { x.checked = false; x.disabled = false; toast(errorText(r.error), true); }
      });
      lb.append(x, el('span', null, it.text));
      row.append(lb, ibtn('trash', t('panel_remove_x', { w: it.text }), async () => {
        const r = await api('/api/shopping/remove', { list: l.entity, uid: it.uid });
        if (!r.ok) { toast(errorText(r.error), true); return; }
        D.shopping = r;
        render(true);
        toast(t('panel_removed', { w: short(it.text) }), {
          undo: async () => { const a = await api('/api/shopping/add', { list: l.entity, text: it.text }); if (a.ok) fresh(); else toast(errorText(a.error), true); },
        });
      }));
      ul.append(row);
    }
    const f = el('form', 'after');
    const i = el('input');
    i.type = 'text';
    i.id = 'add-' + n;
    i.maxLength = 200;
    i.required = true;
    i.enterKeyHint = 'done';
    i.placeholder = t('panel_add_to', { w: l.name });
    i.setAttribute('aria-label', t('panel_add_to', { w: l.name }));
    f.append(i);
    f.addEventListener('submit', async ev => {
      ev.preventDefault();
      const text = i.value.trim();
      if (!text) return;
      i.disabled = true;
      const r = await api('/api/shopping/add', { list: l.entity, text });
      i.disabled = false;
      if (!r.ok) { toast(errorText(r.error), true); return; }
      i.value = '';
      const shown = el('li', 'pending');
      shown.append(el('span', null, text));
      ul.querySelectorAll('li.empty').forEach(x => x.remove());
      ul.append(shown);
      i.focus();
      // Home Assistant sends the list again in a moment.
      fresh();
    });
    s.append(el('span', 'label first', l.name), ul, f);
    cols.append(s);
  });
  return frag(h, cols, help('panel_shopping_hint'));
}

// ---- Settings: Screen (with how often the widgets change), Voice, Lists and calendars; side
// by side on a desk, three pages on a phone.

function group(key) { return ((D.schema && D.schema.groups) || []).find(g => g.key === key); }

function settingsGroup(key, h) {
  const g = group(key);
  if (!g) return el('p', 'help', t('panel_error'));
  const items = g.items.slice();
  if (key === 'screen') { const e = item('widgets_every'); if (e) items.unshift(e); }
  const f = frag(el(h, h === 'h2' ? 'g' : null, g.label));
  const night = D.values.night_on === true;
  let pair = null;
  for (const it of items) {
    // The hours of the night only with the night on, side by side.
    if (it.key === 'night_start' || it.key === 'night_end') {
      if (!night) continue;
      if (!pair) { pair = el('div', 'two'); f.append(pair); }
      const l = el('label');
      l.append(el('span', 'label', it.label), control(it, D.values[it.key], v => save(it, v)));
      pair.append(l);
      continue;
    }
    f.append(setting(it));
  }
  return f;
}

function settingsAll() {
  const cols = el('div', 'cols');
  for (const k of GROUPS) {
    if (!group(k)) continue;
    const s = el('section');
    s.append(settingsGroup(k, 'h2'));
    cols.append(s);
  }
  return frag(el('h1', null, t('panel_tab_settings')), cols);
}

function settingsMenu() {
  const ul = el('ul', 'rows');
  for (const k of GROUPS) { const g = group(k); if (g) ul.append(navRow('impostazioni/' + k, g.label)); }
  return frag(el('h1', null, t('panel_tab_settings')), ul);
}

// ---- Access: the devices let in; revoke asks twice; log out of this one.

function when(sec) {
  try {
    return new Date(sec * 1000).toLocaleString(lang, { day: 'numeric', month: 'short', hour: '2-digit', minute: '2-digit' });
  } catch (e) {
    return '';
  }
}

function access() {
  const d = el('div', 'narrow');
  const ul = el('ul', 'rows');
  for (const s of (D.devices && D.devices.sessions) || []) {
    const li = el('li');
    const nm = el('div', 'nm');
    const line = t('panel_device_line', { browser: s.browser, os: s.os });
    nm.append(el('b', null, line), el('span', null, s.peer + ' · ' + t('panel_last_used', { w: when(s.last_used) })));
    li.append(nm);
    if (s.current) li.append(el('span', 'badge', t('panel_devices_this')));
    else {
      const b = button('ghost small', t('panel_remove'));
      b.setAttribute('aria-label', t('panel_revoke_x', { w: line }));
      let armed = 0;
      b.addEventListener('click', async () => {
        // Asks twice: a second touch within 3 s.
        if (!armed) {
          b.classList.add('armed');
          b.textContent = t('panel_revoke_sure');
          say(t('panel_revoke_sure_x', { w: line }));
          armed = setTimeout(() => { armed = 0; b.classList.remove('armed'); b.textContent = t('panel_remove'); }, 3000);
          return;
        }
        clearTimeout(armed);
        const r = await api('/api/sessions/revoke', { id: s.id });
        if (!r.ok) toast(errorText(r.error), true);
        const fresh = await api('/api/sessions');
        if (fresh.ok) { D.devices = fresh; render(true); }
      });
      li.append(b);
    }
    ul.append(li);
  }
  const rm = el('div', 'remove');
  rm.append(button('ghost', t('panel_logout'), async () => { await api('/api/logout', {}); login(); }));
  d.append(el('h1', null, t('panel_section_devices')), ul, help('panel_access_new_hint'), rm);
  return d;
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
  const was = inside;
  chrome(true);
  render(was);
}

async function start() {
  const s = await api('/api/strings');
  if (s.ok) { T = s.strings; lang = s.lang; document.documentElement.lang = lang; }
  const h = await api('/api/hello');
  if (!h.ok) {
    app.replaceChildren(el('p', 'help', t('panel_error', { name: '' })));
    return;
  }
  name = h.name;
  document.title = name;
  for (const k of Object.keys(T)) T[k] = T[k].split('{name}').join(name);
  const qr = new URLSearchParams(location.search).get('qr');
  if (!h.on) off();
  else if (h.session) main();
  else if (qr === 'bad_ticket') login(errorText(qr), t('panel_qr_old_title'));
  else login(qr ? errorText(qr) : '');
}

addEventListener('hashchange', () => { S.sorting = {}; render(); scrollTo(0, 0); });
wideQ.addEventListener('change', () => render(true));
// Back to the front: the data again.
document.addEventListener('visibilitychange', () => {
  if (document.visibilityState === 'visible' && inside && !document.querySelector('.dragging')) main();
});

start();

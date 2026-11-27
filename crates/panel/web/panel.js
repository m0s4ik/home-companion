// SPDX-License-Identifier: GPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Andrea Mattera
// The browser panel's page: drawn from what the body says (its words, the settings' schema,
// the widget's pages as the screen draws them, the shopping lists, the devices). Text is only
// ever set as text (textContent), never as HTML; icons are constant SVG paths.
//
// Panel v6: four sections. Home is the device: its screen drawn whole (1280 x 800, scaled) with
// the chosen widget in its place, the turn under it as a strip (drag, Alt + arrows, Before /
// After), the chosen widget's controls beside it. Screen: the same frame, live, with its state
// always written under it. Shopping. Settings, with the devices let in. On a desk a rail on the
// left; on a phone a tab bar at the bottom. The address says which page (#home[/<kind>],
// #schermo, #spesa, #impostazioni[/<group>]): the browser's back key works.
'use strict';

const app = document.getElementById('app');
const side = document.getElementById('side');
const tabs = document.getElementById('tabs');
const head = document.getElementById('top');
const NS = 'http://www.w3.org/2000/svg';
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
  bars: 'M5 9h14 M5 15h14', ok: 'M5 12l5 5 9-10', screen: 'M3 5h18v11H3z M9 20h6 M12 16v4',
  grid: 'M4 4h7v7H4z M13 4h7v4h-7z M13 10h7v10h-7z M4 13h7v7H4z', board: 'M4 4h7v7H4z M13 4h7v7h-7z M4 13h7v7H4z M13 13h7v7h-7z', close: 'M6 6l12 12 M18 6L6 18',
  custom: 'M4 7h7v10H4z M13 7h7v4h-7z M13 13h7v4h-7z', sliders: 'M4 7h10 M18 7h2 M4 17h2 M10 17h10 M16 9a2 2 0 100-4 2 2 0 000 4z M8 19a2 2 0 100-4 2 2 0 000 4z',
  house: 'M4 11l8-7 8 7 M6 9.5V20h12V9.5', stop: 'M7 7h10v10H7z', lock: 'M7 11h10v9H7z M9 11V8a3 3 0 016 0v3',
  sun: 'M12 8a4 4 0 100 8 4 4 0 000-8z M12 2v2 M12 20v2 M4.9 4.9l1.4 1.4 M17.7 17.7l1.4 1.4 M2 12h2 M20 12h2 M4.9 19.1l1.4-1.4 M17.7 6.3l1.4-1.4',
  cloud: 'M7 18h10a3.5 3.5 0 000-7 5 5 0 00-9.6-1A4 4 0 007 18z', rain: 'M7 15h10a3.5 3.5 0 000-7 5 5 0 00-9.6-1A4 4 0 007 15z M9 18l-1 2 M13 18l-1 2 M17 18l-1 2',
  snow: 'M7 15h10a3.5 3.5 0 000-7 5 5 0 00-9.6-1A4 4 0 007 15z M9 19h.1 M13 19h.1 M17 19h.1',
  moon: 'M19 14A7 7 0 0110 5a7 7 0 109 9z', partly: 'M7 20h10a3.5 3.5 0 000-7 5 5 0 00-9.6-1A4 4 0 007 20z M12 4v1.5 M6 6.5l1 1 M18 6.5l-1 1',
};
// The device's dock, filled (Material Design Icons, as the screen draws them).
const MDI = {
  dash: 'M13,3V9H21V3M13,21H21V11H13M3,21H11V15H3M3,13H11V3H3V13Z',
  mic: 'M12,2A3,3 0 0,1 15,5V11A3,3 0 0,1 12,14A3,3 0 0,1 9,11V5A3,3 0 0,1 12,2M19,11C19,14.53 16.39,17.44 13,17.93V21H11V17.93C7.61,17.44 5,14.53 5,11H7A5,5 0 0,0 12,16A5,5 0 0,0 17,11H19Z',
  more: 'M16,12A2,2 0 0,1 18,10A2,2 0 0,1 20,12A2,2 0 0,1 18,14A2,2 0 0,1 16,12M10,12A2,2 0 0,1 12,10A2,2 0 0,1 14,12A2,2 0 0,1 12,14A2,2 0 0,1 10,12M4,12A2,2 0 0,1 6,10A2,2 0 0,1 8,12A2,2 0 0,1 6,14A2,2 0 0,1 4,12Z',
};
// Home Assistant's conditions → an icon.
const SKY = { 'sunny': 'sun', 'clear-night': 'moon', 'partlycloudy': 'partly', 'cloudy': 'cloud', 'fog': 'cloud', 'windy': 'cloud',
  'windy-variant': 'cloud', 'rainy': 'rain', 'pouring': 'rain', 'lightning': 'rain', 'lightning-rainy': 'rain', 'hail': 'rain',
  'snowy': 'snow', 'snowy-rainy': 'snow', 'exceptional': 'cloud' };

function svg(d, cls) {
  const s = document.createElementNS(NS, 'svg');
  s.setAttribute('viewBox', '0 0 24 24');
  if (cls) s.setAttribute('class', cls);
  s.setAttribute('aria-hidden', 'true');
  const p = document.createElementNS(NS, 'path');
  p.setAttribute('d', d || '');
  s.append(p);
  return s;
}

function ico(key, cls) { return svg(IC[key], cls === undefined ? 'i' : cls); }

function ibtn(icon, label, fn, cls) {
  const b = el('button', cls || 'ibtn');
  b.type = 'button';
  b.setAttribute('aria-label', label);
  b.title = label;
  b.append(ico(icon));
  if (fn) b.addEventListener('click', fn);
  return b;
}

// A button: its class, its words, what it does; `icon` before the words.
function button(cls, text, fn, icon) {
  const b = el('button', cls);
  b.type = 'button';
  if (icon) b.append(ico(icon));
  b.append(document.createTextNode(text));
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
  if (o.undo) b.append(button('btn', t('panel_undo'), () => { b.remove(); o.undo(); }));
  document.body.append(b);
  toastTimer = setTimeout(() => b.remove(), o.undo ? 7000 : 2600);
}

// A name in a short line (a toast).
function short(s) { return s.length > 40 ? s.slice(0, 39) + '…' : s; }

// ---- Without access: ask for it, or follow the QR code's answer. A column in the middle:
// Alfred, a title, a line, one button.

function chrome(within) {
  inside = within;
  document.body.className = within ? 'in' : 'out';
  // Drawn again on the way in (the name, the words).
  side.replaceChildren(); tabs.replaceChildren(); head.replaceChildren();
}

function pairPage(pose, title, text, extra) {
  clearInterval(polling);
  // Nothing of the sections keeps running behind it (the Screen's poll would draw a login page
  // over the code page every few seconds).
  clearInterval(R.poll);
  R.poll = 0;
  screenStop(false);
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
    [button('btn primary big', t('panel_ask'), ask), el('p', 'fine', t('panel_access_new_hint'))]);
}

async function ask() {
  const r = await api('/api/pair', {});
  if (!r.ok) { login(errorText(r.error)); return; }
  // The screen shows the same code, and Allow to touch.
  pairPage('listening', t('panel_code_title'), t('panel_code_text'),
    [el('div', 'code', r.code), button('btn quiet', t('panel_cancel'), () => login())]);
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

const D = { schema: null, values: {}, widgets: null, shopping: null, notes: null, devices: null, samples: null, screen: null, board: null };
// What only this page remembers: a list being reordered, the window of rows each preview shows.
const S = { sorting: {}, win: {}, only: {} };

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
    if (busyHere()) { waitTries--; waitData(); return; }
    const r = await api('/api/widgets');
    if (took(r) && route().p === 'home') render(true);
  }, 3000);
}

// Never draw again under a finger or a cursor: it would take the field or the drag away.
function busyHere() {
  const a = document.activeElement;
  return !!document.querySelector('.dragging') || !!(a && ['INPUT', 'TEXTAREA', 'SELECT'].includes(a.tagName) && a.type !== 'checkbox');
}

// A page of the turn by its id ("quotes", "quotes-2": the kinds whose content is a list can be
// on the home more than once, each page with its own list).
function pages() { return (D.widgets && D.widgets.pages) || []; }
function page(id) { return pages().find(p => p.id === id); }
function listOf(p) { return ((D.widgets && D.widgets.lists) || {})[p.id] || []; }
function onPages() { return pages().filter(p => p.on); }

// ---- Controls from the schema: a switch, a choice (two or three: segments), a number, some
// of a list.

function control(it, value, onChange, opt) {
  const o = opt || {};
  if (it.kind === 'bool') {
    const i = el('input', 'sw');
    i.type = 'checkbox';
    i.setAttribute('role', 'switch');
    i.setAttribute('aria-label', it.label);
    i.checked = value === true;
    i.addEventListener('change', () => onChange(i.checked));
    return i;
  }
  if (it.kind === 'choice' && !o.select && it.options.length <= 3) {
    const g = el('div', 'seg');
    g.setAttribute('role', 'group');
    g.setAttribute('aria-label', it.label);
    for (const op of it.options) {
      const b = button(null, op.label, () => { if (String(op.value) !== String(value)) onChange(op.value); });
      b.setAttribute('aria-pressed', String(String(op.value) === String(value)));
      g.append(b);
    }
    return g;
  }
  if (it.kind === 'choice' || (it.kind === 'int' && it.max - it.min <= 24)) {
    const s = el('select');
    s.setAttribute('aria-label', o.label || it.label);
    const opts = it.kind === 'choice' ? it.options
      : Array.from({ length: (it.max - it.min) / it.step + 1 }, (_, k) => {
        const v = it.min + k * it.step;
        return { value: v, label: String(v).padStart(2, '0') + ':00' };
      });
    for (const op of opts) {
      const e = el('option', null, op.label);
      e.value = String(op.value);
      if (String(op.value) === String(value)) e.selected = true;
      s.append(e);
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
  return null;
}

// Some of a list (the shopping lists, the calendars): «All | Only these», and the choices under
// it. An empty value means all of them (new ones too), and so does every one ticked (the body
// says so): «Only these» opens the ticks here, and the first one taken away is saved. The last
// tick cannot be taken away (none ticked would be all again).
function multi(it, chosen, onChange) {
  const value = Array.isArray(chosen) ? chosen : [];
  const all = value.length === 0 && !S.only[it.key];
  const refs = it.options.map(o => String(o.value));
  const g = el('div', 'seg');
  g.setAttribute('role', 'group');
  g.setAttribute('aria-label', it.label);
  const a = button(null, t('panel_all'), () => {
    if (all) return;
    S.only[it.key] = false;
    if (value.length) onChange([]); else render(true);
  });
  const b = button(null, t('panel_only_these'), () => { if (all) { S.only[it.key] = true; render(true); } });
  a.setAttribute('aria-pressed', String(all));
  b.setAttribute('aria-pressed', String(!all));
  b.disabled = !refs.length;
  g.append(a, b);
  const ul = el('ul', 'group');
  ul.setAttribute('role', 'group');
  ul.setAttribute('aria-label', it.label);
  for (const o of it.options) {
    const li = el('li');
    const l = el('label', 'choice' + (all ? ' dim' : ''));
    const c = el('input', 'chk');
    c.type = 'checkbox';
    c.checked = all || !value.length || value.includes(String(o.value));
    c.disabled = all;
    c.addEventListener('change', () => {
      const now = Array.from(ul.querySelectorAll('input:checked')).map(x => x.dataset.v);
      if (!now.length) { c.checked = true; say(t('panel_keep_one')); toast(t('panel_keep_one')); return; }
      onChange(now);
    });
    c.dataset.v = String(o.value);
    l.append(c, el('span', null, o.label));
    li.append(l);
    ul.append(li);
  }
  if (!it.options.length) ul.append(el('li', 'row empty', t('panel_nothing_yet')));
  return { seg: g, list: ul };
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

// A row of a group: its words on the left (label, one line of help), its control on the right.
function row(label, helpText, ctl, cls) {
  const li = el('li', 'row' + (cls ? ' ' + cls : ''));
  const txt = el('div', 'txt');
  txt.append(el('b', null, label));
  if (helpText) txt.append(el('span', null, helpText));
  li.append(txt);
  if (ctl) li.append(ctl);
  return li;
}

// ---- The widget's card as the screen draws it: header (icon, title, the right side), then
// the rows of one window (curve rows, timed rows, plain rows) or the weather's own drawing.

function drawCard(kind, model, win, emptyText, label, p) {
  if (kind === 'custom') { const g = customCard(p); if (g) return g; }
  const c = el('div', 'wc');
  const hd = el('div', 'hd');
  const w = model && model.windows && model.windows.length ? model.windows[(win || 0) % model.windows.length] : null;
  hd.append(ico(kind), el('span', 't', model ? model.title : label || ''));
  if (w && w.aside) hd.append(el('span', 'a', w.aside));
  c.append(hd);
  if (model && model.layout === 'weather' && model.hero) return wrapCard(weatherCard(c, model.hero, w ? w.rows : []));
  if (!w || !w.rows.length) { c.append(el('div', 'empty', emptyText)); return wrapCard(c); }
  if (model.layout === 'hours') { drawWeather(c, w); return wrapCard(c); }
  const curves = kind === 'quotes' || kind === 'sensors';
  for (const r of w.rows) {
    const form = curves ? 'cv' : r.left ? 'tm' : 'pl';
    const line = el('div', 'rw ' + form);
    if (form === 'tm') line.append(el('span', 'l', r.left));
    line.append(el('span', null, r.text));
    if (form === 'cv') {
      line.append(r.curve ? curve(r.curve) : el('span', 'k'));
      line.append(el('span', 'v' + (r.wait ? ' wait' : ''), r.right || ''));
      const dir = kind === 'quotes' && r.trend ? (r.trend > 0 ? ' up' : ' dn') : '';
      line.append(el('span', 'c' + dir, r.change || ''));
    } else {
      if (form === 'tm') line.append(r.note ? ico(SKY[r.note] || 'cloud', 'w') : el('span'));
      line.append(el('span', 'v' + (r.wait ? ' wait' : ' dim'), r.right || ''));
    }
    c.append(line);
  }
  return wrapCard(c);
}

// The weather as the screen draws it with three slots: the sky big on the left, a dotted line,
// what the weather does next in words with the day's range small under it; the next hours small
// under it. No header and no big number: the date's line above has the temperature now.
function weatherCard(c, h, rows) {
  c.replaceChildren();
  c.classList.add('wx');
  const top = el('div', 'top');
  const sky = el('div', 'sky');
  sky.append(ico(SKY[h.kind] || 'cloud', ''));
  const right = el('div', 'main');
  right.append(el('div', 'say', h.label));
  if (h.range) right.append(el('div', 'rg', h.range));
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

// A custom widget as the device draws it: page `p`'s board tiles on 4 × 2 cells (none: no tiles).
function customCard(p) {
  const cv = p && canvases().find(x => x.page === p.id);
  if (!cv || !cv.tiles.length) return null;
  const g = el('div', 'bmini');
  for (const x of cv.tiles) {
    const tl = x.tile;
    const cell = el('div', 'x' + tl.x + ' y' + tl.y + ' w' + tl.w + ' h' + tl.h);
    cell.append(tileFace(x));
    g.append(cell);
  }
  const w = el('div', 'wp');
  w.append(g);
  return w;
}

// ---- The device's screen as it looks now: Alfred, the clock, the date (with the weather of
// the house when there is one), the widget `kind` in its slot, the turn's dots, the dock.

function hm() {
  try { return new Date().toLocaleTimeString(lang, { hour: '2-digit', minute: '2-digit', hourCycle: 'h23' }); } catch (e) { return ''; }
}

function today() {
  let s = '';
  try { s = new Date().toLocaleDateString(lang, { weekday: 'long', day: 'numeric', month: 'long' }); } catch (e) { /* none */ }
  return s.charAt(0).toUpperCase() + s.slice(1);
}

// `p` the page in the slot (none: an empty slot).
function deviceScreen(p, model, win) {
  const kind = p ? p.kind : null;
  const s = el('div', 'scr');
  s.setAttribute('aria-hidden', 'true');
  const date = el('div', 'date');
  date.append(el('span', null, today()));
  const wx = page('weather');
  const hero = wx && wx.model && !wx.model.sample && wx.model.hero;
  if (hero) {
    date.append(el('i'), ico(SKY[hero.kind] || 'cloud', ''));
    if (hero.temp) date.append(el('span', null, hero.temp));
  }
  const slot = el('div', 'slot');
  const on = onPages();
  const turns = on.length > 1 && !!p && on.some(q => q.id === p.id);
  if (kind) {
    const card = drawCard(kind, model, win, t('panel_empty_' + kind), p.label, p);
    // The same height for every page (the CSS of .slot, widgets::size); the form only places
    // its content: the weather's block, its columns, or its rows.
    const w = model && model.windows && model.windows.length ? model.windows[(win || 0) % model.windows.length] : null;
    const n = w ? w.rows.length : 0;
    const form = model && model.layout === 'weather' && model.hero ? 'wx' : model && model.layout === 'hours' && n ? 'hours'
      : 'n' + Math.min(4, Math.max(1, n || 2));
    card.firstChild.classList.add(form);
    slot.append(card);
  }
  slot.classList.toggle('turns', turns);
  if (turns) {
    const dots = el('div', 'dots');
    for (const q of on) dots.append(el('i', q.id === p.id ? 'on' : null));
    slot.append(dots);
  }
  const band = el('div', 'band');
  const dock = (key, word, cls) => {
    const d = el('div', cls);
    if (cls) { const c = el('span'); c.append(svg(MDI[key])); d.append(c); } else d.append(svg(MDI[key]));
    d.append(el('em', null, t(word)));
    return d;
  };
  band.append(dock('dash', 'ha_short'), dock('mic', 'talk', 'talk'), dock('more', 'more'));
  s.append(art('idle', 'alfred'), el('div', 'clock', hm()), date, slot, band);
  return s;
}

function deviceFrame(content, label) {
  const d = el('div', 'device');
  const g = el('div', 'glass');
  if (label) d.setAttribute('aria-label', label);
  g.append(content);
  d.append(g);
  return { device: d, glass: g };
}

setInterval(() => { for (const c of document.querySelectorAll('.scr .clock')) c.textContent = hm(); }, 10000);

// ---- The page drawn from the address.

const SECTIONS = [['home', 'panel_tab_home', 'grid'], ['plancia', 'panel_tab_board', 'board'], ['schermo', 'panel_tab_screen', 'screen'],
  ['spesa', 'panel_tab_shopping', 'shopping'], ['impostazioni', 'panel_tab_settings', 'sliders']];
const GROUPS = ['screen', 'voice', 'lists'];
// The addresses of the panel before v6.
const OLD = { widgets: 'home', shopping: 'spesa', settings: 'impostazioni', access: 'impostazioni/access',
  accessi: 'impostazioni/access', ordine: 'home', aggiungi: 'home', menu: 'home' };

function route() {
  let [p, k] = (location.hash.slice(1) || 'home').split('/');
  if (p === 'w') p = 'home';
  else if (OLD[p]) [p, k] = OLD[p].split('/');
  if (!SECTIONS.some(s => s[0] === p)) p = 'home';
  return { p, k: k ? decodeURIComponent(k) : '' };
}

function go(to) {
  if (location.hash === '#' + to) render(true); else location.hash = to;
}

function pagehead(title, sub) {
  const h = el('div', 'pagehead');
  h.append(el('h1', null, title));
  if (sub) h.append(el('span', 'sub', sub));
  return h;
}

function render(keep) {
  if (!inside) return;
  const y = scrollY;
  const focused = document.activeElement && document.activeElement.id;
  const r = route();
  let body;
  if (r.p === 'schermo') body = screenPage();
  else if (r.p === 'spesa') body = shopping();
  else if (r.p === 'impostazioni') body = settingsPage();
  else if (r.p === 'plancia') body = boardPage(r);
  else body = homePage(r);
  if (r.p !== 'schermo') screenLeave();
  drawNav(r);
  app.replaceChildren(body);
  if (keep) {
    scrollTo(0, y);
    const f = focused && document.getElementById(focused);
    if (f) f.focus();
  } else if (r.p === 'impostazioni' && r.k) {
    const g = document.getElementById('s-' + r.k);
    if (g) g.scrollIntoView();
  }
}

// The rail (a desk) and the tab bar (a phone): the same four sections; a red dot on Screen
// while this device controls it.
function drawNav(r) {
  const sec = (r || route()).p;
  // Drawn once; then only the current section and the red dot change (the focus stays).
  if (side.querySelector('.nav')) {
    for (const a of document.querySelectorAll('#side a[data-sec], #tabs a[data-sec]')) {
      if (a.dataset.sec === sec) a.setAttribute('aria-current', 'page'); else a.removeAttribute('aria-current');
      const dot = a.querySelector('.badge-live');
      if (a.dataset.sec === 'schermo' && R.live && !dot) a.append(liveBadge());
      else if (dot && !R.live) dot.remove();
    }
    return;
  }
  const who = el('div', 'who');
  const face = art('face');
  face.width = 40; face.height = 40;
  const nm = el('div');
  const st = el('span');
  st.append(el('i', 'dot'), document.createTextNode(t('panel_status_on')));
  nm.append(el('b', null, name), st);
  who.append(face, nm);
  const links = () => {
    const box = el('div', 'nav');
    for (const [k, word, icon] of SECTIONS) {
      const a = link(null, k);
      a.dataset.sec = k;
      a.append(ico(icon), el('span', null, t(word)));
      if (k === sec) a.setAttribute('aria-current', 'page');
      if (k === 'schermo' && R.live) a.append(liveBadge());
      box.append(a);
    }
    return box;
  };
  side.setAttribute('aria-label', t('panel_tabs'));
  tabs.setAttribute('aria-label', t('panel_tabs'));
  side.replaceChildren(who, links());
  tabs.replaceChildren(...links().childNodes);
  const f = art('face');
  f.width = 28; f.height = 28;
  const s2 = el('span', 'st');
  s2.append(el('i', 'dot'), document.createTextNode(t('panel_status_on')));
  head.replaceChildren(f, el('b', null, name), s2);
}

function liveBadge() {
  const b = el('i', 'badge-live');
  b.title = t('panel_screen_state_live');
  return b;
}

// ---- Home: the device, the turn under it, the chosen widget beside it.

let samplesAsked = false;
async function samples() {
  if (D.samples || samplesAsked) return;
  samplesAsked = true;
  const r = await api('/api/widgets/samples');
  D.samples = r.ok ? r.samples : [];
  if (route().p === 'home') render(true);
}

// What a page shows in the preview: its own data, or (not on the home and nothing of its own)
// the example of its kind.
function shownModel(p) {
  const m = p.model;
  if (p.on) return { model: m, example: !!(m && m.sample) };
  const real = m && !m.sample && m.windows.some(w => w.rows.length);
  if (real) return { model: m, example: false };
  if (!D.samples) { samples(); return { model: m, example: true }; }
  return { model: (D.samples.find(x => x.kind === p.kind) || {}).model || m, example: true };
}

function homePage(r) {
  if (!D.widgets || !D.widgets.ok) return frag(pagehead(t('panel_tab_home')), el('p', 'lead', t('panel_error')));
  const on = onPages();
  const showing = D.widgets.showing && on.some(p => p.id === D.widgets.showing) ? D.widgets.showing : null;
  const k = page(r.k) ? r.k : (showing || (on[0] || pages()[0] || {}).id);
  const p = page(k);
  if (!p) return frag(pagehead(t('panel_tab_home')), el('p', 'lead', t('panel_error')));
  const { model, example } = shownModel(p);
  // A custom widget shows all its tiles at once.
  const n = model && model.windows && p.kind !== 'custom' ? model.windows.length : 1;
  const win = Math.min(S.win[k] || 0, Math.max(0, n - 1));

  const stage = el('div', 'stagecol');
  // The device in a box as large as the page leaves it (the turn under it stays in view).
  const fit = el('div', 'devfit');
  fit.append(deviceFrame(deviceScreen(p, model, win), t('panel_device_preview')).device);
  stage.append(fit);
  const only = el('div', 'cardonly');
  only.append(drawCard(p.kind, model, win, t('panel_empty_' + p.kind), p.label, p));
  stage.append(only);
  const note = el('p', 'preview-note');
  if (!p.on) note.append(el('span', 'tag', t('panel_not_on_home')), el('span', 'grow', t('panel_would_look')));
  else note.append(el('span', 'grow', t(example ? 'panel_preview_sample' : 'panel_preview_now')));
  if (n > 1) {
    note.append(ibtn('left', t('panel_rows_prev'), () => { S.win[k] = (win + n - 1) % n; render(true); }),
      el('span', 'num', (win + 1) + '/' + n),
      ibtn('right', t('panel_rows_next'), () => { S.win[k] = (win + 1) % n; render(true); }));
  }
  stage.append(note);

  // The turn: a numbered tile for each widget on the home, in its order.
  const th = el('div', 'turnhead');
  th.append(el('h2', null, t('panel_rotation')), el('span', 'grow'));
  const every = item('widgets_every');
  if (every) {
    const l = el('label', null, t('panel_every'));
    const c = control(every, D.values.widgets_every, v => save(every, v), { select: true, label: t('panel_every') });
    c.id = 'every';
    l.append(c);
    th.append(l);
  }
  const strip = el('div', 'strip');
  strip.setAttribute('role', 'group');
  strip.setAttribute('aria-label', t('panel_rotation'));
  on.forEach((q, i) => {
    const b = el('button', 'tile');
    b.type = 'button';
    b.id = 'tile-' + q.id;
    b.dataset.id = q.id;
    b.setAttribute('aria-pressed', String(q.id === k));
    b.setAttribute('aria-label', t('panel_pos_of', { w: q.label, a: i + 1, n: on.length }));
    b.title = t('panel_move_hint');
    b.append(el('span', 'n', i + 1));
    if (q.id === showing) { const d = el('i', 'live'); d.title = t('panel_showing_now'); b.append(d); }
    b.append(ico(q.kind), el('b', null, q.label));
    b.addEventListener('click', () => { if (!b.dataset.dragged) go('home/' + q.id); });
    strip.append(b);
  });
  if (!on.length) strip.append(el('p', 'none', t('panel_no_widgets')));
  stripSort(strip, ids => saveOrder(ids));
  const offs = pages().filter(q => !q.on);
  // A kind whose content is a list is always offered once more (another page with its own
  // list), unless one of its pages is already offered off.
  const more = (D.widgets.more || []).filter(m => !offs.some(q => q.kind === m.kind));
  const avail = el('div', 'avail');
  avail.append(el('span', null, offs.length || more.length ? t('panel_can_add') : t('panel_all_on')));
  const offer = (kd, label, pressed, fn) => {
    const b = el('button', 'tile off');
    b.type = 'button';
    b.setAttribute('aria-pressed', String(pressed));
    const plus = el('span', 'plus');
    plus.append(ico('plus'));
    b.append(ico(kd), el('b', null, label), plus);
    b.addEventListener('click', fn);
    avail.append(b);
    return b;
  };
  for (const q of offs) offer(q.kind, q.label, q.id === k, () => go('home/' + q.id));
  for (const m of more) {
    const b = offer(m.kind, m.label, false, () => addAnother(m.kind, b));
    b.id = 'more-' + m.kind;
    b.setAttribute('aria-label', t('panel_add_another', { w: m.label }));
  }
  stage.append(th, strip, avail);

  const ed = el('aside', 'editor');
  ed.setAttribute('aria-label', p.label);
  const eh = el('div', 'edhead');
  const ic = el('span', 'ic');
  ic.append(ico(p.kind));
  const words = el('div', 'grow');
  const pos = on.findIndex(q => q.id === k) + 1;
  words.append(el('h2', null, p.label), el('p', null, p.on ? t('panel_pos_in', { a: pos, n: on.length }) : t('panel_what_' + p.kind)));
  eh.append(ic, words);
  if (p.on) {
    const l = el('label', null, t('panel_on_home'));
    const s = el('input', 'sw');
    s.type = 'checkbox';
    s.checked = true;
    s.setAttribute('role', 'switch');
    s.addEventListener('change', () => { s.disabled = true; removeFromHome(k); });
    l.append(s);
    eh.append(l);
  }
  ed.append(eh);
  if (!p.on) {
    const box = el('div', 'edoff');
    box.append(el('p', null, t('panel_off_home_line')),
      button('btn primary big', t('panel_add_home'), ev => addToHome(k, ev.currentTarget), 'plus'));
    ed.append(box);
  }
  const ctl = el('div', 'ctl');
  (EDIT[p.kind] || (() => {}))(ctl, p);
  if (REPEATS.includes(p.kind)) ctl.append(pageBlock(p));
  ed.append(ctl);
  if (p.on && on.length > 1) {
    const f = el('div', 'edfoot');
    const ids = on.map(q => q.id);
    const move = d => { const i = ids.indexOf(k); [ids[i], ids[i + d]] = [ids[i + d], ids[i]]; saveOrder(ids, true); };
    const before = button('btn quiet', t('panel_before'), () => move(-1), 'left');
    const after = el('button', 'btn quiet');
    after.type = 'button';
    after.append(document.createTextNode(t('panel_after')), ico('right'));
    after.addEventListener('click', () => move(1));
    before.id = 'mv-before'; after.id = 'mv-after';
    before.disabled = pos === 1;
    after.disabled = pos === on.length;
    f.append(el('span', 'grow fine', t('panel_position')), before, after);
    ed.append(f);
  }
  const home = el('div', 'home');
  home.append(stage, ed);
  return frag(pagehead(t('panel_tab_home'), t('panel_home_sub')), home);
}

// The green dot: the page on the screen now, asked every 5 s while Home is in front.
setInterval(async () => {
  if (!inside || document.hidden || route().p !== 'home' || !D.widgets || document.querySelector('.dragging')) return;
  const r = await api('/api/widgets/showing');
  if (!r.ok || r.showing === D.widgets.showing) return;
  D.widgets.showing = r.showing;
  for (const b of document.querySelectorAll('.strip .tile')) {
    const dot = b.querySelector('.live');
    if (b.dataset.id === r.showing && !dot) {
      const d = el('i', 'live');
      d.title = t('panel_showing_now');
      b.querySelector('.n').after(d);
    } else if (b.dataset.id !== r.showing && dot) dot.remove();
  }
}, 5000);

// The turn's new order (the widgets off keep their place at the end).
async function saveOrder(ids, quiet) {
  const all = ids.concat(pages().filter(p => !ids.includes(p.id)).map(p => p.id));
  const r = await api('/api/widgets/order', { order: all });
  if (took(r)) toast(t('panel_order_saved'));
  else toast(errorText(r.error), true);
  render(true);
  if (quiet) say(t('panel_order_saved'));
}

// On: at the end of the turn. The same page then shows its controls.
async function addToHome(id, b) {
  b.disabled = true;
  const on = onPages().map(x => x.id).concat(id);
  const all = on.concat(pages().filter(x => !x.on && x.id !== id).map(x => x.id));
  const r1 = await api('/api/widgets/order', { order: all });
  const r2 = r1.ok ? await api('/api/set', { key: 'widgets', value: on }) : r1;
  took(r1); took(r2);
  if (!r2.ok) { b.disabled = false; toast(errorText(r2.error), true); return; }
  toast(t('panel_now_on', { w: page(id).label }));
  go('home/' + id);
}

// Another page of a kind whose content is a list: on, at the end of the turn, its list empty;
// the editor opens on it to choose what it shows.
async function addAnother(kind, b) {
  b.disabled = true;
  const r = await api('/api/widgets/add', { kind });
  if (!took(r)) { b.disabled = false; toast(errorText(r.error), true); return; }
  const p = page(r.added);
  toast(t('panel_now_on', { w: p ? p.label : kind }));
  go('home/' + r.added);
}

// Off: the page stays (now as a preview); Undo puts it back where it was.
async function removeFromHome(id) {
  const was = pages().map(x => x.id);
  const wasOn = onPages().map(x => x.id);
  const label = page(id).label;
  const r = await api('/api/set', { key: 'widgets', value: wasOn.filter(k => k !== id) });
  if (!took(r)) { toast(errorText(r.error), true); render(true); return; }
  go('home/' + id);
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

// ---- Reordering the turn's tiles: drag (a mouse at once, a finger after holding it still a
// moment, so that the strip still scrolls), Alt + arrows on a focused tile. While dragging, the
// tile goes before the tile under the pointer if the pointer is on its first half, else after.

function stripSort(box, done) {
  const items = () => Array.from(box.children).filter(x => x.dataset.id);
  for (const it of items()) {
    it.addEventListener('contextmenu', e => { if (e.pointerType !== 'mouse') e.preventDefault(); });
    it.addEventListener('keydown', ev => {
      if (!ev.altKey) return;
      const d = { ArrowLeft: -1, ArrowUp: -1, ArrowRight: 1, ArrowDown: 1 }[ev.key];
      if (!d) return;
      ev.preventDefault();
      const ids = items().map(o => o.dataset.id);
      const from = ids.indexOf(it.dataset.id);
      const to = from + d;
      if (to < 0 || to >= ids.length) return;
      [ids[from], ids[to]] = [ids[to], ids[from]];
      say(t('panel_moved', { a: to + 1, n: ids.length }));
      done(ids);
      // The page is drawn again: the moved tile keeps the focus.
      setTimeout(() => { const f = document.getElementById(it.id); if (f) f.focus(); }, 0);
    });
    it.addEventListener('pointerdown', ev => {
      if (ev.button !== 0) return;
      const id = ev.pointerId;
      const touch = ev.pointerType === 'touch';
      const x0 = ev.clientX, y0 = ev.clientY;
      let x = x0, y = y0, dragging = false, scrolling = 0;
      const before = items().map(o => o.dataset.id).join();
      delete it.dataset.dragged;
      const start = () => {
        dragging = true;
        // (iOS: no scrolling may take the finger from here on)
        it.classList.add('dragging', 'held');
        box.classList.add('dragging');
        it.dataset.dragged = '1';
        try { it.setPointerCapture(id); } catch (e) { /* gone */ }
        if (touch && navigator.vibrate) navigator.vibrate(10);
      };
      const hold = touch ? setTimeout(start, 350) : 0;
      const place = () => {
        for (const o of items()) {
          if (o === it) continue;
          const r = o.getBoundingClientRect();
          if (x < r.left || x > r.right || y < r.top || y > r.bottom) continue;
          const target = x > r.left + r.width / 2 ? o.nextElementSibling : o;
          if (target !== it && target !== it.nextElementSibling) box.insertBefore(it, target);
          return;
        }
      };
      // A strip wider than the page (a phone) scrolls by itself near its edges.
      const edge = () => {
        const r = box.getBoundingClientRect();
        const v = x < r.left + 40 ? -10 : x > r.right - 40 ? 10 : 0;
        if (v && box.scrollWidth > box.clientWidth) { box.scrollLeft += v; place(); }
        scrolling = v ? requestAnimationFrame(edge) : 0;
      };
      const stopTouch = e => { if (dragging) e.preventDefault(); };
      const move = e => {
        if (e.pointerId !== id) return;
        x = e.clientX; y = e.clientY;
        if (!dragging) {
          const far = Math.hypot(x - x0, y - y0);
          if (touch) { if (far > 8) end(); return; }
          if (far < 6) return;
          start();
        }
        e.preventDefault();
        place();
        if (!scrolling) scrolling = requestAnimationFrame(edge);
      };
      const end = () => {
        clearTimeout(hold);
        removeEventListener('pointermove', move);
        removeEventListener('pointerup', up);
        removeEventListener('pointercancel', cancel);
        removeEventListener('touchmove', stopTouch);
        cancelAnimationFrame(scrolling);
        it.classList.remove('dragging', 'held');
        box.classList.remove('dragging');
      };
      const up = e => {
        if (e.pointerId !== id) return;
        end();
        if (!dragging) return;
        // The click that follows a drag is not a choice.
        setTimeout(() => { delete it.dataset.dragged; }, 0);
        const ids = items().map(o => o.dataset.id);
        if (ids.join() !== before) done(ids);
      };
      // Taken away by the browser (a scroll, a call): nothing saved, the order as it was.
      const cancel = e => {
        if (e.pointerId !== id) return;
        end();
        setTimeout(() => { delete it.dataset.dragged; }, 0);
        if (dragging && items().map(o => o.dataset.id).join() !== before) render(true);
      };
      addEventListener('pointermove', move, { passive: false });
      addEventListener('pointerup', up);
      addEventListener('pointercancel', cancel);
      addEventListener('touchmove', stopTouch, { passive: false });
    });
  }
}

// The same, up and down, by a handle (the rows of a widget's list in «Reorder»).
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
      const id = ev.pointerId;
      const move = e => {
        if (e.pointerId !== id) return;
        e.preventDefault();
        y = e.clientY;
        for (const o of items()) {
          if (o === it) continue;
          const r = o.getBoundingClientRect();
          if (y < r.top || y > r.bottom) continue;
          const target = y > r.top + r.height / 2 ? o.nextElementSibling : o;
          if (target !== it && target !== it.nextElementSibling) box.insertBefore(it, target);
          break;
        }
      };
      const up = e => {
        if (e.pointerId !== id) return;
        removeEventListener('pointermove', move);
        removeEventListener('pointerup', up);
        removeEventListener('pointercancel', up);
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
      const to = a.indexOf(it) + d;
      if (to < 0 || to >= a.length) return;
      box.insertBefore(it, d < 0 ? a[to] : a[to].nextSibling);
      say(t('panel_moved', { a: to + 1, n: a.length }));
      done(items().map(o => o.dataset.id), it.dataset.id, true);
    });
  }
}

function refocus(id) {
  const it = Array.from(document.querySelectorAll('[data-id]')).find(x => x.dataset.id === id);
  const g = it && it.querySelector('.grip');
  if (g) g.focus();
}

// ---- The controls of each kind: a label over each block, one line of help.

function label(text, forId) {
  const l = el(forId ? 'label' : 'span', 'lab', text);
  if (forId) l.htmlFor = forId;
  return l;
}

function help(key) { return el('p', 'fine', t(key)); }

// The lists or the calendars shown: the same setting as in Settings.
function multiBlock(c, key, extra) {
  const it = item(key);
  if (!it) return;
  const m = multi(it, D.values[key], v => save(it, v));
  c.append(label(it.label), m.seg, m.list);
  m.list.classList.add('after-seg');
  if (extra) c.append(extra);
}

// A list of chosen things: name and detail, its value now (from the preview), bin; in «Reorder»
// a handle instead of the bin.
function chosen(p) {
  const kind = p.kind;
  const items = listOf(p);
  const sorting = !!S.sorting[p.id];
  const ul = el('ul', 'group');
  if (!items.length) ul.append(el('li', 'row empty', t('panel_' + kind + '_empty')));
  const rows = ((p.model && p.model.windows) || []).flatMap(w => w.rows);
  for (const it of items) {
    const li = el('li', 'row');
    li.dataset.id = it.ref;
    const nm = el('div', 'txt');
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
    if (sorting) li.append(ibtn('bars', t('panel_move', { w: it.name }), null, 'ibtn grip'));
    else {
      li.append(ibtn('trash', t('panel_remove_x', { w: it.name }), async () => {
        const a = await api('/api/' + kind + '/remove', { ref: it.ref, page: p.id });
        if (!took(a)) { toast(errorText(a.error), true); return; }
        render(true);
        toast(t('panel_removed', { w: short(it.name) }), a.undo ? { undo: undo } : {});
      }));
    }
    ul.append(li);
  }
  if (sorting) {
    sortable(ul, async (ids, id, keys) => {
      const a = await api('/api/' + kind + '/move', { ref: id, to: ids.indexOf(id), page: p.id });
      if (!took(a)) toast(errorText(a.error), true);
      render(true);
      if (keys) refocus(id);
    });
  }
  return ul;
}

// «Reorder» for a widget's list: a quiet button next to its label.
function chosenHead(p, key) {
  const h = el('div', 'turnhead');
  h.append(el('span', 'lab grow', t(key)));
  if (listOf(p).length > 1) {
    const sorting = !!S.sorting[p.id];
    h.append(button('btn quiet', t(sorting ? 'panel_finish' : 'panel_reorder'), () => { S.sorting[p.id] = !sorting; render(true); }));
  }
  return h;
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
  i.setAttribute('aria-label', t(labelKey));
  const st = finds[what] || (finds[what] = { q: '', hits: null, id: '', last: 0, timer: 0, seq: 0 });
  i.value = st.q;
  f.append(ico('search'), i);
  const out = el('ul', 'group hits');
  out.setAttribute('aria-live', 'polite');
  const show = () => {
    out.replaceChildren();
    out.hidden = !st.q;
    if (!st.q) return;
    if (st.hits === null) {
      const li = el('li', 'row empty');
      li.append(art('thinking', 'think'), el('span', null, t('panel_searching')));
      out.append(li);
      return;
    }
    if (typeof st.hits === 'string') { out.append(el('li', 'row empty', st.hits)); return; }
    if (!st.hits.length) { out.append(el('li', 'row empty', t('panel_search_none'))); return; }
    for (const h of st.hits) {
      const li = el('li', 'row');
      const nm = el('div', 'txt');
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
  return frag(f, out);
}

// Adds a hit to a list; the answer's pages show it in the preview at once.
// `p` the page whose list it is (none: the player).
function adder(kind, p) {
  return async (h, search) => {
    const id = p ? p.id : '';
    const a = await api(kind === 'player' ? '/api/player' : '/api/' + kind + '/add', { search, ref: h.ref, page: id });
    if (!took(a)) return a;
    if (kind !== 'player') {
      const n = (a.lists && a.lists[id] || []).length;
      S.win[id] = Math.floor(Math.max(0, n - 1) / 4);
      toast(t('panel_added', { w: short(h.name) }), {
        undo: async () => { const r = await api('/api/' + kind + '/remove', { ref: h.ref, page: id }); if (took(r)) render(true); },
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
  return (c, p) => {
    const items = listOf(p);
    c.append(chosenHead(p, shownKey), chosen(p));
    if (items.length < D.widgets.limits[kind]) {
      c.append(finder(what, addKey, h => items.some(x => x.ref === h.ref), adder(kind, p)));
    } else c.append(help('panel_list_full'));
    if (kind === 'quotes') {
      const it = item('widgets_currency');
      if (it) {
        const s = control(it, D.values.widgets_currency, v => save(it, v), { select: true });
        s.id = 'set-widgets_currency';
        c.append(label(it.label, s.id), s);
      }
    }
    c.append(help(helpKey));
    if (kind === 'quotes' || kind === 'cities') c.append(el('p', 'credit', t('panel_feeds_credit')));
  };
}

// The kinds whose content is a list: a page of them has a name and, from the second on, can
// be removed.
const REPEATS = ['quotes', 'cities', 'sensors'];

// The page's own name (empty: «Markets 2») and, for a second page, removing it with its list.
function pageBlock(p) {
  const box = el('div', 'pageblock');
  const id = 'name-' + p.id;
  const i = el('input');
  i.type = 'text';
  i.id = id;
  i.maxLength = (D.widgets.limits && D.widgets.limits.name) || 24;
  i.value = p.name || '';
  i.placeholder = p.label;
  i.autocomplete = 'off';
  const saveName = async () => {
    const name = i.value.trim();
    if (name === (p.name || '')) return;
    const a = await api('/api/widgets/name', { page: p.id, name });
    if (!took(a)) { toast(errorText(a.error), true); return; }
    toast(t('panel_saved'));
    render(true);
  };
  i.addEventListener('change', saveName);
  i.addEventListener('keydown', e => { if (e.key === 'Enter') { e.preventDefault(); i.blur(); } });
  box.append(label(t('panel_page_name'), id), i, help('panel_page_name_help'));
  if (p.n > 1) {
    const rm = button('btn danger', t('panel_remove_page', { w: p.label }), async ev => {
      ev.currentTarget.disabled = true;
      const a = await api('/api/widgets/remove', { page: p.id });
      if (!took(a)) { toast(errorText(a.error), true); render(true); return; }
      go('home');
      toast(t('panel_page_removed', { w: p.label }), a.undo ? { undo: async () => { await undo(); go('home/' + p.id); } } : {});
    }, 'trash');
    rm.id = 'remove-' + p.id;
    box.append(rm);
  }
  return box;
}

const EDIT = {
  // A custom widget: its tiles are the board's.
  custom: (c, p) => {
    const cv = canvases().find(x => x.page === p.id);
    const l = el('p', 'lead');
    l.append(document.createTextNode(t('panel_board_edit_in') + ' '), link('inl', 'plancia' + (cv ? '/' + cv.id : ''), t('panel_tab_board')), document.createTextNode('.'));
    c.append(l);
  },
  next: c => multiBlock(c, 'calendars'),
  shopping: c => {
    const more = el('p', 'fine');
    more.append(document.createTextNode(t('panel_lists_edit_in') + ' '), link('inl', 'spesa', t('panel_tab_shopping')), document.createTextNode('.'));
    multiBlock(c, 'lists', more);
  },
  weather: c => c.append(el('p', 'lead', t('panel_help_weather'))),
  quotes: listBlock('quotes', 'quote', 'panel_shown_quotes', 'panel_add_quote', 'panel_src_quotes'),
  cities: listBlock('cities', 'city', 'panel_shown_cities', 'panel_add_city', 'panel_src_cities'),
  sensors: (c, p) => {
    if (!D.widgets.ha) { c.append(el('p', 'lead', t('panel_no_ha'))); return; }
    listBlock('sensors', 'sensor', 'panel_shown_sensors', 'panel_add_sensor', 'panel_src_sensors')(c, p);
  },
  notes: c => notesBlock(c),
  check: c => {
    const ul = el('ul', 'group');
    for (const key of ['check_openings', 'check_batteries']) {
      const it = item(key);
      if (!it) continue;
      const li = row(t('panel_' + key), null, control(it, D.values[key], v => save(it, v)));
      ul.append(li);
    }
    c.append(label(t('panel_check_what')), ul, help('panel_help_check'));
  },
  playing: c => {
    if (!D.widgets.ha) { c.append(el('p', 'lead', t('panel_no_ha'))); return; }
    const pl = D.widgets.player;
    if (pl) {
      const ul = el('ul', 'group');
      ul.append(row(pl.name, t('panel_player_chosen'), ibtn('trash', t('panel_remove_x', { w: pl.name }), async () => {
        const a = await api('/api/player', { ref: '' });
        if (!took(a)) { toast(errorText(a.error), true); return; }
        render(true);
        toast(t('panel_removed', { w: pl.name }), a.undo ? { undo: undo } : {});
      })));
      c.append(label(t('panel_which_player')), ul);
    } else c.append(label(t('panel_choose_player'), 'find-player'), finder('player', 'panel_choose_player', () => false, adder('player')));
    c.append(help('panel_help_playing'));
  },
  home: c => {
    if (!D.widgets.ha) { c.append(el('p', 'lead', t('panel_no_ha'))); return; }
    const people = D.widgets.people || { all: [], chosen: [] };
    const ul = el('ul', 'group');
    ul.setAttribute('role', 'group');
    ul.setAttribute('aria-label', t('panel_who'));
    for (const o of people.all) {
      const li = el('li');
      const l = el('label', 'choice');
      const x = el('input', 'chk');
      x.type = 'checkbox';
      x.value = o.ref;
      x.checked = (people.chosen || []).includes(o.ref);
      x.addEventListener('change', async () => {
        const refs = Array.from(ul.querySelectorAll('input:checked')).map(y => y.value);
        const a = await api('/api/people', { refs });
        if (took(a)) render(true); else toast(errorText(a.error), true);
      });
      l.append(x, el('span', null, o.name));
      li.append(l);
      ul.append(li);
    }
    if (!people.all.length) ul.append(el('li', 'row empty', t('panel_nothing_yet')));
    c.append(label(t('panel_who')), ul, help('panel_people_hint'));
  },
};

// The board: a note per line, the pencil puts it back in the box, the bin removes it (Undo
// writes it again); 280 characters at most.
let noteEditing = null;

function notesBlock(c) {
  const data = D.notes;
  if (!data || !data.ok) { c.append(el('p', 'lead', t('panel_no_brain'))); return; }
  const ul = el('ul', 'group');
  if (data.notes.length === 0) ul.append(el('li', 'row empty', t('panel_notes_empty')));
  if (noteEditing !== null && !data.notes.some(n => n.id === noteEditing)) noteEditing = null;
  const box = el('textarea');
  box.id = 'note-text';
  box.maxLength = 280;
  box.rows = 2;
  box.placeholder = t('panel_notes_placeholder');
  const send = button('btn', t(noteEditing === null ? 'panel_note_add' : 'panel_save'), null, 'plus');
  const after = async () => {
    await new Promise(res => setTimeout(res, 700));
    D.notes = await api('/api/notes');
    took(await api('/api/widgets'));
    render(true);
  };
  for (const n of data.notes) {
    const li = row(n.text, null, ibtn('pencil', t('panel_edit'), () => {
      noteEditing = n.id; box.value = n.text;
      send.lastChild.textContent = t('panel_save'); box.focus();
    }));
    li.append(ibtn('trash', t('panel_remove_x', { w: n.text.slice(0, 40) }), async () => {
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
  send.addEventListener('click', async () => {
    const text = box.value.trim();
    if (!text) { box.focus(); return; }
    send.disabled = true;
    const r = noteEditing === null ? await api('/api/notes/add', { text }) : await api('/api/notes/edit', { id: noteEditing, text });
    send.disabled = false;
    if (!r.ok) { toast(errorText(r.error), true); return; }
    noteEditing = null;
    box.value = '';
    toast(t('panel_saved'));
    after();
  });
  const nb = el('div', 'notebox');
  nb.append(box, send);
  c.append(label(t('panel_notes')), ul, label(t('panel_note_new'), box.id), nb, help('panel_notes_hint'));
}

// ---- Shopping: the lists side by side; tick = bought, bin = removed (Undo adds it again), a
// line under each list to add.

function shopping() {
  const lists = (D.shopping && D.shopping.lists) || [];
  const h = pagehead(t('panel_tab_shopping'), t('panel_shopping_sub'));
  if (!lists.length) return frag(h, el('p', 'lead', t('panel_shopping_none')));
  const fresh = async () => {
    await new Promise(res => setTimeout(res, 1500));
    const r = await api('/api/shopping');
    if (r.ok) { D.shopping = r; if (route().p === 'spesa' && !busyHere()) render(true); }
  };
  const box = el('div', 'lists');
  lists.forEach((l, n) => {
    const s = el('section');
    const ul = el('ul', 'group');
    if (!l.items.length) ul.append(el('li', 'row empty', t('panel_shopping_empty')));
    for (const it of l.items) {
      const li = el('li', 'item');
      const lb = el('label');
      const x = el('input', 'chk');
      x.type = 'checkbox';
      x.setAttribute('aria-label', t('panel_bought_label', { w: it.text }));
      x.addEventListener('change', async () => {
        x.disabled = true;
        const r = await api('/api/shopping/done', { list: l.entity, uid: it.uid });
        if (r.ok) { D.shopping = r; toast(t('panel_bought', { w: short(it.text) })); render(true); }
        else { x.checked = false; x.disabled = false; toast(errorText(r.error), true); }
      });
      lb.append(x, el('span', null, it.text));
      li.append(lb, ibtn('trash', t('panel_remove_x', { w: it.text }), async () => {
        const r = await api('/api/shopping/remove', { list: l.entity, uid: it.uid });
        if (!r.ok) { toast(errorText(r.error), true); return; }
        D.shopping = r;
        render(true);
        toast(t('panel_removed', { w: short(it.text) }), {
          undo: async () => { const a = await api('/api/shopping/add', { list: l.entity, text: it.text }); if (a.ok) fresh(); else toast(errorText(a.error), true); },
        });
      }));
      ul.append(li);
    }
    const add = el('li', 'addrow');
    const f = el('form');
    const i = el('input');
    i.type = 'text';
    i.id = 'add-' + n;
    i.maxLength = 200;
    i.required = true;
    i.enterKeyHint = 'done';
    i.placeholder = t('panel_add_to', { w: l.name });
    i.setAttribute('aria-label', t('panel_add_to', { w: l.name }));
    f.append(i);
    add.append(ico('plus'), f);
    ul.append(add);
    f.addEventListener('submit', async ev => {
      ev.preventDefault();
      const text = i.value.trim();
      if (!text) return;
      i.disabled = true;
      const r = await api('/api/shopping/add', { list: l.entity, text });
      i.disabled = false;
      if (!r.ok) { toast(errorText(r.error), true); return; }
      i.value = '';
      const shown = el('li', 'item pending');
      shown.append(el('span', null, text));
      ul.querySelectorAll('li.empty').forEach(x => x.remove());
      ul.insertBefore(shown, add);
      i.focus();
      // Home Assistant sends the list again in a moment.
      fresh();
    });
    s.append(el('span', 'lab', t('panel_to_buy', { w: l.name, n: l.items.length })), ul);
    box.append(s);
  });
  const hint = el('p', 'fine', t('panel_shopping_hint'));
  return frag(h, box, hint);
}

// ---- Settings: one column of groups of rows, an index beside it (a phone: pills on top);
// Access from the browser at the end, with the devices let in.

function group(key) { return ((D.schema && D.schema.groups) || []).find(g => g.key === key); }

function settingsGroup(key) {
  const g = group(key);
  const ul = el('ul', 'group');
  const night = D.values.night_on === true;
  for (const it of g.items) {
    if (it.key === 'night_end') continue;
    // How many windows in a row: only with conversation mode on, as on the screen.
    if (it.key === 'conversation_max' && D.values.conversation_mode !== true) continue;
    // The hours of the night: one row, only with the night on.
    if (it.key === 'night_start') {
      if (!night) continue;
      const end = item('night_end');
      const hours = el('span', 'sub-row');
      hours.append(el('span', null, t('panel_from')), control(it, D.values.night_start, v => save(it, v)),
        el('span', null, t('panel_to')), end ? control(end, D.values.night_end, v => save(end, v)) : null);
      ul.append(row(t('panel_night_hours'), null, hours, 'wraps'));
      continue;
    }
    if (it.kind === 'multi') {
      const m = multi(it, D.values[it.key], v => save(it, v));
      // (the schema's help explains the old ticks: «All | Only these» says it now)
      ul.append(row(it.label, null, m.seg, 'wraps'));
      if (m.seg.lastChild.getAttribute('aria-pressed') === 'true') {
        const li = el('li', 'row sub');
        m.list.classList.add('inner');
        li.append(m.list);
        ul.append(li);
      }
      continue;
    }
    const c = control(it, D.values[it.key], v => save(it, v));
    ul.append(row(it.label, it.help, c, c && c.classList.contains('seg') ? 'wraps' : null));
  }
  const s = el('section');
  const h = el('h2', null, g.label);
  h.id = 's-' + key;
  s.append(h, ul);
  if (key === 'screen') {
    const p = el('p', 'fine');
    p.append(document.createTextNode(t('panel_rotation_in') + ' '), link('inl', 'home', t('panel_tab_home')), document.createTextNode('.'));
    s.append(p);
  }
  return s;
}

function when(sec) {
  try {
    return new Date(sec * 1000).toLocaleString(lang, { day: 'numeric', month: 'short', hour: '2-digit', minute: '2-digit' });
  } catch (e) {
    return '';
  }
}

function deviceName(s) { return t('panel_device_line', { browser: s.browser, os: s.os }); }

// Access from the browser: the remote screen (read only: it is switched on the device), the
// devices let in (revoke asks twice), log out of this one.
function accessGroup() {
  const s = el('section');
  const h = el('h2', null, t('panel_access'));
  h.id = 's-access';
  const ul = el('ul', 'group');
  const on = !!(D.screen && D.screen.enabled);
  const ro = el('span', 'ro');
  ro.append(ico('lock'), el('span', 'pill', t(on ? 'panel_access_on' : 'panel_access_off')));
  ul.append(row(t('panel_screen'), t('panel_screen_ro'), ro));
  for (const d of (D.devices && D.devices.sessions) || []) {
    const line = deviceName(d);
    const detail = d.peer + ' · ' + t('panel_last_used', { w: when(d.last_used) });
    if (d.current) { ul.append(row(line, detail, el('span', 'pill', t('panel_devices_this')))); continue; }
    const b = button('btn danger', t('panel_revoke'));
    b.setAttribute('aria-label', t('panel_revoke_x', { w: line }));
    let armed = 0;
    b.addEventListener('click', async () => {
      // Asks twice: a second touch within 3 s.
      if (!armed) {
        b.classList.add('armed');
        b.textContent = t('panel_revoke_sure');
        say(t('panel_revoke_sure_x', { w: line }));
        armed = setTimeout(() => { armed = 0; b.classList.remove('armed'); b.textContent = t('panel_revoke'); }, 3000);
        return;
      }
      clearTimeout(armed);
      b.disabled = true;
      const r = await api('/api/sessions/revoke', { id: d.id });
      if (!r.ok) toast(errorText(r.error), true);
      else toast(t('panel_revoked', { w: line }));
      const fresh = await api('/api/sessions');
      if (fresh.ok) D.devices = fresh;
      render(true);
    });
    ul.append(row(line, detail, b));
  }
  const out = el('div', 'after');
  out.append(button('btn quiet', t('panel_logout'), async () => { await api('/api/logout', {}); login(); }));
  s.append(h, ul, help('panel_access_new_hint'), out);
  return s;
}

function settingsPage() {
  const box = el('div', 'settings');
  const toc = el('nav', 'toc');
  toc.setAttribute('aria-label', t('panel_tab_settings'));
  const col = el('div');
  const keys = GROUPS.filter(group).concat('access');
  for (const k of keys) {
    col.append(k === 'access' ? accessGroup() : settingsGroup(k));
    const a = link(null, 'impostazioni/' + k, k === 'access' ? t('panel_access') : group(k).label);
    a.dataset.g = k;
    a.addEventListener('click', ev => {
      ev.preventDefault();
      history.replaceState(null, '', '#impostazioni/' + k);
      const target = document.getElementById('s-' + k);
      if (target) target.scrollIntoView({ behavior: matchMedia('(prefers-reduced-motion: reduce)').matches ? 'auto' : 'smooth' });
      mark(k);
    });
    toc.append(a);
  }
  const mark = k => { for (const a of toc.children) a.classList.toggle('cur', a.dataset.g === k); };
  mark(route().k || keys[0]);
  box.append(toc, col);
  return frag(pagehead(t('panel_tab_settings')), box);
}

// The index follows the page.
addEventListener('scroll', () => {
  if (route().p !== 'impostazioni') return;
  const toc = document.querySelector('.toc');
  if (!toc) return;
  let cur = null;
  for (const h of document.querySelectorAll('.settings h2')) if (h.getBoundingClientRect().top < 160) cur = h.id.slice(2);
  if (!cur) cur = toc.firstChild && toc.firstChild.dataset.g;
  for (const a of toc.children) a.classList.toggle('cur', a.dataset.g === cur);
}, { passive: true });

// ---- Screen: the device's screen, live, touched from here (crates/home-companion/src/panel/
// remote.rs). Frames are asked one at a time (the panel waits for a new one), only while this
// page is open and visible; touches go in small batches. The device shows who is in control.
// The state is always written under the frame: ready (not shared), waiting (asked on the device),
// live, paused (a private page), busy (another device), lost, off (switched off on the device),
// stopped, denied, unanswered, cannotask (on the device).

const R = { state: 'checking', why: '', by: '', live: false, seq: 0, run: 0, queue: [], flush: 0, down: false,
  since: 0, shots: [], box: null, glass: null, canvas: null, taps: null, cover: null, chip: null, bar: null,
  drawn: false, poll: 0, tick: 0, shown: '', hold: false };

function screenBuild() {
  if (R.box) return;
  R.canvas = el('canvas');
  R.canvas.width = 640; R.canvas.height = 400;
  R.canvas.setAttribute('aria-label', t('panel_screen_title'));
  R.canvas.hidden = true;
  R.taps = document.createElementNS(NS, 'svg');
  R.taps.setAttribute('class', 'taps');
  R.taps.setAttribute('viewBox', '0 0 1280 800');
  R.taps.setAttribute('aria-hidden', 'true');
  R.cover = el('div', 'cover');
  R.chip = el('span', 'livechip');
  const f = deviceFrame(R.canvas, t('panel_screen_title'));
  R.glass = f.glass;
  R.glass.append(R.taps, R.chip, R.cover);
  R.bar = el('div', 'sbar');
  R.bar.setAttribute('aria-live', 'polite');
  R.box = el('div', 'screen');
  R.box.append(f.device, R.bar);
  const at = e => {
    const b = R.canvas.getBoundingClientRect();
    const c = v => Math.min(1, Math.max(0, v));
    return { x: c((e.clientX - b.left) / b.width), y: c((e.clientY - b.top) / b.height) };
  };
  R.canvas.addEventListener('pointerdown', e => {
    if (R.state !== 'live' || R.down) return;
    R.down = true;
    R.canvas.setPointerCapture(e.pointerId);
    const p = at(e);
    R.queue.push({ k: 'd', ...p });
    ring(p);
    screenFlush();
    e.preventDefault();
  });
  R.canvas.addEventListener('pointermove', e => {
    if (!R.live || !R.down) return;
    const p = at(e);
    const last = R.queue[R.queue.length - 1];
    // Within one batch only the last move counts.
    if (last && last.k === 'm') { last.x = p.x; last.y = p.y; } else R.queue.push({ k: 'm', ...p });
    if (!R.flush) R.flush = setTimeout(screenFlush, 70);
  });
  const up = e => {
    if (!R.down) return;
    R.down = false;
    R.queue.push({ k: 'u', ...at(e) });
    screenFlush();
  };
  R.canvas.addEventListener('pointerup', up);
  R.canvas.addEventListener('pointercancel', up);
}

// A gold ring where the touch went.
function ring(p) {
  const c = document.createElementNS(NS, 'circle');
  c.setAttribute('cx', (p.x * 1280).toFixed(0));
  c.setAttribute('cy', (p.y * 800).toFixed(0));
  c.setAttribute('r', '40');
  R.taps.append(c);
  setTimeout(() => c.remove(), 600);
}

function mins(ms) { const s = Math.max(0, Math.floor(ms / 1000)); return Math.floor(s / 60) + ':' + String(s % 60).padStart(2, '0'); }

// What the state says: the layer over the frame, the line under it, the chip on it.
function screenView() {
  if (!R.box) return;
  const s = R.state;
  R.shown = s + '|' + R.why + '|' + R.by;
  R.box.classList.toggle('live', s === 'live');
  R.box.classList.toggle('paused', s === 'paused');
  R.canvas.hidden = !(R.drawn && (s === 'live' || s === 'paused'));
  // The layer: what happens, and the one thing to do.
  const c = R.cover;
  const inner = el('div');
  const retry = () => button('btn', t('panel_retry'), () => { R.hold = false; setState('checking'); screenCheck(true); });
  const me = ((D.devices && D.devices.sessions) || []).find(x => x.current);
  if (s === 'ready') {
    inner.append(el('h2', null, t('panel_screen_ready_title')), el('p', 'why', t('panel_screen_ready_text')),
      button('btn primary big', t('panel_screen_start'), () => { R.hold = false; screenStart(); }, 'screen'),
      el('p', 'fine', me ? t('panel_screen_sign', { w: deviceName(me) }) : t('panel_screen_lead')));
    if (D.screen && D.screen.confirm) inner.append(el('p', 'fine', t('panel_screen_will_ask')));
  } else if (s === 'waiting') {
    inner.append(el('h2', null, t('panel_screen_waiting_title')), el('p', null, t('panel_screen_waiting')),
      button('btn', t('panel_cancel'), () => { screenStop(true); }));
  } else if (['denied', 'unanswered', 'cannotask'].includes(s)) {
    inner.append(el('h2', null, t('panel_screen_' + s + '_title')), el('p', null, t('panel_screen_' + s)));
    if (s !== 'denied') inner.append(retry());
  } else if (s === 'off') {
    inner.append(ico('lock'), el('h2', null, t('panel_screen_off_title')), el('p', null, t('panel_screen_off')));
  } else if (s === 'paused') {
    inner.append(ico('lock'), el('h2', null, t('panel_screen_private_title')), el('p', null, t('panel_screen_hidden')));
  } else if (s === 'busy') {
    inner.append(el('h2', null, t('panel_screen_busy_title')),
      el('p', null, R.by ? t('panel_screen_busy_by', { w: R.by }) : t('panel_screen_busy')), retry());
  } else if (s === 'stopped') {
    inner.append(el('h2', null, t('panel_screen_stopped_title')), el('p', null, t('panel_screen_stopped')), retry());
  } else if (s === 'lost') {
    inner.append(el('h2', null, t('panel_screen_lost_title')),
      el('p', null, R.why === 'screen_unavailable' ? t('panel_screen_unavailable') : t('panel_screen_lost')), retry());
  }
  c.replaceChildren(inner);
  c.hidden = s === 'live' || s === 'checking';
  R.chip.hidden = s !== 'live';
  screenChip();
  // The line under the frame.
  const st = el('span', 'state');
  const strong = (k, rest) => { st.append(el('b', null, t(k))); if (rest) st.append(document.createTextNode(' · ' + rest)); };
  R.bar.replaceChildren(st);
  if (s === 'live') {
    strong('panel_screen_state_live', t('panel_screen_state_mine') + ' · ');
    const time = el('span', 'num', mins(Date.now() - R.since));
    time.id = 'screen-time';
    st.append(time);
  } else if (s === 'paused') strong('panel_screen_state_paused', t('panel_screen_state_private'));
  else if (s !== 'checking') st.textContent = t('panel_screen_state_' + s);
  if (s === 'live' || s === 'paused') {
    R.bar.append(button('btn', t('panel_screen_home'), async () => {
      const r = await api('/api/screen/home', {});
      if (r.ok) toast(t('panel_screen_home_done')); else toast(errorText(r.error), true);
    }, 'house'), button('btn danger', t('panel_screen_stop'), () => { screenStop(true); toast(t('panel_screen_stopped_here')); }, 'stop'));
  }
  if (route().p === 'schermo') drawNav();
}

// «Live · 4 pictures a second», counted here over the last 3 s.
function screenChip() {
  const now = Date.now();
  R.shots = R.shots.filter(x => now - x < 3000);
  const fps = Math.round(R.shots.length / 3);
  R.chip.replaceChildren(el('i'), document.createTextNode(t('panel_screen_state_live') + ' · ' + t('panel_screen_fps', { n: fps })));
}

// Drawn again only when the state (or who has it) changed: a poll that finds the same thing
// leaves the layer, the line and the focus alone.
function setState(s, why) {
  R.state = s;
  R.why = why || '';
  const key = s + '|' + R.why + '|' + R.by;
  if (key === R.shown) return;
  R.shown = key;
  screenView();
}

// A refusal or an end → the state it means.
function ended(code) {
  if (code === 'screen_off') return 'off';
  if (code === 'screen_busy') return 'busy';
  if (code === 'screen_stopped') return 'stopped';
  if (code === 'screen_denied') return 'denied';
  if (code === 'screen_unanswered') return 'unanswered';
  if (code === 'screen_cannot_ask') return 'cannotask';
  if (!code || code === 'screen_ended') return 'ready';
  return 'lost';
}

async function screenFlush() {
  clearTimeout(R.flush);
  R.flush = 0;
  if (!R.queue.length || !R.live) { R.queue = []; return; }
  const events = R.queue.splice(0, 32);
  const r = await api('/api/screen/input', { events });
  if (!r.ok && !['screen_hidden', 'slow_down', 'screen_waiting'].includes(r.error)) screenEnded(r.error);
  if (R.queue.length) screenFlush();
}

// What a session leaves behind: a finger down, touches not sent, its last picture.
function screenReset() {
  clearTimeout(R.flush);
  R.flush = 0;
  R.down = false;
  R.queue = [];
  R.drawn = false;
  R.shots = [];
  if (R.canvas) R.canvas.getContext('2d').clearRect(0, 0, R.canvas.width, R.canvas.height);
}

async function screenStart() {
  screenReset();
  const r = await api('/api/screen/start', {});
  if (!r.ok) { if (r.error === 'screen_busy') await screenCheck(false); setState(ended(r.error), r.error); return; }
  R.live = true;
  R.seq = 0;
  R.since = Date.now();
  setState(r.waiting ? 'waiting' : 'live');
  clearInterval(R.tick);
  R.tick = setInterval(() => {
    const e = document.getElementById('screen-time');
    if (e) e.textContent = mins(Date.now() - R.since);
    if (R.state === 'live') screenChip();
  }, 1000);
  screenLoop(++R.run);
}

function screenEnded(code) {
  R.live = false;
  R.run++;
  clearInterval(R.tick);
  screenReset();
  const s = ended(code);
  // Ended or taken over (another window of this browser, say): it starts again only when
  // asked to here, so two windows never take it from each other in turn.
  if (code === 'screen_ended') { R.hold = true; toast(t('panel_screen_ended')); }
  setState(s, code);
}

async function screenStop(tell) {
  clearInterval(R.tick);
  if (!R.live) return;
  R.live = false;
  R.run++;
  screenReset();
  if (R.box) setState('ready');
  if (tell) await api('/api/screen/stop', {});
}

// One frame after the other while this run lasts.
async function screenLoop(run) {
  const ctx = R.canvas.getContext('2d');
  let misses = 0;
  while (R.live && run === R.run) {
    let r;
    try {
      r = await fetch('/api/screen/frame?after=' + R.seq, { credentials: 'same-origin', cache: 'no-store' });
      misses = 0;
    } catch (e) {
      // Three misses in a row: the device is gone (Wi-Fi, power).
      if (++misses >= 3) { screenEnded('network'); return; }
      await new Promise(res => setTimeout(res, 1000));
      continue;
    }
    if (run !== R.run) return;
    if (r.status === 204) continue;
    if (r.status === 429) { await new Promise(res => setTimeout(res, 1000)); continue; }
    const type = r.headers.get('Content-Type') || '';
    if (r.ok && type.startsWith('image/jpeg')) {
      R.seq = Number(r.headers.get('X-Frame-Seq')) || 0;
      try {
        const bmp = await createImageBitmap(await r.blob());
        if (run !== R.run) { bmp.close(); return; }
        if (R.canvas.width !== bmp.width || R.canvas.height !== bmp.height) {
          R.canvas.width = bmp.width; R.canvas.height = bmp.height;
        }
        ctx.drawImage(bmp, 0, 0);
        bmp.close();
        R.shots.push(Date.now());
        // The first picture shows the canvas (drawn again even if the state is the same).
        if (!R.drawn) { R.drawn = true; R.shown = ''; }
        if (R.state === 'waiting') R.since = Date.now();
        setState('live');
      } catch (e) { /* a broken frame: the next one */ }
      continue;
    }
    let j = {};
    try { j = await r.json(); } catch (e) { j = { ok: false, error: 'network' }; }
    if (r.status === 401) { screenEnded(); login(); return; }
    if (j.ok && j.waiting) { setState('waiting'); await new Promise(res => setTimeout(res, 700)); continue; }
    if (j.ok && j.hidden) {
      ctx.fillStyle = '#03050d';
      ctx.fillRect(0, 0, R.canvas.width, R.canvas.height);
      R.seq = 0;
      if (R.state !== 'paused') setState('paused');
      await new Promise(res => setTimeout(res, 700));
      continue;
    }
    screenEnded(j.error);
    return;
  }
}

// Whether it may, whether another device is in control, whether this one is already (a
// reload: it goes on). `resume` (the page opened, Retry): start again if this device had it;
// the poll never starts anything.
async function screenCheck(resume) {
  const s = await api('/api/screen');
  if (!s.ok || R.live) return;
  D.screen = s;
  R.by = s.by ? deviceName(s.by) : '';
  if (!s.enabled) setState('off');
  else if (s.active && !s.mine) setState('busy');
  else if (s.mine && resume && !R.hold) screenStart();
  else if (resume || ['checking', 'off', 'busy'].includes(R.state)) setState('ready');
}

function screenLeave() {
  clearInterval(R.poll);
  R.poll = 0;
  screenStop(true);
}

function screenPage() {
  screenBuild();
  // Under the layer: the home as it looks now, until the first frame.
  const old = R.glass.querySelector('.scr');
  const on = onPages();
  const k = (D.widgets && D.widgets.showing) || (on[0] || {}).id;
  const p = k && page(k);
  const fresh = deviceScreen(p || null, p ? shownModel(p).model : null, 0);
  if (old) old.replaceWith(fresh); else R.glass.prepend(fresh);
  screenView();
  if (!R.live && !R.poll) {
    screenCheck(true);
    // While nobody here controls it: its state again every few seconds (switched on on the
    // device, the other device done).
    R.poll = setInterval(() => { if (!R.live && !document.hidden) screenCheck(false); }, 4000);
  }
  const facts = el('div', 'facts');
  for (const k2 of ['tap', 'seen', 'private']) {
    const d = el('div');
    d.append(el('b', null, t('panel_fact_' + k2)), el('p', null, t('panel_fact_' + k2 + '_text')));
    facts.append(d);
  }
  return frag(pagehead(t('panel_tab_screen'), t('panel_screen_sub')), R.box, facts);
}

// ---- Board («Plancia»): pages of tiles (6 x 4, the whole screen) and custom widgets (4 x 2) of Home Assistant's
// entities (crates/home-companion/src/ui/board.rs, panel/board_ops.rs). The canvases on top, the
// grid as on the device with the tiles the body draws (their state refreshed every 5 s), the
// chosen tile's form beside it (under it on a phone, and under a page: it is as wide as the screen). Every change is saved at once as the whole
// canvas; the body keeps what is allowed and says what it left out. Drag and resize with
// gridstack.js (web/vendor, loaded when the section opens), snapped to the sizes a tile may
// have; without a pointer: the size buttons and Move (then a place).

const BD = { sel: -1, adding: null, moving: false, replacing: false, adv: false, grid: null, gs: null, busy: false,
  find: { q: '', hits: null, last: 0, timer: 0, seq: 0 }, icons: { open: false, q: '', hits: null, all: true, timer: 0, seq: 0 } };

function loadGridstack() {
  if (BD.gs) return BD.gs;
  // After the fonts: the panel takes four connections from one address at once.
  BD.gs = document.fonts.ready.then(() => new Promise((ok, no) => {
    const l = el('link');
    l.rel = 'stylesheet';
    l.href = '/vendor/gridstack-14.1.0.css';
    document.head.append(l);
    const s = el('script');
    s.src = '/vendor/gridstack-14.1.0.js';
    s.addEventListener('load', () => ok(window.GridStack));
    s.addEventListener('error', () => { BD.gs = null; no(new Error('gridstack')); });
    document.head.append(s);
  }));
  return BD.gs;
}

function canvases() { return (D.board && D.board.canvases) || []; }
function canvasOf(id) { return canvases().find(c => c.id === id); }
function rowsOf(c) { return c.type === 'widget' ? D.board.grids.widget[1] : D.board.grids.page[1]; }
function colsOf(c) { return c.type === 'widget' ? D.board.grids.widget[0] : D.board.grids.page[0]; }
// The grid's width in the device's pixels: its cells and gaps, and half a gap at each side.
function gridUnits(c) {
  const k = c.type === 'widget' ? D.board.cells.widget : D.board.cells.page;
  return colsOf(c) * (k.width + k.gap);
}
// The tile's form is under the grid on a phone.
function formUnder() { return matchMedia('(max-width: 760px)').matches; }
function spanKey(s) { return s[0] + '×' + s[1]; }
function rawTiles(c) { return c.tiles.map(x => Object.assign({}, x.tile)); }

// The tile fits at (x, y) as w × h: inside, over no other tile (`skip`: the tile itself).
function fits(tiles, skip, x, y, w, h, rows, cols) {
  if (x < 0 || y < 0 || x + w > cols || y + h > rows) return false;
  return tiles.every((o, i) => i === skip || x >= o.x + o.w || o.x >= x + w || y >= o.y + o.h || o.y >= y + h);
}

// The first place a w × h tile fits, from (x, y) on then from the start.
function placeFor(tiles, skip, w, h, rows, cols, x, y) {
  if (fits(tiles, skip, x, y, w, h, rows, cols)) return { x, y };
  for (let yy = 0; yy < rows; yy++) for (let xx = 0; xx < cols; xx++) if (fits(tiles, skip, xx, yy, w, h, rows, cols)) return { x: xx, y: yy };
  return null;
}

// Saves a canvas's tiles; the body's answer is drawn. `sel`: the tile chosen afterwards.
async function boardSave(c, tiles, opt) {
  const o = opt || {};
  BD.busy = true;
  const r = await api('/api/board/set', { canvas: { id: c.id, tiles } });
  BD.busy = false;
  if (!r.ok) { toast(errorText(r.error), true); render(true); return false; }
  D.board = r;
  if (o.sel !== undefined) BD.sel = o.sel;
  const nc = canvasOf(c.id);
  if (nc && BD.sel >= nc.tiles.length) BD.sel = -1;
  if (r.dropped && r.dropped.length) toast(t('panel_board_dropped'), { bad: true, undo: boardUndo });
  else toast(o.text || t('panel_saved'), { undo: boardUndo });
  render(true);
  return true;
}

async function boardUndo() {
  const r = await api('/api/board/undo', {});
  if (!r.ok) { toast(errorText(r.error), true); return; }
  D.board = r;
  BD.sel = -1;
  if (!canvasOf(route().k) && canvases()[0]) go('plancia/' + canvases()[0].id); else render(true);
}

// A canvas's op (add, remove, name, order): the answer drawn.
async function boardOp(path, body, then) {
  const r = await api('/api/board/' + path, body);
  if (!r.ok) { toast(errorText(r.error), true); return null; }
  D.board = r;
  if (then) then(r); else render(true);
  return r;
}

// An MDI icon as the screen draws it (filled, 24 × 24).
function mdi(d, cls) {
  const s = svg(d, cls || 'mdi');
  return s;
}

// A tile as the device draws it, from the body's model (`draw`).
function tileFace(x) {
  const d = x.draw;
  const t0 = x.tile;
  const f = el('div', 'bt k-' + t0.kind + ' s-' + d.state + ' w' + t0.w + ' h' + t0.h);
  const icon = mdi(d.path, 'mdi bi');
  const nm = el('span', 'bn', d.name);
  const st = el('span', 'bs', d.status);
  if (t0.kind === 'sensor') {
    const top = el('div', 'btop');
    top.append(icon, el('span', 'bn', d.name));
    f.append(top, el('span', 'bv', d.value || d.status));
    if (d.curve) {
      const box = t0.w === 4 ? [440, 44] : t0.h === 2 ? [D.board.cells.page.width * 2 + D.board.cells.page.gap - 40, 84] : [150, 44];
      const c = document.createElementNS(NS, 'svg');
      c.setAttribute('viewBox', '0 0 ' + box[0] + ' ' + box[1]);
      c.setAttribute('class', 'bc');
      c.setAttribute('aria-hidden', 'true');
      c.setAttribute('preserveAspectRatio', 'none');
      const p = document.createElementNS(NS, 'path');
      p.setAttribute('d', d.curve);
      c.append(p);
      f.append(c);
    }
    if (d.range) f.append(el('span', 'br', d.range));
  } else if (t0.kind === 'level') {
    const top = el('div', 'btop');
    top.append(nm, el('span', 'bs', d.status));
    const bar = el('div', 'bl');
    const fill = el('i', 'lv' + Math.round(Math.max(0, d.level) / 5));
    bar.append(fill, icon, el('span', 'bv', d.value));
    f.append(top, bar);
  } else {
    const words = el('div', 'bw');
    words.append(nm);
    if (d.status) words.append(st);
    f.append(icon, words);
  }
  if (d.confirm && t0.kind !== 'sensor') {
    const q = el('i', 'bq');
    q.title = t('panel_board_confirm');
    f.append(q);
  }
  return f;
}

// The page.
function boardPage(r) {
  const head = pagehead(t('panel_tab_board'), t('panel_board_sub'));
  if (!D.board || !D.board.ok) return frag(head, el('p', 'lead', t('panel_error')));
  const all = canvases();
  const c = canvasOf(r.k) || all[0];
  if (c && c.id !== r.k) history.replaceState(null, '', '#plancia/' + c.id);
  if (!c || c.id !== BD.cv) { BD.sel = -1; BD.adding = null; BD.moving = false; BD.replacing = false; BD.icons.open = false; }
  BD.cv = c ? c.id : '';
  const box = el('div', 'board');
  box.append(canvasRow(c));
  if (!c) {
    const e = el('div', 'bempty');
    e.append(art('idle'), el('p', 'lead', t('panel_board_empty')));
    box.append(e);
    return frag(head, box);
  }
  // One screen: the grid as large as the window lets it be, the inspector beside it (the tile,
  // or the page itself when no tile is chosen). On a phone the inspector goes under the grid.
  const lay = el('div', 'blay ' + (c.type === 'widget' ? 'wide' : 'tall'));
  const stage = el('div', 'bstage');
  stage.append(gridFrame(c));
  const ed = el('aside', 'editor bform');
  ed.setAttribute('aria-label', t('panel_board_tile'));
  tileForm(ed, c);
  lay.append(stage, ed);
  box.append(lay);
  if (!D.board.ha) box.append(el('p', 'fine', t('panel_no_ha')));
  requestAnimationFrame(() => mountGrid(c));
  return frag(head, box);
}

// The canvases: pages, then widgets, each with «+».
function canvasRow(cur) {
  const row = el('div', 'cvrow');
  row.setAttribute('role', 'group');
  row.setAttribute('aria-label', t('panel_tab_board'));
  const lim = D.board.limits;
  for (const [type, word, add, max] of [['page', 'panel_board_pages', 'panel_board_add_page', lim.pages],
    ['widget', 'panel_board_widgets', 'panel_board_add_widget', lim.widgets]]) {
    const g = el('div', 'cvg');
    g.append(el('span', 'lab', t(word)));
    const list = canvases().filter(c => c.type === type);
    for (const c of list) {
      const a = link('cvpill', 'plancia/' + c.id, c.label);
      if (cur && c.id === cur.id) a.setAttribute('aria-current', 'page');
      a.append(el('span', 'num cnt', String(c.tiles.length)));
      g.append(a);
    }
    if (list.length < max) {
      const b = button('cvpill add', t(add), async ev => {
        ev.currentTarget.disabled = true;
        await boardOp(type + '/add', { name: '' }, r => { BD.sel = -1; go('plancia/' + r.added); });
      }, 'plus');
      b.id = 'add-' + type;
      g.append(b);
    }
    row.append(g);
  }
  return row;
}

// The grid: the device's cells (each empty one a «+»), the tiles over them; a page under its
// header as the device shows it (Alfred's face, the page's name, the time).
function gridFrame(c) {
  const rows = rowsOf(c);
  const cols = colsOf(c);
  // Its columns and width in the device's pixels: classes (panel.css), no style attribute.
  const frame = el('div', 'bframe ' + (c.type === 'widget' ? 'r2' : 'r4') + ' c' + cols + (BD.moving ? ' moving' : ''));
  if (c.type === 'page') {
    const head = el('div', 'bhead');
    head.setAttribute('aria-hidden', 'true');
    head.append(art('face'), el('b', null, c.label), el('span', 'num', hm()));
    frame.append(head);
  }
  const body = el('div', 'bbody');
  const cells = el('div', 'bcells');
  const taken = new Set();
  c.tiles.forEach(x => { for (let yy = x.tile.y; yy < x.tile.y + x.tile.h; yy++) for (let xx = x.tile.x; xx < x.tile.x + x.tile.w; xx++) taken.add(yy * cols + xx); });
  const full = c.tiles.length >= (c.type === 'widget' ? D.board.limits.widget_tiles : D.board.limits.page_tiles);
  for (let yy = 0; yy < rows; yy++) {
    for (let xx = 0; xx < cols; xx++) {
      const b = el('button', 'bcell');
      b.type = 'button';
      b.id = 'cell-' + xx + '-' + yy;
      const self = BD.moving && BD.sel >= 0 && (() => { const s = c.tiles[BD.sel].tile; return xx >= s.x && xx < s.x + s.w && yy >= s.y && yy < s.y + s.h; })();
      if ((taken.has(yy * cols + xx) && !self) || (full && !BD.moving)) { b.classList.add('taken'); b.tabIndex = -1; b.setAttribute('aria-hidden', 'true'); }
      const here = BD.adding && BD.adding.x === xx && BD.adding.y === yy;
      if (here) b.setAttribute('aria-pressed', 'true');
      b.setAttribute('aria-label', t(BD.moving ? 'panel_board_move_here' : 'panel_board_cell_add', { r: yy + 1, c: xx + 1 }));
      b.append(ico('plus'));
      b.addEventListener('click', () => cellTapped(c, xx, yy));
      cells.append(b);
    }
  }
  const g = el('div', 'grid-stack');
  g.id = 'bgrid';
  c.tiles.forEach((x, i) => {
    const it = el('div', 'grid-stack-item');
    const tl = x.tile;
    for (const [k, v] of [['gs-x', tl.x], ['gs-y', tl.y], ['gs-w', tl.w], ['gs-h', tl.h], ['gs-id', i]]) it.setAttribute(k, String(v));
    const spans = (x.allowed[tl.kind] || [[tl.w, tl.h]]);
    it.setAttribute('gs-min-w', String(Math.min(...spans.map(s => s[0]))));
    it.setAttribute('gs-max-w', String(Math.max(...spans.map(s => s[0]))));
    it.setAttribute('gs-min-h', String(Math.min(...spans.map(s => s[1]))));
    it.setAttribute('gs-max-h', String(Math.min(rows, Math.max(...spans.map(s => s[1])))));
    const content = el('div', 'grid-stack-item-content');
    content.id = 'bt-' + i;
    content.tabIndex = 0;
    content.setAttribute('role', 'button');
    content.setAttribute('aria-pressed', String(i === BD.sel));
    content.setAttribute('aria-label', t('panel_board_tile_of', { name: x.draw.name, w: tl.w, h: tl.h, r: tl.y + 1, c: tl.x + 1 }));
    content.append(tileFace(x));
    const pick = () => {
      if (BD.dragged) return;
      if (BD.moving) { cellTapped(c, tl.x, tl.y); return; }
      BD.sel = i; BD.adding = null; BD.replacing = false; BD.icons.open = false;
      render(true);
      const f = document.getElementById('bt-' + i);
      if (f) f.focus({ preventScroll: true });
      // The form is under the grid: brought into view.
      if (formUnder()) { const ed = document.querySelector('.bform'); if (ed) ed.scrollIntoView({ block: 'nearest', behavior: 'smooth' }); }
    };
    content.addEventListener('click', pick);
    content.addEventListener('keydown', e => { if (e.key === 'Enter' || e.key === ' ') { e.preventDefault(); pick(); } });
    it.append(content);
    g.append(it);
  });
  body.append(cells, g);
  frame.append(body);
  return frame;
}

// The grid comes alive (gridstack) once it is on the page; without the script it stays as
// drawn, and the size buttons and Move still work.
async function mountGrid(c) {
  let GS;
  try { GS = await loadGridstack(); } catch (e) { return; }
  const g = document.getElementById('bgrid');
  if (!g || !g.isConnected || BD.cv !== c.id) return;
  if (BD.grid) { try { BD.grid.destroy(false); } catch (e) { /* gone with its page */ } BD.grid = null; }
  const rows = rowsOf(c);
  const gw = gridUnits(c);
  const unit = g.getBoundingClientRect().width / gw;
  const ch = (c.type === 'widget' ? D.board.cells.widget.height : D.board.cells.page.height) + 12;
  const grid = GS.init({
    column: colsOf(c), maxRow: rows, minRow: rows, float: true, animate: false,
    cellHeight: Math.round(ch * unit), margin: Math.max(2, Math.round(6 * unit)),
    resizable: { handles: 'se' }, draggable: { cancel: 'button, input' },
    acceptWidgets: false, removable: false,
  }, g);
  BD.grid = grid;
  grid.on('dragstart resizestart', () => { BD.dragged = true; BD.busy = true; });
  grid.on('dragstop resizestop', (ev, item) => {
    setTimeout(() => { BD.dragged = false; }, 0);
    BD.busy = false;
    gridChanged(c, item);
  });
  const fit = () => {
    if (!g.isConnected) { removeEventListener('resize', fit); return; }
    const u = g.getBoundingClientRect().width / gw;
    grid.cellHeight(Math.round(ch * u));
    grid.margin(Math.max(2, Math.round(6 * u)));
  };
  addEventListener('resize', fit);
}

// After a drag or a resize: each tile snapped to the nearest size its kind may have, then the
// canvas saved if anything moved.
function gridChanged(c, item) {
  const grid = BD.grid;
  if (!grid) return;
  const rows = rowsOf(c);
  const node = item && item.gridstackNode;
  if (node) {
    const x = c.tiles[Number(node.id)];
    const spans = x.allowed[x.tile.kind] || [[x.tile.w, x.tile.h]];
    if (!spans.some(s => s[0] === node.w && s[1] === node.h)) {
      const best = spans.slice().sort((a, b) => (Math.abs(a[0] - node.w) * 2 + Math.abs(a[1] - node.h)) - (Math.abs(b[0] - node.w) * 2 + Math.abs(b[1] - node.h)))
        .find(s => node.x + s[0] <= colsOf(c) && node.y + s[1] <= rows);
      if (best) grid.update(item, { w: best[0], h: best[1] });
      else grid.update(item, { x: x.tile.x, y: x.tile.y, w: x.tile.w, h: x.tile.h });
    }
  }
  const tiles = rawTiles(c);
  let moved = false;
  for (const n of grid.engine.nodes) {
    const i = Number(n.id);
    const tl = tiles[i];
    if (!tl) continue;
    if (tl.x !== n.x || tl.y !== n.y || tl.w !== n.w || tl.h !== n.h) moved = true;
    Object.assign(tl, { x: n.x, y: n.y, w: n.w, h: n.h });
  }
  const sel = node ? Number(node.id) : BD.sel;
  if (moved) boardSave(c, tiles, { sel });
  else { BD.sel = sel; render(true); }
}

// A place tapped: a tile there (adding), or the chosen tile moved there.
function cellTapped(c, x, y) {
  if (BD.moving && BD.sel >= 0) {
    const tiles = rawTiles(c);
    const s = tiles[BD.sel];
    BD.moving = false;
    if (!fits(tiles, BD.sel, x, y, s.w, s.h, rowsOf(c), colsOf(c))) { toast(t('panel_board_no_room'), true); render(true); return; }
    s.x = x; s.y = y;
    boardSave(c, tiles, { sel: BD.sel, text: t('panel_board_moved') });
    return;
  }
  BD.adding = { x, y };
  BD.sel = -1;
  BD.replacing = false;
  render(true);
  const i = document.getElementById('bfind');
  if (i) i.focus({ preventScroll: true });
  if (formUnder()) { const ed = document.querySelector('.bform'); if (ed) ed.scrollIntoView({ block: 'nearest', behavior: 'smooth' }); }
}

// Under the grid: the canvas's own name, its order (pages) or its place on the home (widgets),
// removing it.
function canvasBar(c) {
  const bar = el('div', 'cvbar');
  const id = 'cvname';
  const i = el('input');
  i.type = 'text';
  i.id = id;
  i.autocomplete = 'off';
  i.maxLength = c.type === 'widget' ? D.board.limits.widget_name : D.board.limits.name;
  i.value = c.name || '';
  i.placeholder = c.label;
  i.addEventListener('change', () => boardOp(c.type + '/name', { canvas: c.id, name: i.value.trim() }, () => { toast(t('panel_saved')); render(true); }));
  i.addEventListener('keydown', e => { if (e.key === 'Enter') { e.preventDefault(); i.blur(); } });
  const nm = el('div', 'cvname');
  nm.append(label(t(c.type === 'widget' ? 'panel_board_widget_name' : 'panel_board_page_name'), id), i);
  bar.append(nm);
  const line = el('p', 'fine');
  if (c.type === 'widget') {
    line.append(document.createTextNode(t('panel_board_widget_hint') + ' '));
    if (c.page) {
      line.append(document.createTextNode((c.on ? t('panel_on_home') : t('panel_not_on_home')) + ' · '),
        link('inl', 'home/' + c.page, t('panel_tab_home')));
    }
  } else line.textContent = t('panel_board_page_hint');
  bar.append(line);
  const acts = el('div', 'cvacts');
  if (c.type === 'page') {
    const pagesNow = canvases().filter(x => x.type === 'page').map(x => x.id);
    const at = pagesNow.indexOf(c.id);
    if (pagesNow.length > 1) {
      const mv = d => { const ids = pagesNow.slice(); [ids[at], ids[at + d]] = [ids[at + d], ids[at]]; boardOp('page/order', { order: ids }, () => { toast(t('panel_order_saved')); render(true); }); };
      const b = button('btn quiet', t('panel_before'), () => mv(-1), 'left');
      b.disabled = at === 0;
      const a = el('button', 'btn quiet');
      a.type = 'button';
      a.append(document.createTextNode(t('panel_after')), ico('right'));
      a.disabled = at === pagesNow.length - 1;
      a.addEventListener('click', () => mv(1));
      acts.append(b, a);
    }
  }
  acts.append(el('span', 'grow'));
  const rm = button('btn danger', t(c.type === 'widget' ? 'panel_board_remove_widget' : 'panel_board_remove_page'), async ev => {
    ev.currentTarget.disabled = true;
    const r = await api('/api/board/' + c.type + '/remove', { canvas: c.id });
    if (!r.ok) { toast(errorText(r.error), true); render(true); return; }
    D.board = r;
    const next = canvases()[0];
    go('plancia' + (next ? '/' + next.id : ''));
    toast(t('panel_removed', { w: c.label }), { undo: boardUndo });
  }, 'trash');
  rm.id = 'cvremove';
  acts.append(rm);
  bar.append(acts);
  return bar;
}

// The form beside the grid: adding (a search), the chosen tile, or how to begin.
function tileForm(ed, c) {
  if (BD.adding) { ed.append(addForm(c)); return; }
  const x = BD.sel >= 0 ? c.tiles[BD.sel] : null;
  if (!x) {
    ed.append(canvasBar(c));
    const e = el('div', 'bhint');
    e.append(el('p', null, t('panel_board_pick')), el('p', 'fine', t('panel_board_never')));
    ed.append(e);
    return;
  }
  const tl = x.tile;
  const head = el('div', 'edhead');
  const ic = el('span', 'ic');
  ic.append(mdi(x.draw.path));
  const words = el('div', 'grow');
  words.append(el('h2', null, x.draw.name), el('p', 'num', tl.entity_id));
  head.append(ic, words, button('btn quiet', t('panel_board_change'), () => { BD.replacing = !BD.replacing; BD.find.q = ''; BD.find.hits = null; render(true); const i = document.getElementById('bfind'); if (i) i.focus(); }),
    ibtn('close', t('panel_board_done'), () => { BD.sel = -1; BD.moving = false; BD.replacing = false; BD.icons.open = false; render(true); }));
  ed.append(head);
  if (BD.replacing) { ed.append(searchBlock(c, h => replaceEntity(c, h))); return; }
  const tiles = () => rawTiles(c);
  const put = (f, opt) => { const all = tiles(); f(all[BD.sel]); boardSave(c, all, Object.assign({ sel: BD.sel }, opt || {})); };

  // Name.
  const nid = 'tname';
  const n = el('input');
  n.type = 'text';
  n.id = nid;
  n.autocomplete = 'off';
  n.maxLength = D.board.limits.name;
  n.value = tl.name || '';
  n.placeholder = x.entity_name || x.draw.name;
  n.addEventListener('change', () => put(t1 => { t1.name = n.value.trim(); }));
  n.addEventListener('keydown', e => { if (e.key === 'Enter') { e.preventDefault(); n.blur(); } });
  ed.append(label(t('panel_board_name'), nid), n);

  // Icon.
  ed.append(label(t('panel_board_icon')));
  const ib = el('button', 'iconpick');
  ib.type = 'button';
  ib.id = 'ticon';
  ib.setAttribute('aria-expanded', String(BD.icons.open));
  ib.append(mdi(x.draw.path), el('span', 'grow', x.draw.icon + (tl.icon ? '' : ' · ' + t('panel_board_icon_default'))), ico('search'));
  ib.addEventListener('click', () => { BD.icons.open = !BD.icons.open; render(true); const i = document.getElementById('iconq'); if (i) i.focus(); });
  ed.append(ib);
  if (BD.icons.open) ed.append(iconPicker(name => put(t1 => { t1.icon = name; }), !!tl.icon));

  // Size and kind.
  const spans = x.allowed[tl.kind] || [];
  // Side by side: the size and the kind (one line, the form fits beside the grid).
  const pair = el('div', 'bpair');
  const sz = el('div');
  sz.append(label(t('panel_board_size')));
  const seg = el('div', 'seg');
  seg.setAttribute('role', 'group');
  seg.setAttribute('aria-label', t('panel_board_size'));
  for (const s of spans) {
    const b = button(null, spanKey(s), () => {
      const all = tiles();
      const p = placeFor(all, BD.sel, s[0], s[1], rowsOf(c), colsOf(c), tl.x, tl.y);
      if (!p) { toast(t('panel_board_no_room'), true); return; }
      Object.assign(all[BD.sel], { w: s[0], h: s[1], x: p.x, y: p.y });
      boardSave(c, all, { sel: BD.sel });
    });
    b.setAttribute('aria-pressed', String(s[0] === tl.w && s[1] === tl.h));
    seg.append(b);
  }
  sz.append(seg);
  pair.append(sz);
  ed.append(pair);
  const kinds = Object.keys(x.allowed);
  if (kinds.length > 1) {
    const kd = el('div');
    kd.append(label(t('panel_board_kind')));
    const ks = el('div', 'seg');
    ks.setAttribute('role', 'group');
    ks.setAttribute('aria-label', t('panel_board_kind'));
    for (const k of kinds) {
      const b = button(null, t('panel_board_kind_' + k), () => {
        const all = tiles();
        const me = all[BD.sel];
        const ok = x.allowed[k];
        let s = ok.find(s1 => s1[0] === me.w && s1[1] === me.h) ? [me.w, me.h] : null;
        let p = s ? { x: me.x, y: me.y } : null;
        for (const s1 of s ? [] : ok) { p = placeFor(all, BD.sel, s1[0], s1[1], rowsOf(c), colsOf(c), me.x, me.y); if (p) { s = s1; break; } }
        if (!s || !p) { toast(t('panel_board_no_room'), true); return; }
        Object.assign(me, { kind: k, w: s[0], h: s[1], x: p.x, y: p.y });
        if (k !== 'button') { delete me.service; delete me.service_data; }
        boardSave(c, all, { sel: BD.sel });
      });
      b.setAttribute('aria-pressed', String(k === tl.kind));
      ks.append(b);
    }
    kd.append(ks);
    pair.append(kd);
  }

  // Ask first.
  if (tl.kind !== 'sensor') {
    const cl = el('label', 'choice bconfirm');
    const cb = el('input', 'chk');
    cb.type = 'checkbox';
    cb.id = 'tconfirm';
    cb.checked = tl.confirm === undefined || tl.confirm === null ? x.confirm_default : tl.confirm;
    cb.addEventListener('change', () => put(t1 => {
      if (cb.checked === x.confirm_default) delete t1.confirm; else t1.confirm = cb.checked;
    }));
    const w2 = el('div', 'txt');
    w2.append(el('b', null, t('panel_board_confirm')), el('span', null, t(x.confirm_default ? 'panel_board_confirm_hint' : 'panel_board_confirm_off')));
    cl.append(cb, w2);
    ed.append(cl);
  }

  // Advanced: the service and its data by hand (buttons only; the body checks them).
  if (tl.kind === 'button') {
    const det = el('details', 'adv');
    det.open = BD.adv;
    det.addEventListener('toggle', () => { BD.adv = det.open; });
    const sm = el('summary');
    sm.append(el('span', null, t('panel_board_advanced')), el('span', 'fine', t('panel_board_advanced_hint')));
    det.append(sm);
    const sv = el('input');
    sv.type = 'text';
    sv.id = 'tservice';
    sv.autocomplete = 'off';
    sv.spellcheck = false;
    sv.value = tl.service || '';
    sv.placeholder = 'light.turn_on';
    const data = el('textarea');
    data.id = 'tdata';
    data.rows = 3;
    data.spellcheck = false;
    data.value = tl.service_data ? JSON.stringify(tl.service_data) : '';
    data.placeholder = '{"brightness_pct": 30}';
    const bad = el('p', 'fine bad');
    bad.hidden = true;
    const apply = button('btn', t('panel_board_apply'), () => {
      const s = sv.value.trim();
      let d = null;
      if (data.value.trim()) {
        try { d = JSON.parse(data.value); } catch (e) { d = undefined; }
        if (!d || typeof d !== 'object' || Array.isArray(d)) { bad.textContent = t('panel_board_bad_json'); bad.hidden = false; return; }
      }
      if (s && !/^[a-z0-9_]+\.[a-z0-9_]+$/.test(s)) { bad.textContent = t('panel_board_bad_service'); bad.hidden = false; return; }
      put(t1 => {
        if (s) { t1.service = s; if (d) t1.service_data = d; else delete t1.service_data; } else { delete t1.service; delete t1.service_data; }
      });
    });
    apply.id = 'tapply';
    det.append(label(t('panel_board_service'), 'tservice'), sv, label(t('panel_board_data'), 'tdata'), data, bad, apply);
    ed.append(det);
  }

  // Move, Delete.
  const foot = el('div', 'edfoot');
  const mv = button('btn', BD.moving ? t('panel_cancel') : t('panel_board_move'), () => { BD.moving = !BD.moving; render(true); if (BD.moving) say(t('panel_board_move_hint')); });
  mv.id = 'tmove';
  mv.setAttribute('aria-pressed', String(BD.moving));
  const del = button('btn danger', t('panel_board_delete'), () => {
    const all = tiles();
    all.splice(BD.sel, 1);
    boardSave(c, all, { sel: -1, text: t('panel_removed', { w: x.draw.name }) });
  }, 'trash');
  del.id = 'tdelete';
  foot.append(mv, el('span', 'grow'), del);
  ed.append(foot);
  if (BD.moving) ed.append(el('p', 'fine', t('panel_board_move_hint')));
}

// Adding a tile: where, the search, the hits (a tap places the tile as proposed).
function addForm(c) {
  const f = el('div');
  const head = el('div', 'edhead');
  const ic = el('span', 'ic');
  ic.append(ico('plus'));
  const words = el('div', 'grow');
  words.append(el('h2', null, t('panel_board_add_title')), el('p', null, t('panel_board_at', { r: BD.adding.y + 1, c: BD.adding.x + 1 })));
  head.append(ic, words, ibtn('close', t('panel_cancel'), () => { BD.adding = null; render(true); }));
  f.append(head, searchBlock(c, h => addTile(c, h)), el('p', 'fine', t('panel_board_never')));
  return f;
}

function addTile(c, h) {
  const tiles = rawTiles(c);
  const rows = rowsOf(c);
  const spans = h.allowed[h.kind] || [[1, 1]];
  let p = null;
  let s = null;
  for (const s1 of spans) { if (fits(tiles, -1, BD.adding.x, BD.adding.y, s1[0], s1[1], rows, colsOf(c))) { s = s1; p = BD.adding; break; } }
  for (const s1 of s ? [] : spans) { p = placeFor(tiles, -1, s1[0], s1[1], rows, colsOf(c), BD.adding.x, BD.adding.y); if (p) { s = s1; break; } }
  if (!s) { toast(t('panel_board_no_room'), true); return; }
  tiles.push({ x: p.x, y: p.y, w: s[0], h: s[1], kind: h.kind, entity_id: h.entity_id });
  BD.adding = null;
  boardSave(c, tiles, { sel: tiles.length - 1, text: t('panel_added', { w: h.name }) });
}

function replaceEntity(c, h) {
  const tiles = rawTiles(c);
  const me = tiles[BD.sel];
  const kind = h.allowed[me.kind] ? me.kind : h.kind;
  const spans = h.allowed[kind];
  let s = spans.find(s1 => s1[0] === me.w && s1[1] === me.h);
  let p = s ? { x: me.x, y: me.y } : null;
  for (const s1 of s ? [] : spans) { p = placeFor(tiles, BD.sel, s1[0], s1[1], rowsOf(c), colsOf(c), me.x, me.y); if (p) { s = s1; break; } }
  if (!s) { toast(t('panel_board_no_room'), true); return; }
  tiles[BD.sel] = { x: p.x, y: p.y, w: s[0], h: s[1], kind, entity_id: h.entity_id };
  BD.replacing = false;
  boardSave(c, tiles, { sel: BD.sel });
}

// The entities a tile may show: as you type (250 ms after the last key; the body allows a
// search every 2 s, the first list is free).
function searchBlock(c, pick) {
  const st = BD.find;
  const f = el('div', 'find');
  const i = el('input');
  i.type = 'search';
  i.id = 'bfind';
  i.maxLength = D.board.limits.query;
  i.autocomplete = 'off';
  i.placeholder = t('panel_board_search');
  i.setAttribute('aria-label', t('panel_board_search'));
  i.value = st.q;
  f.append(ico('search'), i);
  const out = el('ul', 'group hits bhits');
  out.setAttribute('aria-live', 'polite');
  const show = () => {
    out.replaceChildren();
    if (st.hits === null) { out.append(el('li', 'row empty', t('panel_searching'))); return; }
    if (typeof st.hits === 'string') { out.append(el('li', 'row empty', st.hits)); return; }
    if (!st.hits.length) { out.append(el('li', 'row empty', t(D.board.ha ? 'panel_search_none' : 'panel_no_ha'))); return; }
    const on = new Set(c.tiles.map(x => x.tile.entity_id));
    for (const h of st.hits) {
      const li = el('li', 'row');
      li.tabIndex = 0;
      li.setAttribute('role', 'button');
      li.setAttribute('aria-label', t('panel_add') + ': ' + h.name);
      const nm = el('div', 'txt');
      nm.append(el('b', null, h.name), el('span', null, h.entity_id + ' · ' + t('panel_board_kind_' + h.kind)));
      const a = el('span', 'add' + (on.has(h.entity_id) ? ' have' : ''));
      a.append(ico('plus'), el('span', null, t('panel_add')));
      li.append(mdi(h.path, 'mdi hi'), nm, a);
      const go1 = () => { if (!li.dataset.busy) { li.dataset.busy = '1'; pick(h); } };
      li.addEventListener('click', go1);
      li.addEventListener('keydown', e => { if (e.key === 'Enter' || e.key === ' ') { e.preventDefault(); go1(); } });
      out.append(li);
    }
  };
  st.show = () => { if (out.isConnected) show(); };
  const run = async () => {
    const seq = ++st.seq;
    const wait = st.q ? st.last + 2100 - Date.now() : 0;
    if (wait > 0) { st.timer = setTimeout(run, wait); return; }
    if (st.q) st.last = Date.now();
    const r = await api('/api/board/search', { q: st.q, kind: '' });
    if (seq !== st.seq) return;
    if (!r.ok && r.error === 'search_wait') { st.timer = setTimeout(run, 2100); return; }
    st.hits = r.ok ? r.hits : errorText(r.error);
    st.show();
  };
  i.addEventListener('input', () => {
    clearTimeout(st.timer);
    st.seq++;
    st.q = i.value.trim();
    st.hits = null;
    show();
    st.timer = setTimeout(run, 250);
  });
  i.addEventListener('keydown', e => { if (e.key === 'Escape') { BD.adding = null; BD.replacing = false; render(true); } });
  show();
  if (st.hits === null) run();
  return frag(f, out);
}

// The icons: a search among the Material Design Icons (the body answers 40 at most).
function iconPicker(choose, custom) {
  const st = BD.icons;
  const box = el('div', 'icons');
  const f = el('div', 'find');
  const i = el('input');
  i.type = 'search';
  i.id = 'iconq';
  i.maxLength = 32;
  i.autocomplete = 'off';
  i.placeholder = t('panel_board_icons_search');
  i.setAttribute('aria-label', t('panel_board_icons_search'));
  i.value = st.q;
  f.append(ico('search'), i);
  const out = el('div', 'igrid');
  out.setAttribute('role', 'group');
  out.setAttribute('aria-label', t('panel_board_icon'));
  const note = el('p', 'fine');
  const show = () => {
    out.replaceChildren();
    note.textContent = st.all ? '' : t('panel_board_icons_some');
    if (st.hits === null) { out.append(el('span', 'fine', t('panel_searching'))); return; }
    if (typeof st.hits === 'string') { out.append(el('span', 'fine', st.hits)); return; }
    if (!st.hits.length) { out.append(el('span', 'fine', t('panel_search_none'))); return; }
    for (const h of st.hits) {
      const b = el('button', 'ibox');
      b.type = 'button';
      b.title = h.name;
      b.setAttribute('aria-label', h.name);
      b.append(mdi(h.path));
      b.addEventListener('click', () => { st.open = false; choose(h.name); });
      out.append(b);
    }
  };
  st.show = () => { if (out.isConnected) show(); };
  const run = async () => {
    const seq = ++st.seq;
    const r = await api('/api/board/icons', { q: st.q });
    if (seq !== st.seq) return;
    if (!r.ok && r.error === 'search_wait') { st.timer = setTimeout(run, 250); return; }
    st.hits = r.ok ? r.icons : errorText(r.error);
    st.all = r.ok ? r.all : true;
    st.show();
  };
  i.addEventListener('input', () => {
    clearTimeout(st.timer);
    st.q = i.value.trim().toLowerCase();
    st.timer = setTimeout(run, 250);
  });
  i.addEventListener('keydown', e => { if (e.key === 'Escape') { st.open = false; render(true); } });
  box.append(f, out, note);
  if (custom) box.append(button('btn quiet', t('panel_board_icon_reset'), () => { st.open = false; choose(''); }));
  show();
  if (st.hits === null) run();
  return box;
}

// The tiles' states every 5 s while the section is in front (never under a finger or a field).
setInterval(async () => {
  if (!inside || document.hidden || route().p !== 'plancia' || !D.board || BD.busy || busyHere()) return;
  const r = await api('/api/board');
  if (!r.ok || BD.busy || busyHere() || route().p !== 'plancia') return;
  const was = JSON.stringify(D.board.canvases);
  D.board = r;
  if (JSON.stringify(r.canvases) !== was) render(true);
}, 5000);

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
  D.board = await api('/api/board');
  D.shopping = await api('/api/shopping');
  D.notes = await api('/api/notes');
  D.devices = await api('/api/sessions');
  const sc = await api('/api/screen');
  if (sc.ok) D.screen = sc;
  if (location.search) history.replaceState(null, '', '/' + location.hash);
  const r = route();
  if (location.hash && location.hash.slice(1) !== r.p + (r.k ? '/' + r.k : '')) history.replaceState(null, '', '#' + r.p + (r.k ? '/' + r.k : ''));
  const was = inside;
  chrome(true);
  render(was);
}

async function start() {
  const s = await api('/api/strings');
  if (s.ok) { T = s.strings; lang = s.lang; document.documentElement.lang = lang; }
  const h = await api('/api/hello');
  if (!h.ok) {
    app.replaceChildren(el('p', 'lead', t('panel_error', { name: '' })));
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

addEventListener('hashchange', () => {
  S.sorting = {};
  render();
  scrollTo(0, 0);
});
addEventListener('pagehide', () => {
  if (R.live) { R.live = false; fetch('/api/screen/stop', { method: 'POST', keepalive: true, credentials: 'same-origin', headers: { 'Content-Type': 'application/json' }, body: '{}' }); }
});
// Back to the front: the data again.
document.addEventListener('visibilitychange', () => {
  // Hidden: no frames for nobody (the device ends the session after a few seconds anyway).
  if (document.visibilityState === 'hidden') screenStop(true);
  if (document.visibilityState === 'visible' && inside && !busyHere()) main();
});

start();

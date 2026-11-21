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
  grid: 'M4 4h7v7H4z M13 4h7v4h-7z M13 10h7v10h-7z M4 13h7v7H4z', sliders: 'M4 7h10 M18 7h2 M4 17h2 M10 17h10 M16 9a2 2 0 100-4 2 2 0 000 4z M8 19a2 2 0 100-4 2 2 0 000 4z',
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

const D = { schema: null, values: {}, widgets: null, shopping: null, notes: null, devices: null, samples: null, screen: null };
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

function pages() { return (D.widgets && D.widgets.pages) || []; }
function page(kind) { return pages().find(p => p.kind === kind); }
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

function deviceScreen(kind, model, win) {
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
  const turns = on.length > 1 && on.some(p => p.kind === kind);
  if (kind) {
    const card = drawCard(kind, model, win, t('panel_empty_' + kind));
    // Its height as the screen gives it (the CSS of .slot): the weather's block, its columns,
    // or its rows (none: as two, for the empty line).
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
    for (const p of on) dots.append(el('i', p.kind === kind ? 'on' : null));
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

const SECTIONS = [['home', 'panel_tab_home', 'grid'], ['schermo', 'panel_tab_screen', 'screen'],
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
  const showing = D.widgets.showing && on.some(p => p.kind === D.widgets.showing) ? D.widgets.showing : null;
  const k = page(r.k) ? r.k : (showing || (on[0] || pages()[0] || {}).kind);
  const p = page(k);
  if (!p) return frag(pagehead(t('panel_tab_home')), el('p', 'lead', t('panel_error')));
  const { model, example } = shownModel(p);
  const n = model && model.windows ? model.windows.length : 1;
  const win = Math.min(S.win[k] || 0, Math.max(0, n - 1));

  const stage = el('div', 'stagecol');
  stage.append(deviceFrame(deviceScreen(k, model, win), t('panel_device_preview')).device);
  const only = el('div', 'cardonly');
  only.append(drawCard(k, model, win, t('panel_empty_' + k)));
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
    b.id = 'tile-' + q.kind;
    b.dataset.id = q.kind;
    b.setAttribute('aria-pressed', String(q.kind === k));
    b.setAttribute('aria-label', t('panel_pos_of', { w: q.label, a: i + 1, n: on.length }));
    b.title = t('panel_move_hint');
    b.append(el('span', 'n', i + 1));
    if (q.kind === showing) { const d = el('i', 'live'); d.title = t('panel_showing_now'); b.append(d); }
    b.append(ico(q.kind), el('b', null, q.label));
    b.addEventListener('click', () => { if (!b.dataset.dragged) go('home/' + q.kind); });
    strip.append(b);
  });
  if (!on.length) strip.append(el('p', 'none', t('panel_no_widgets')));
  stripSort(strip, ids => saveOrder(ids));
  const offs = pages().filter(q => !q.on);
  const avail = el('div', 'avail');
  avail.append(el('span', null, offs.length ? t('panel_can_add') : t('panel_all_on')));
  for (const q of offs) {
    const b = el('button', 'tile off');
    b.type = 'button';
    b.setAttribute('aria-pressed', String(q.kind === k));
    const plus = el('span', 'plus');
    plus.append(ico('plus'));
    b.append(ico(q.kind), el('b', null, q.label), plus);
    b.addEventListener('click', () => go('home/' + q.kind));
    avail.append(b);
  }
  stage.append(th, strip, avail);

  const ed = el('aside', 'editor');
  ed.setAttribute('aria-label', p.label);
  const eh = el('div', 'edhead');
  const ic = el('span', 'ic');
  ic.append(ico(k));
  const words = el('div', 'grow');
  const pos = on.findIndex(q => q.kind === k) + 1;
  words.append(el('h2', null, p.label), el('p', null, p.on ? t('panel_pos_in', { a: pos, n: on.length }) : t('panel_what_' + k)));
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
  (EDIT[k] || (() => {}))(ctl);
  ed.append(ctl);
  if (p.on && on.length > 1) {
    const f = el('div', 'edfoot');
    const ids = on.map(q => q.kind);
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
  const all = ids.concat(pages().filter(p => !ids.includes(p.kind)).map(p => p.kind));
  const r = await api('/api/widgets/order', { order: all });
  if (took(r)) toast(t('panel_order_saved'));
  else toast(errorText(r.error), true);
  render(true);
  if (quiet) say(t('panel_order_saved'));
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
  go('home/' + kind);
}

// Off: the page stays (now as a preview); Undo puts it back where it was.
async function removeFromHome(kind) {
  const was = pages().map(x => x.kind);
  const wasOn = onPages().map(x => x.kind);
  const label = page(kind).label;
  const r = await api('/api/set', { key: 'widgets', value: wasOn.filter(k => k !== kind) });
  if (!took(r)) { toast(errorText(r.error), true); render(true); return; }
  go('home/' + kind);
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
function chosen(kind) {
  const items = D.widgets[kind] || [];
  const sorting = !!S.sorting[kind];
  const ul = el('ul', 'group');
  if (!items.length) ul.append(el('li', 'row empty', t('panel_' + kind + '_empty')));
  const q = page(kind);
  const rows = ((q && q.model && q.model.windows) || []).flatMap(w => w.rows);
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
  return ul;
}

// «Reorder» for a widget's list: a quiet button next to its label.
function chosenHead(kind, key) {
  const h = el('div', 'turnhead');
  h.append(el('span', 'lab grow', t(key)));
  if ((D.widgets[kind] || []).length > 1) {
    const sorting = !!S.sorting[kind];
    h.append(button('btn quiet', t(sorting ? 'panel_finish' : 'panel_reorder'), () => { S.sorting[kind] = !sorting; render(true); }));
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
    c.append(chosenHead(kind, shownKey), chosen(kind));
    if (items.length < D.widgets.limits[kind]) {
      c.append(finder(what, addKey, h => items.some(x => x.ref === h.ref), adder(kind)));
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

const EDIT = {
  next: c => multiBlock(c, 'calendars'),
  shopping: c => {
    const more = el('p', 'fine');
    more.append(document.createTextNode(t('panel_lists_edit_in') + ' '), link('inl', 'spesa', t('panel_tab_shopping')), document.createTextNode('.'));
    multiBlock(c, 'lists', more);
  },
  weather: c => c.append(el('p', 'lead', t('panel_help_weather'))),
  quotes: listBlock('quotes', 'quote', 'panel_shown_quotes', 'panel_add_quote', 'panel_src_quotes'),
  cities: listBlock('cities', 'city', 'panel_shown_cities', 'panel_add_city', 'panel_src_cities'),
  sensors: c => {
    if (!D.widgets.ha) { c.append(el('p', 'lead', t('panel_no_ha'))); return; }
    listBlock('sensors', 'sensor', 'panel_shown_sensors', 'panel_add_sensor', 'panel_src_sensors')(c);
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
// The state is always written under the frame: ready (not shared), live, paused (a private
// page), busy (another device), lost, off (switched off on the device), stopped (on the device).

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
  if (!code || code === 'screen_ended') return 'ready';
  return 'lost';
}

async function screenFlush() {
  clearTimeout(R.flush);
  R.flush = 0;
  if (!R.queue.length || !R.live) { R.queue = []; return; }
  const events = R.queue.splice(0, 32);
  const r = await api('/api/screen/input', { events });
  if (!r.ok && r.error !== 'screen_hidden' && r.error !== 'slow_down') screenEnded(r.error);
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
  setState('live');
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
        setState('live');
      } catch (e) { /* a broken frame: the next one */ }
      continue;
    }
    let j = {};
    try { j = await r.json(); } catch (e) { j = { ok: false, error: 'network' }; }
    if (r.status === 401) { screenEnded(); login(); return; }
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
  const k = (D.widgets && D.widgets.showing) || (on[0] || {}).kind;
  const p = k && page(k);
  const fresh = deviceScreen(k, p ? shownModel(p).model : null, 0);
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

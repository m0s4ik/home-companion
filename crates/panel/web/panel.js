// SPDX-License-Identifier: GPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Andrea Mattera
// The browser panel's page: drawn from what the body says (its words, the settings' schema,
// the shopping lists, the devices). Text is only ever set as text (textContent), never as HTML.
'use strict';

const app = document.getElementById('app');
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

let toastTimer = 0;
function toast(text, bad) {
  let b = document.getElementById('toast');
  if (!b) { b = el('div'); b.id = 'toast'; b.setAttribute('role', 'status'); document.body.append(b); }
  b.textContent = text;
  b.className = bad ? 'show bad' : 'show';
  clearTimeout(toastTimer);
  toastTimer = setTimeout(() => { b.className = ''; }, 2200);
}

function header(withLogout) {
  const h = el('header');
  h.append(el('h1', null, name));
  if (withLogout) {
    const b = el('button', 'ghost', t('panel_logout'));
    b.addEventListener('click', async () => { await api('/api/logout', {}); login(); });
    h.append(b);
  }
  return h;
}

function note() {
  return el('p', 'note', t('panel_lan_note'));
}

function clear() {
  app.replaceChildren();
}

// ---- Without access: ask for it, or follow the QR code's answer.

function off() {
  clear();
  const c = el('section', 'card center');
  c.append(el('p', null, t('panel_off')));
  app.append(header(false), c, note());
}

let polling = 0;

function login(message) {
  clearInterval(polling);
  clear();
  const c = el('section', 'card center');
  if (message) c.append(el('p', 'warn', message));
  const b = el('button', 'primary', t('panel_ask'));
  b.addEventListener('click', ask);
  c.append(b, el('p', 'hint', t('panel_ask_hint')));
  app.append(header(false), c, note());
}

async function ask() {
  const r = await api('/api/pair', {});
  if (!r.ok) { login(errorText(r.error)); return; }
  clear();
  const c = el('section', 'card center');
  c.append(el('p', null, t('panel_check_code')), el('p', 'code', r.code), el('p', 'hint', t('panel_waiting')));
  app.append(header(false), c, note());
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

// ---- With access: settings, shopping, devices.

function control(item, value, onChange) {
  if (item.kind === 'bool') {
    const i = el('input');
    i.type = 'checkbox';
    i.setAttribute('role', 'switch');
    i.checked = value === true;
    i.addEventListener('change', () => onChange(i.checked));
    return i;
  }
  if (item.kind === 'choice' || (item.kind === 'int' && item.max - item.min <= 24)) {
    const s = el('select');
    const opts = item.kind === 'choice' ? item.options
      : Array.from({ length: (item.max - item.min) / item.step + 1 }, (_, k) => {
        const v = item.min + k * item.step;
        return { value: v, label: String(v).padStart(2, '0') };
      });
    for (const o of opts) {
      const op = el('option', null, o.label);
      op.value = String(o.value);
      if (String(o.value) === String(value)) op.selected = true;
      s.append(op);
    }
    s.addEventListener('change', () => onChange(item.kind === 'int' ? Number(s.value) : s.value));
    return s;
  }
  if (item.kind === 'int') {
    const w = el('span', 'range');
    const i = el('input');
    i.type = 'range';
    i.min = item.min; i.max = item.max; i.step = item.step;
    i.value = value;
    const out = el('output', null, value);
    i.addEventListener('input', () => { out.textContent = i.value; });
    i.addEventListener('change', () => onChange(Number(i.value)));
    w.append(i, out);
    return w;
  }
  // multi: for the lists and calendars an empty choice is all of them.
  const box = el('div', 'multi');
  const all = Array.isArray(value) && value.length === 0 && item.key !== 'widgets';
  for (const o of item.options) {
    const l = el('label');
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
  if (item.options.length === 0) box.append(el('span', 'hint', '–'));
  return box;
}

function settingsGroup(g, values) {
  const s = el('section', 'card');
  if (g.label) s.append(el('h2', null, g.label));
  for (const item of g.items) {
    const row = el('div', item.kind === 'multi' ? 'row column' : 'row');
    const id = 'set-' + item.key;
    const label = el('label', null, item.label);
    label.htmlFor = id;
    let c;
    const changed = async v => {
      const r = await api('/api/set', { key: item.key, value: v });
      if (r.values) Object.assign(values, r.values);
      if (r.ok) {
        toast(t('panel_saved'));
        if (item.key === 'language') location.reload();
      } else {
        toast(errorText(r.error), true);
        const fresh = control(item, values[item.key], changed);
        fresh.id = id;
        c.replaceWith(fresh);
        c = fresh;
      }
    };
    c = control(item, values[item.key], changed);
    c.id = id;
    row.append(label, c);
    s.append(row);
  }
  return s;
}

function shoppingSection(data) {
  const s = el('section', 'card');
  s.id = 'shopping';
  s.append(el('h2', null, t('panel_section_shopping')));
  const lists = (data && data.lists) || [];
  if (lists.length === 0) s.append(el('p', 'hint', t('panel_shopping_none')));
  for (const l of lists) {
    if (lists.length > 1) s.append(el('h3', null, l.name));
    const ul = el('ul', 'items');
    if (l.items.length === 0) ul.append(el('li', 'hint', t('panel_shopping_empty')));
    for (const it of l.items) {
      const li = el('li');
      li.append(el('span', 'grow', it.text));
      for (const [op, word] of [['done', 'panel_done'], ['remove', 'panel_remove']]) {
        const b = el('button', op === 'done' ? 'small' : 'small ghost', t(word));
        b.addEventListener('click', async () => {
          // Remove asks twice, as Revoke does: a second click within 3 s.
          if (op === 'remove' && !arm(b, t(word))) return;
          b.disabled = true;
          const r = await api('/api/shopping/' + op, { list: l.entity, uid: it.uid });
          if (r.ok) s.replaceWith(shoppingSection(r));
          else { b.disabled = false; toast(errorText(r.error), true); }
        });
        li.append(b);
      }
      ul.append(li);
    }
    const f = el('form', 'add');
    const i = el('input');
    i.type = 'text';
    i.maxLength = 200;
    i.required = true;
    i.placeholder = t('panel_shopping_placeholder');
    i.setAttribute('aria-label', t('panel_shopping_placeholder'));
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
      shown.append(el('span', 'grow', text));
      ul.append(shown);
      // Home Assistant sends the list again in a moment.
      setTimeout(async () => {
        const fresh = await api('/api/shopping');
        if (fresh.ok) document.getElementById('shopping').replaceWith(shoppingSection(fresh));
      }, 1500);
    });
    s.append(ul, f);
  }
  return s;
}

// The first click arms a button ("Remove?") for 3 s; true on the second.
function arm(b, label) {
  if (b.classList.contains('armed')) return true;
  b.classList.add('armed');
  b.textContent = label + '?';
  setTimeout(() => { b.classList.remove('armed'); b.textContent = label; }, 3000);
  return false;
}

// ---- The home's widget: its pages in their order, the markets, the other cities.

// A list of chosen items: name and detail, arrows to move, a remove that asks twice.
function chosenList(kind, items, onChange) {
  const ul = el('ul', 'items');
  if (items.length === 0) ul.append(el('li', 'hint', t('panel_' + kind + '_empty')));
  items.forEach((it, i) => {
    const li = el('li');
    const who = el('div', 'grow');
    who.append(el('div', null, it.name), el('div', 'hint', it.detail));
    li.append(who);
    for (const [label, to, sign] of [['panel_up', i - 1, '▲'], ['panel_down', i + 1, '▼']]) {
      const b = el('button', 'small ghost', sign);
      b.setAttribute('aria-label', t(label));
      b.disabled = to < 0 || to >= items.length;
      b.addEventListener('click', async () => onChange(await api('/api/' + kind + '/move', { ref: it.ref, to })));
      li.append(b);
    }
    const x = el('button', 'small ghost', t('panel_remove'));
    x.addEventListener('click', async () => {
      if (!arm(x, t('panel_remove'))) return;
      onChange(await api('/api/' + kind + '/remove', { ref: it.ref }));
    });
    li.append(x);
    ul.append(li);
  });
  return ul;
}

// A search box: the body searches, the page asks how it went every half second (12 s at most);
// each hit is a button that adds it.
function searchBox(what, kind, onChange) {
  const box = el('div');
  const f = el('form', 'add');
  const i = el('input');
  i.type = 'search';
  i.maxLength = 40;
  i.required = true;
  i.placeholder = t('panel_search_' + what);
  i.setAttribute('aria-label', t('panel_search_' + what));
  const b = el('button', 'primary', t('panel_search'));
  f.append(i, b);
  const out = el('ul', 'items hits');
  out.setAttribute('aria-live', 'polite');
  f.addEventListener('submit', async ev => {
    ev.preventDefault();
    const q = i.value.trim();
    if (!q) return;
    b.disabled = true;
    out.replaceChildren(el('li', 'hint', t('panel_searching')));
    const r = await api('/api/search', { what, q });
    if (!r.ok) { b.disabled = false; out.replaceChildren(el('li', 'hint', errorText(r.error))); return; }
    let s = { status: 'waiting' };
    for (let n = 0; n < 24 && s.ok !== false && s.status === 'waiting'; n++) {
      await new Promise(res => setTimeout(res, 500));
      s = await api('/api/search/' + encodeURIComponent(r.id));
    }
    b.disabled = false;
    if (s.status !== 'done') { out.replaceChildren(el('li', 'hint', t('panel_search_failed'))); return; }
    out.replaceChildren();
    if (s.hits.length === 0) out.append(el('li', 'hint', t('panel_search_none')));
    for (const h of s.hits) {
      const li = el('li');
      const add = el('button', 'hit');
      add.append(el('span', null, h.name), el('span', 'hint', ' · ' + h.detail));
      add.addEventListener('click', async () => {
        add.disabled = true;
        const a = await api(kind === 'player' ? '/api/player' : '/api/' + kind + '/add', { search: r.id, ref: h.ref });
        if (a.ok) { i.value = ''; out.replaceChildren(); toast(t('panel_saved')); }
        else { add.disabled = false; toast(errorText(a.error), true); }
        onChange(a);
      });
      li.append(add);
      out.append(li);
    }
  });
  box.append(f, out);
  return box;
}

// The widget's card: every page in the turn's order with its switch and its arrows, then the
// group's other settings (the interval).
function widgetsSection(w, g, values) {
  const s = el('section', 'card');
  s.id = 'widgets';
  s.append(el('h2', null, g.label), el('p', 'hint', t('panel_widgets_pages')));
  const ul = el('ul', 'items pages');
  const redraw = r => { if (r && r.ok) { Object.assign(w, r); s.replaceWith(widgetsSection(w, g, values)); } else if (r) toast(errorText(r.error), true); };
  w.order.forEach((p, i) => {
    const li = el('li');
    const c = el('input');
    c.type = 'checkbox';
    c.setAttribute('role', 'switch');
    c.id = 'page-' + p.kind;
    c.checked = p.on;
    c.addEventListener('change', async () => {
      p.on = c.checked;
      const on = w.order.filter(x => x.on).map(x => x.kind);
      const r = await api('/api/set', { key: 'widgets', value: on });
      if (r.ok) toast(t('panel_saved')); else { toast(errorText(r.error), true); c.checked = !c.checked; p.on = c.checked; }
    });
    const l = el('label', 'grow', p.label);
    l.htmlFor = c.id;
    li.append(c, l);
    for (const [label, to, sign] of [['panel_up', i - 1, '▲'], ['panel_down', i + 1, '▼']]) {
      const b = el('button', 'small ghost', sign);
      b.setAttribute('aria-label', t(label));
      b.disabled = to < 0 || to >= w.order.length;
      b.addEventListener('click', async () => {
        const order = w.order.map(x => x.kind);
        order.splice(to, 0, order.splice(i, 1)[0]);
        redraw(await api('/api/widgets/order', { order }));
      });
      li.append(b);
    }
    ul.append(li);
  });
  s.append(ul);
  const rest = settingsGroup({ label: '', items: g.items.filter(x => x.key === 'widgets_every') }, values);
  s.append(...rest.childNodes);
  return s;
}

// Home Assistant's side: the sensors (a search of its catalogue), the player, the people, what
// to check; one sentence on who sees them.
function haSection(w, g, values) {
  const s = el('section', 'card');
  s.id = 'ha';
  const redraw = r => { if (r && r.ok) { Object.assign(w, r); s.replaceWith(haSection(w, g, values)); } else if (r) toast(errorText(r.error), true); };
  if (!w.ha) { s.append(el('h2', null, t('panel_sensors')), el('p', 'hint', t('panel_no_ha'))); return s; }
  s.append(el('h2', null, t('panel_sensors')), chosenList('sensors', w.sensors || [], redraw));
  if ((w.sensors || []).length < w.limits.sensors) s.append(searchBox('sensor', 'sensors', redraw));
  s.append(el('h3', null, t('panel_player')));
  const pl = el('ul', 'items');
  const cur = el('li');
  cur.append(el('span', 'grow', w.player ? w.player.name : t('panel_player_none')));
  if (w.player) {
    const x = el('button', 'small ghost', t('panel_remove'));
    x.addEventListener('click', async () => redraw(await api('/api/player', { ref: '' })));
    cur.append(x);
  }
  pl.append(cur);
  s.append(pl, searchBox('player', 'player', redraw));
  s.append(el('h3', null, t('panel_people')));
  const box = el('div', 'multi');
  for (const p of w.people.all) {
    const l = el('label');
    const c = el('input');
    c.type = 'checkbox';
    c.value = p.ref;
    c.checked = w.people.chosen.includes(p.ref);
    c.addEventListener('change', async () => {
      redraw(await api('/api/people', { refs: Array.from(box.querySelectorAll('input:checked')).map(x => x.value) }));
    });
    l.append(c, el('span', null, p.name));
    box.append(l);
  }
  s.append(box, el('p', 'hint', t('panel_people_hint')));
  s.append(el('h3', null, t('panel_check')));
  const checks = settingsGroup({ label: '', items: g.items.filter(x => x.key.startsWith('check_')) }, values);
  s.append(...checks.childNodes, el('p', 'hint', t('panel_ha_privacy')));
  return s;
}

// The markets (and the coins' currency) or the other cities, with the sentences about what
// leaves the house.
function feedSection(kind, w, g, values) {
  const s = el('section', 'card');
  s.id = kind;
  s.append(el('h2', null, t(kind === 'quotes' ? 'panel_markets' : 'panel_cities')));
  const redraw = r => { if (r && r.ok) { Object.assign(w, r); s.replaceWith(feedSection(kind, w, g, values)); } else if (r) toast(errorText(r.error), true); };
  s.append(chosenList(kind, w[kind] || [], redraw));
  const full = (w[kind] || []).length >= w.limits[kind];
  if (full) s.append(el('p', 'hint', t('panel_list_full')));
  else s.append(searchBox(kind === 'quotes' ? 'quote' : 'city', kind, redraw));
  if (kind === 'quotes') {
    const cur = settingsGroup({ label: '', items: g.items.filter(x => x.key === 'widgets_currency') }, values);
    s.append(...cur.childNodes);
  }
  // Once, under the second of the two cards.
  if (kind === 'cities') s.append(el('p', 'hint', t('panel_feeds_privacy')), el('p', 'hint small', t('panel_feeds_credit')));
  return s;
}

// The board: a note per line, Edit puts it back in the box, Remove asks twice; 280 at most.
function notesSection(data) {
  const s = el('section', 'card');
  s.id = 'notes';
  s.append(el('h2', null, t('panel_notes')));
  if (!data || !data.ok) { s.append(el('p', 'hint', t('panel_no_brain'))); return s; }
  const reload = async () => {
    await new Promise(res => setTimeout(res, 700));
    const fresh = await api('/api/notes');
    document.getElementById('notes').replaceWith(notesSection(fresh));
  };
  const ul = el('ul', 'items');
  if (data.notes.length === 0) ul.append(el('li', 'hint', t('panel_notes_empty')));
  let editing = null;
  const f = el('form', 'add column');
  const box = el('textarea');
  box.maxLength = 280;
  box.rows = 2;
  box.required = true;
  box.placeholder = t('panel_notes_placeholder');
  box.setAttribute('aria-label', t('panel_notes_placeholder'));
  const count = el('span', 'hint', '0 / 280');
  box.addEventListener('input', () => { count.textContent = box.value.length + ' / 280'; });
  const b = el('button', 'primary', t('panel_add'));
  for (const n of data.notes) {
    const li = el('li');
    li.append(el('span', 'grow', n.text));
    const e = el('button', 'small ghost', t('panel_edit'));
    e.addEventListener('click', () => {
      editing = n.id; box.value = n.text; count.textContent = box.value.length + ' / 280';
      b.textContent = t('panel_save'); box.focus();
    });
    const x = el('button', 'small ghost', t('panel_remove'));
    x.addEventListener('click', async () => {
      if (!arm(x, t('panel_remove'))) return;
      const r = await api('/api/notes/remove', { id: n.id });
      if (!r.ok) toast(errorText(r.error), true); else reload();
    });
    li.append(e, x);
    ul.append(li);
  }
  const row = el('div', 'add');
  row.append(count, b);
  f.append(box, row);
  f.addEventListener('submit', async ev => {
    ev.preventDefault();
    const text = box.value.trim();
    if (!text) return;
    b.disabled = true;
    const r = editing === null ? await api('/api/notes/add', { text })
      : await api('/api/notes/edit', { id: editing, text });
    b.disabled = false;
    if (!r.ok) { toast(errorText(r.error), true); return; }
    toast(t('panel_saved'));
    reload();
  });
  s.append(ul, f, el('p', 'hint', t('panel_notes_hint')));
  return s;
}

function when(sec) {
  try {
    return new Date(sec * 1000).toLocaleString(lang, { day: 'numeric', month: 'short', hour: '2-digit', minute: '2-digit' });
  } catch (e) {
    return '';
  }
}

function devicesSection(data) {
  const s = el('section', 'card');
  s.id = 'devices';
  s.append(el('h2', null, t('panel_section_devices')));
  const ul = el('ul', 'items');
  for (const d of (data && data.sessions) || []) {
    const li = el('li');
    const who = el('div', 'grow');
    who.append(el('div', null, t('panel_device_line', { browser: d.browser, os: d.os })));
    who.append(el('div', 'hint', d.peer + ' · ' + when(d.last_used)));
    li.append(who);
    if (d.current) {
      li.append(el('span', 'badge', t('panel_devices_this')));
    } else {
      const b = el('button', 'small ghost', t('panel_devices_revoke'));
      b.addEventListener('click', async () => {
        if (!arm(b, t('panel_devices_revoke'))) return;
        const r = await api('/api/sessions/revoke', { id: d.id });
        if (!r.ok) toast(errorText(r.error), true);
        const fresh = await api('/api/sessions');
        if (fresh.ok) s.replaceWith(devicesSection(fresh));
      });
      li.append(b);
    }
    ul.append(li);
  }
  s.append(ul);
  return s;
}

async function main() {
  // One after the other: the panel takes few connections from one address at once.
  const schema = await api('/api/schema');
  const state = await api('/api/state');
  const shopping = await api('/api/shopping');
  const widgets = await api('/api/widgets');
  const notes = await api('/api/notes');
  const devices = await api('/api/sessions');
  if (!schema.ok || !state.ok) return;
  if (location.search) history.replaceState(null, '', '/');
  clear();
  const values = state.values || {};
  const left = el('div', 'col');
  const right = el('div', 'col');
  const groups = schema.groups || [];
  const wg = groups.find(g => g.key === 'widgets');
  if (wg && widgets.ok) {
    left.append(widgetsSection(widgets, wg, values), feedSection('quotes', widgets, wg, values),
      feedSection('cities', widgets, wg, values), haSection(widgets, wg, values));
  } else if (wg) {
    left.append(settingsGroup(wg, values));
  }
  left.append(notesSection(notes), shoppingSection(shopping));
  const settings = el('div');
  settings.append(el('h2', 'section', t('panel_section_settings')));
  for (const g of groups.filter(g => g.key !== 'widgets')) settings.append(settingsGroup(g, values));
  right.append(settings, devicesSection(devices));
  const grid = el('div', 'grid');
  grid.append(left, right);
  app.append(header(true), grid, note());
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
  for (const k of Object.keys(T)) T[k] = T[k].split('{name}').join(name);
  const qr = new URLSearchParams(location.search).get('qr');
  if (!h.on) off();
  else if (h.session) main();
  else login(qr ? errorText(qr) : '');
}

document.addEventListener('visibilitychange', () => {
  if (document.visibilityState === 'visible' && document.querySelector('.grid')) main();
});

start();

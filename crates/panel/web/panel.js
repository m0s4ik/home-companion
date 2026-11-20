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
  s.append(el('h2', null, g.label));
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
  const devices = await api('/api/sessions');
  if (!schema.ok || !state.ok) return;
  if (location.search) history.replaceState(null, '', '/');
  clear();
  const values = state.values || {};
  const left = el('div', 'col');
  const right = el('div', 'col');
  const groups = schema.groups || [];
  const widgets = groups.filter(g => g.key === 'widgets');
  for (const g of widgets) left.append(settingsGroup(g, values));
  left.append(shoppingSection(shopping));
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

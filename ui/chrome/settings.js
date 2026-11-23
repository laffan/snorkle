// Browser chrome (see core.js): Settings: Blocked, AI connections.
'use strict';

// ---- Settings → Blocked ---------------------------------------------------
//
// The list lives in the core (`snorkle_core::blocklist`), which keeps
// matching pages from loading. Adding a pattern is immediate. Changing or
// removing one first asks for a passage, typed out in full each time;
// the core refuses the change unless it receives the passage exactly.
let blocked = { patterns: [], message: '', default_message: '', passage: '' };

function loadBlocked() {
  T.core.invoke('get_blocked').then((b) => { blocked = b; renderBlocked(); }).catch((e) => toast(String(e)));
}

function renderBlocked() {
  const list = $('blocked-list');
  list.textContent = '';
  if (!blocked.patterns.length) list.append(el('li', 'empty muted small', 'Nothing is blocked.'));
  for (const p of blocked.patterns) {
    const li = el('li');
    li.append(el('span', 'pattern mono-inline', p));
    const edit = el('button', 'button blocked-edit', 'Edit…');
    edit.setAttribute('aria-label', `Edit or remove ${p}`);
    edit.addEventListener('click', () => openUnblock(p));
    li.append(edit);
    list.append(li);
  }
  const m = $('blocked-message');
  if (document.activeElement !== m) m.value = blocked.message;
  m.placeholder = blocked.default_message;
}

function addBlocked() {
  const input = $('blocked-input');
  const pattern = input.value.trim();
  if (!pattern) return;
  T.core.invoke('add_blocked', { pattern })
    .then((b) => { blocked = b; input.value = ''; $('blocked-msg').textContent = ''; renderBlocked(); })
    .catch((e) => { $('blocked-msg').textContent = String(e); });
}
$('blocked-add').addEventListener('click', addBlocked);
$('blocked-input').addEventListener('keydown', (ev) => { if (ev.key === 'Enter') { ev.preventDefault(); addBlocked(); } });
$('blocked-input').addEventListener('input', () => { $('blocked-msg').textContent = ''; });
$('blocked-message').addEventListener('change', () => {
  T.core.invoke('set_blocked_message', { message: $('blocked-message').value })
    .then((b) => { blocked = b; renderBlocked(); $('blocked-message-state').textContent = 'Saved.'; })
    .catch((e) => { $('blocked-message-state').textContent = String(e); });
});
$('blocked-message').addEventListener('input', () => { $('blocked-message-state').textContent = ''; });

// The passage modal. What has been typed is kept here, not in the
// textarea: every change is checked against it, and only one character
// added at the end or the last one deleted is accepted, so pasting,
// dropping, dictation, predictive text and moving the caret change
// nothing.
let unblock = null; // { pattern, typed, passage }
const UNBLOCK_ALLOWED = new Set(['insertText', 'deleteContentBackward']);

function openUnblock(pattern) {
  unblock = { pattern, typed: '', passage: blocked.passage };
  $('unblock-pattern').textContent = pattern;
  $('unblock-step-type').hidden = false;
  $('unblock-step-edit').hidden = true;
  $('unblock-msg').textContent = '';
  const text = $('unblock-text');
  text.textContent = '';
  for (const ch of unblock.passage) text.append(el('span', '', ch));
  const input = $('unblock-input');
  input.value = '';
  renderUnblock();
  $('unblock-modal').hidden = false;
  input.focus();
}

function closeUnblock() {
  unblock = null;
  $('unblock-input').value = '';
  $('unblock-modal').hidden = true;
}

function renderUnblock() {
  const { typed, passage } = unblock;
  const spans = $('unblock-text').children;
  let correct = 0;
  while (correct < typed.length && typed[correct] === passage[correct]) correct++;
  for (let i = 0; i < spans.length; i++) {
    const c = i < correct ? 'ok' : i < typed.length ? 'bad' : i === typed.length ? 'cur' : '';
    if (spans[i].className !== c) spans[i].className = c;
  }
  const done = typed === passage;
  $('unblock-go').disabled = !done;
  $('unblock-count').textContent = done
    ? 'Done.'
    : correct < typed.length ? 'A letter is wrong: delete back to it.' : `${passage.length - typed.length} letters to go`;
}

const unblockInput = $('unblock-input');
unblockInput.addEventListener('beforeinput', (ev) => {
  if (!unblock) { ev.preventDefault(); return; }
  const one = ev.inputType === 'insertText' && ev.data && [...ev.data].length === 1;
  if (!UNBLOCK_ALLOWED.has(ev.inputType) || (ev.inputType === 'insertText' && !one)) ev.preventDefault();
});
unblockInput.addEventListener('input', () => {
  if (!unblock) return;
  const v = unblockInput.value;
  const t = unblock.typed;
  if (v.length === t.length + 1 && v.startsWith(t) && t.length < unblock.passage.length && v[t.length] !== '\n') {
    unblock.typed = v;
  } else if (v.length === t.length - 1 && t.startsWith(v)) {
    unblock.typed = v;
  }
  if (unblockInput.value !== unblock.typed) unblockInput.value = unblock.typed;
  unblockInput.setSelectionRange(unblock.typed.length, unblock.typed.length);
  renderUnblock();
});
for (const type of ['paste', 'drop', 'dragover']) unblockInput.addEventListener(type, (ev) => ev.preventDefault());
unblockInput.addEventListener('keydown', (ev) => {
  if (ev.key === 'Enter') {
    ev.preventDefault();
    if (!$('unblock-go').disabled) $('unblock-go').click();
  } else if (ev.key === 'Escape') {
    ev.preventDefault();
    ev.stopPropagation();
    closeUnblock();
  } else if (['ArrowLeft', 'ArrowRight', 'ArrowUp', 'ArrowDown', 'Home', 'End'].includes(ev.key)) {
    ev.preventDefault();
  }
});
// The caret always stays at the end.
const keepCaret = () => { if (unblock) unblockInput.setSelectionRange(unblock.typed.length, unblock.typed.length); };
unblockInput.addEventListener('select', keepCaret);
unblockInput.addEventListener('click', keepCaret);
$('unblock-text').addEventListener('click', () => unblockInput.focus());
$('unblock-cancel').addEventListener('click', closeUnblock);
$('unblock-cancel2').addEventListener('click', closeUnblock);
$('unblock-go').addEventListener('click', () => {
  if (!unblock || unblock.typed !== unblock.passage) return;
  $('unblock-step-type').hidden = true;
  $('unblock-step-edit').hidden = false;
  const edit = $('unblock-edit');
  edit.value = unblock.pattern;
  edit.focus();
});
function finishUnblock(replacement) {
  if (!unblock) return;
  const args = { pattern: unblock.pattern, replacement, passage: unblock.typed };
  T.core.invoke('edit_blocked', args)
    .then((b) => { blocked = b; closeUnblock(); renderBlocked(); })
    .catch((e) => { $('unblock-msg').textContent = String(e); });
}
$('unblock-save').addEventListener('click', () => {
  const v = $('unblock-edit').value.trim();
  if (!v) { $('unblock-msg').textContent = 'Enter a pattern, or choose Remove.'; return; }
  finishUnblock(v);
});
$('unblock-edit').addEventListener('keydown', (ev) => {
  if (ev.key === 'Enter') { ev.preventDefault(); $('unblock-save').click(); }
  else if (ev.key === 'Escape') { ev.preventDefault(); ev.stopPropagation(); closeUnblock(); }
});
$('unblock-remove').addEventListener('click', () => finishUnblock(null));

// ---- AI connections -----------------------------------------------------
//
// Keys go to the Keychain through Rust and never come back; the chrome
// only learns whether one is stored (Authentication). Each feature that
// uses AI has its own service and model (Uses); both are UI preferences
// (localStorage), as are the model lists Test fetched.

const AI_DEFAULT_MODEL = { anthropic: 'claude-opus-5-5', openai: '' };
const AI_LABEL = { anthropic: 'Anthropic', openai: 'OpenAI' };
const AI_USES = [
  { id: 'scripts', label: 'Site script helper', note: 'Drafts CSS and JavaScript in Site Scripts.' },
  { id: 'ask', label: 'Ask Selection', note: 'The page chat and “Ask AI” on selected text.' },
  { id: 'summary', label: 'Summary View', note: 'Summarizes the page in the summary view.' },
];
let aiConfigured = { anthropic: false, openai: false };
// The summary view replaced the headers view (2026-11-16): it starts with
// the service and model chosen for Header View Parsing.
if (!pref('use.summary.provider') && pref('use.headers.provider')) {
  setPref('use.summary.provider', pref('use.headers.provider'));
  setPref('use.summary.model', pref('use.headers.model') || '');
}

function aiModels(p) {
  try { return JSON.parse(pref('aiModels.' + p) || '[]'); } catch (e) { return []; }
}
function connectedProviders() {
  return ['anthropic', 'openai'].filter((p) => aiConfigured[p]);
}
// The service and model a use is set to: its saved choice if that
// service is still connected, else the first connected one with its
// first listed model (or the legacy per-provider model).
function useChoice(id) {
  const ps = connectedProviders();
  if (!ps.length) return null;
  const saved = pref(`use.${id}.provider`);
  const p = ps.includes(saved) ? saved : ps[0];
  const models = aiModels(p);
  const m = pref(`use.${id}.model`);
  const model = (p === saved && m && (!models.length || models.some((x) => x.id === m)) ? m : null)
    || pref('aiModel.' + p) || (models[0] && models[0].id) || AI_DEFAULT_MODEL[p];
  if (!model) return null;
  const hit = models.find((x) => x.id === model);
  return { p, id: model, name: hit ? hit.name : model };
}
function setUse(id, p, model) {
  setPref(`use.${id}.provider`, p);
  setPref(`use.${id}.model`, model);
}

function loadAi() {
  return T.core.invoke('ai_status').then((st) => { aiConfigured = st.configured; renderAi(); }).catch(() => {});
}

function renderAi() {
  document.querySelectorAll('.ai-provider').forEach((box) => {
    const p = box.dataset.provider;
    const on = !!aiConfigured[p];
    const n = aiModels(p).length;
    box.querySelector('.ai-state').textContent = on ? (n ? `Key saved · ${n} models` : 'Key saved') : 'Not connected';
    box.querySelector('.ai-key').placeholder = on ? 'Replace the saved key' : `${AI_LABEL[p]} API key`;
    box.querySelector('.ai-test').disabled = !on;
    box.querySelector('.ai-remove').disabled = !on;
  });
  renderUses();
  $('se-ai').hidden = !useChoice('scripts');
}

// Uses: one row per feature, with a service menu and a model menu.
function renderUses() {
  const box = $('ai-uses');
  box.replaceChildren();
  const ps = connectedProviders();
  for (const u of AI_USES) {
    const row = el('div', 'ai-use');
    const label = el('div', 'ai-use-label');
    label.append(el('div', 'option-title', u.label), el('div', 'muted small', u.note));
    const svc = el('select');
    svc.setAttribute('aria-label', `${u.label}: service`);
    const model = el('select');
    model.setAttribute('aria-label', `${u.label}: model`);
    const choice = useChoice(u.id);
    if (!ps.length) {
      svc.appendChild(el('option', null, 'Connect a service above'));
      svc.disabled = model.disabled = true;
    } else {
      for (const p of ps) { const o = el('option', null, AI_LABEL[p]); o.value = p; svc.appendChild(o); }
      svc.value = choice ? choice.p : ps[0];
      const list = aiModels(svc.value);
      if (!list.length) {
        const o = el('option', null, choice ? choice.id : 'Test the key to list models');
        o.value = choice ? choice.id : '';
        model.appendChild(o);
      }
      for (const m of list) {
        const o = el('option', null, m.name === m.id ? m.id : `${m.name} (${m.id})`);
        o.value = m.id;
        model.appendChild(o);
      }
      if (choice) model.value = choice.id;
    }
    svc.addEventListener('change', () => {
      const list = aiModels(svc.value);
      setUse(u.id, svc.value, (list[0] && list[0].id) || AI_DEFAULT_MODEL[svc.value] || '');
      renderAi();
      renderChatModels();
    });
    model.addEventListener('change', () => { setUse(u.id, svc.value, model.value); renderChatModels(); });
    const pickers = el('div', 'ai-use-pickers');
    pickers.append(svc, model);
    row.append(label, pickers);
    box.appendChild(row);
  }
}

document.querySelectorAll('.ai-provider').forEach((box) => {
  const p = box.dataset.provider;
  const msg = (t) => { box.querySelector('.ai-msg').textContent = t; };
  box.querySelector('.ai-save').addEventListener('click', () => {
    const input = box.querySelector('.ai-key');
    T.core.invoke('ai_set_key', { provider: p, key: input.value })
      .then((st) => { input.value = ''; aiConfigured = st.configured; renderAi(); msg('Saved. Testing…'); return test(); })
      .catch((e) => msg(String(e)));
  });
  const test = () => {
    msg('Testing…');
    return T.core.invoke('ai_test', { provider: p })
      .then((models) => {
        setPref('aiModels.' + p, JSON.stringify(models));
        renderAi();
        msg(`Connected. ${models.length} model${models.length === 1 ? '' : 's'} available; choose them under Uses.`);
      })
      .catch((e) => msg(String(e)));
  };
  box.querySelector('.ai-test').addEventListener('click', test);
  box.querySelector('.ai-remove').addEventListener('click', () => {
    T.core.invoke('ai_delete_key', { provider: p })
      .then((st) => { aiConfigured = st.configured; renderAi(); msg('Key removed from this device.'); })
      .catch((e) => msg(String(e)));
  });
});

// Usage: AI requests grouped by local date, with estimated cost.
function money(micros) {
  const d = micros / 1e6;
  return '$' + (d > 0 && d < 0.01 ? d.toFixed(4) : d.toFixed(2));
}
function loadUsage() {
  T.core.invoke('ai_usage').then((events) => {
    const days = new Map();
    for (const e of events) {
      const dt = new Date(e.at_ms);
      const key = `${dt.getFullYear()}-${String(dt.getMonth() + 1).padStart(2, '0')}-${String(dt.getDate()).padStart(2, '0')}`;
      const d = days.get(key) || { label: dt.toLocaleDateString(undefined, { weekday: 'short', day: 'numeric', month: 'short', year: 'numeric' }), n: 0, input: 0, output: 0, cost: 0, unpriced: false };
      d.n += 1; d.input += e.input; d.output += e.output;
      if (e.cost_micros === null || e.cost_micros === undefined) d.unpriced = true; else d.cost += e.cost_micros;
      days.set(key, d);
    }
    const body = $('ai-usage-table').tBodies[0];
    const foot = $('ai-usage-table').tFoot;
    body.replaceChildren();
    foot.replaceChildren();
    const fmt = (n) => n.toLocaleString();
    const row = (cells, cls) => { const tr = el('tr', cls); for (const c of cells) tr.appendChild(el('td', null, c)); return tr; };
    const keys = [...days.keys()].sort().reverse();
    if (!keys.length) { body.appendChild(row(['No AI requests yet.', '', '', '', ''])); return; }
    const total = { n: 0, input: 0, output: 0, cost: 0, unpriced: false };
    for (const k of keys) {
      const d = days.get(k);
      body.appendChild(row([d.label, fmt(d.n), fmt(d.input), fmt(d.output), (d.unpriced ? '≥ ' : '') + money(d.cost)]));
      total.n += d.n; total.input += d.input; total.output += d.output; total.cost += d.cost; total.unpriced ||= d.unpriced;
    }
    foot.appendChild(row(['Total', fmt(total.n), fmt(total.input), fmt(total.output), (total.unpriced ? '≥ ' : '') + money(total.cost)]));
  }).catch(() => {});
}
$('ai-usage-reset').addEventListener('click', () => {
  const b = $('ai-usage-reset');
  if (!b.classList.contains('armed')) {
    b.classList.add('armed'); b.textContent = 'Tap again to reset';
    setTimeout(() => { b.classList.remove('armed'); b.textContent = 'Reset'; }, 4000);
    return;
  }
  b.classList.remove('armed'); b.textContent = 'Reset';
  invoke('ai_reset_usage').then(loadUsage);
});

// Site script helper: drafts CSS/JS into the editor for the user to review.
$('se-ai-go').addEventListener('click', () => {
  const use = useChoice('scripts');
  const out = $('se-ai-msg');
  if (!use) { out.textContent = 'Connect an AI service in Settings → AI.'; return; }
  const p = use.p;
  const model = use.id;
  const btn = $('se-ai-go');
  btn.disabled = true;
  out.textContent = `Asking ${AI_LABEL[p]}…`;
  T.core.invoke('ai_write_script', {
    provider: p, model, domain: $('se-domain').value, css: $('se-css').value, js: $('se-js').value, prompt: $('se-ai-prompt').value,
  })
    .then((d) => {
      loadUsage();
      $('se-css').value = d.css;
      $('se-js').value = d.js;
      out.textContent = (d.note ? d.note + ' ' : '') + 'Review the code, then Save.';
    })
    .catch((e) => { out.textContent = String(e); })
    .finally(() => { btn.disabled = false; });
});

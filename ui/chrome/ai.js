// Browser chrome (see core.js): The AI sidebar: summary and chat.
'use strict';

// ---- summary view ---------------------------------------------------------
//
// Per tab: the button in the URL bar opens a panel docked right of the
// page with an AI summary of the page as bullet points. Opening it on a
// page without a summary asks for one at once; when the tab moves to
// another page the summary goes and the panel asks whether to summarize
// the new one. Each point names the parts of the page it came from (block
// numbers from the page read, C24): clicking it scrolls the page there.
// Summaries are model output (textContent only), kept in memory per tab.

// tab id -> { key, title, status: 'ask'|'busy'|'done'|'error', points, msg, req, current, model, cost }
const summaries = new Map();
let summaryReq = 0;

// The page a summary belongs to: its address without the fragment (a
// jump within the page keeps the summary).
function summaryKey(t) {
  const u = t && t.url ? t.url.full : '';
  return u.split('#')[0];
}

function summarizable(t) {
  return !!t && !!t.url && /^https?:/i.test(t.url.full || '');
}

function summaryFor(t) {
  const key = summaryKey(t);
  let sm = summaries.get(t.id);
  if (!sm || sm.key !== key) {
    // A new page: its summary starts over (a reply still on its way for
    // the old page is ignored: its request number no longer matches).
    sm = { key, status: 'ask', points: [], msg: '', req: 0, current: -1, model: '', cost: null };
    summaries.set(t.id, sm);
  }
  return sm;
}

function summaryShown() {
  return !!state && !narrow && !scriptsOpen && !rightHidden && summaryOn();
}

// The AI sidebar: the summary of the page above the chat about it.
// Pressing its button is the request for a summary: a page without one
// gets one at once. Opened otherwise (Ask AI on a selection, a
// definition's follow-up, ⌘⇧\) or on another tab, the summary waits for
// its Summarize button.
function setAi(open, summarizeNow = false) {
  setChat(open);
  const t = activeTab();
  if (open && summarizeNow && summarizable(t) && summaryFor(t).status === 'ask') summarize(t);
}

function summarize(t) {
  const sm = summaryFor(t);
  if (sm.status === 'busy') return;
  const use = useChoice('summary');
  if (!use) {
    sm.status = 'error';
    sm.msg = 'Connect an AI service in Settings → AI to summarize pages.';
    renderSummary();
    return;
  }
  const req = ++summaryReq;
  Object.assign(sm, { status: 'busy', points: [], msg: '', req, current: -1, model: use.name, cost: null });
  renderSummary();
  T.core.invoke('ai_summarize', { tab: t.id, provider: use.p, model: use.id })
    .then((r) => {
      if (sm.req !== req || summaries.get(t.id) !== sm) return;
      Object.assign(sm, { status: 'done', points: r.points || [], cost: r.cost_micros });
    })
    .catch((e) => {
      if (sm.req !== req || summaries.get(t.id) !== sm) return;
      Object.assign(sm, { status: 'error', msg: String(e) });
    })
    .finally(() => { if (state && state.active === t.id) renderSummary(); });
}

$('summary-go').addEventListener('click', withTab((t) => summarize(t)));
$('summary-again').addEventListener('click', withTab((t) => summarize(t)));

function goToPoint(t, sm, i) {
  const p = sm.points[i];
  if (!p || !p.blocks || !p.blocks.length) return;
  sm.current = i;
  document.querySelectorAll('#summary-list li').forEach((li) => li.classList.toggle('current', Number(li.dataset.i) === i));
  invoke('scroll_to_summary', { tab: t.id, blocks: p.blocks });
}

function renderSummary() {
  const show = summaryShown();
  $('summary').hidden = !show;
  $('summary').classList.toggle('dock', show);
  if (!show) $('summary').classList.remove('split-lower');
  // Tabs that closed take their summaries with them.
  if (state) {
    for (const id of summaries.keys()) if (!state.tabs.some((x) => x.id === id)) summaries.delete(id);
  }
  if (!show) return;
  const t = activeTab();
  const list = $('summary-list');
  const ask = $('summary-ask');
  const msg = $('summary-msg');
  const foot = $('summary-foot');
  if (!summarizable(t)) {
    list.replaceChildren();
    list.dataset.key = '';
    ask.hidden = true;
    $('summary-again').hidden = true;
    foot.hidden = true;
    msg.hidden = false;
    msg.textContent = 'Open a web page to summarize it.';
    return;
  }
  const sm = summaryFor(t);
  const key = `${t.id}|${sm.key}|${sm.status}|${sm.req}|${sm.points.length}|${sm.msg}`;
  $('summary-again').hidden = sm.status !== 'done';
  ask.hidden = !(sm.status === 'ask' || sm.status === 'error');
  $('summary-go').textContent = sm.status === 'error' ? 'Try Again' : 'Summarize';
  $('summary-ask-text').textContent = sm.status === 'error' ? sm.msg
    : (t.title ? `Summarize “${t.title}”?` : 'Summarize this page?');
  msg.hidden = sm.status !== 'busy';
  msg.textContent = sm.status === 'busy' ? `Summarizing with ${sm.model}…` : '';
  foot.hidden = sm.status !== 'done';
  if (sm.status === 'done') {
    const cost = sm.cost === null || sm.cost === undefined ? 'price unknown' : '≈ ' + money(sm.cost);
    foot.textContent = `${sm.model} · ${cost}`;
  }
  if (list.dataset.key === key) return;
  list.dataset.key = key;
  list.replaceChildren();
  if (sm.status !== 'done') return;
  sm.points.forEach((p, i) => {
    const li = el('li', '', p.text);
    li.dataset.i = String(i);
    if (i === sm.current) li.classList.add('current');
    if (p.blocks && p.blocks.length) {
      li.classList.add('link');
      li.tabIndex = 0;
      li.setAttribute('role', 'link');
      li.title = 'Show this in the page';
      li.addEventListener('click', () => {
        // Selecting the point's words (to copy them) is not a click on it.
        const sel = window.getSelection();
        if (sel && !sel.isCollapsed && li.contains(sel.anchorNode)) return;
        goToPoint(t, sm, i);
      });
      li.addEventListener('keydown', (ev) => {
        if (ev.key === 'Enter' || ev.key === ' ') { ev.preventDefault(); goToPoint(t, sm, i); }
      });
    }
    list.appendChild(li);
  });
}

// ---- AI chat about the page ---------------------------------------------
//
// One conversation per tab. A new one starts when the tab's page changes
// (unless it was opened from the archive) or with "+". Each message is
// sent with the whole conversation. The first reads the page; Rust keeps
// the chat (with that page context) in the archive, except in private
// tabs, and later turns reuse it. Replies are model output and are shown
// with textContent only.

// tab id -> { url, sticky, chatId, context, word, messages: [{role, text}],
//             log, input, output, cost, unpriced, busy }
// `word`: a Define chat (the start page's Define row), about that word
// rather than the page.
const chats = new Map();
let archiveOpen = false;

function newChat(url) {
  return { url, sticky: false, chatId: null, context: '', word: '', messages: [], log: [], input: 0, output: 0, cost: 0, unpriced: false, busy: false };
}

function chatFor(t) {
  const url = t && t.url ? t.url.full : '';
  let c = chats.get(t.id);
  if (!c || (c.url !== url && !c.sticky && !c.busy)) {
    c = newChat(url);
    chats.set(t.id, c);
  }
  return c;
}

// Models from every connected provider (the lists Settings → AI → Test
// fetched).
function chatModels() {
  const out = [];
  for (const p of ['anthropic', 'openai']) {
    if (!aiConfigured[p]) continue;
    const models = aiModels(p);
    if (!models.length) {
      const c = useChoice('ask');
      const id = c && c.p === p ? c.id : AI_DEFAULT_MODEL[p];
      if (id) out.push({ p, id, name: id });
    }
    for (const m of models) out.push({ p, id: m.id, name: m.name || m.id });
  }
  return out;
}

// The chat's model is the "Ask Selection" use (Settings → AI → Uses);
// the model link in the chat changes that use.
function chatChoice() {
  const models = chatModels();
  const c = useChoice('ask');
  return (c && models.find((m) => m.p === c.p && m.id === c.id)) || (c && { p: c.p, id: c.id, name: c.name }) || models[0] || null;
}

function renderChatModels() {
  const c = chatChoice();
  const btn = $('chat-model-btn');
  btn.textContent = c ? `${c.name} ▾` : 'No model ▾';
  btn.disabled = !chatModels().length;
  renderChatEfforts(c);
}

let effortGeneration = 0;
let effortModelKey = null;
function effortKey(c) { return `chatEffort.${c.p}.${c.id}`; }
function renderChatEfforts(c) {
  const key = c ? effortKey(c) : null;
  if (key && key === effortModelKey) return;
  effortModelKey = key;
  const generation = ++effortGeneration;
  const select = $('chat-effort');
  select.replaceChildren(new Option('Effort: Default', 'default'));
  select.disabled = true;
  if (!c) return;
  T.core.invoke('ai_efforts', { provider: c.p, model: c.id }).then((options) => {
    if (generation !== effortGeneration) return;
    for (const value of options) select.add(new Option(value.charAt(0).toUpperCase() + value.slice(1), value));
    const saved = pref(effortKey(c));
    select.value = options.includes(saved) ? saved : 'default';
    select.disabled = !options.length;
    select.title = options.length ? 'Reasoning effort' : 'This model uses its provider’s default effort';
  }).catch(() => { if (generation === effortGeneration) effortModelKey = null; });
}
$('chat-effort').addEventListener('change', () => {
  const c = chatChoice();
  if (c) setPref(effortKey(c), $('chat-effort').value);
});

function setModelMenu(open) {
  const menu = $('chat-model-menu');
  menu.hidden = !open;
  $('chat-model-btn').setAttribute('aria-expanded', String(open));
  if (!open) return;
  menu.replaceChildren();
  const cur = chatChoice();
  let lastP = null;
  for (const m of chatModels()) {
    if (m.p !== lastP && connectedProviders().length > 1) { menu.appendChild(el('div', 'vm-title', AI_LABEL[m.p])); lastP = m.p; }
    const b = el('button', cur && cur.p === m.p && cur.id === m.id ? 'chosen' : '', m.name);
    b.setAttribute('role', 'menuitemradio');
    b.setAttribute('aria-checked', String(!!(cur && cur.p === m.p && cur.id === m.id)));
    b.addEventListener('click', () => {
      setUse('ask', m.p, m.id);
      setModelMenu(false);
      renderChatModels();
      chatKey = '';
      renderChat();
    });
    menu.appendChild(b);
  }
}
$('chat-model-btn').addEventListener('click', () => setModelMenu($('chat-model-menu').hidden));

function dateLabel(ms) {
  return new Date(ms).toLocaleDateString(undefined, { day: 'numeric', month: 'short', year: 'numeric' });
}

let chatKey = '';
function renderChat() {
  if (!chatShown()) { chatKey = ''; return; }
  const t = activeTab();
  $('chat-archive').setAttribute('aria-pressed', String(archiveOpen));
  $('chat-log').hidden = archiveOpen;
  $('chat-form').hidden = archiveOpen;
  $('chat-archive-list').hidden = !archiveOpen;
  if (archiveOpen) { $('chat-context').hidden = true; return; }
  // State updates arrive often (load progress); rebuild only on change,
  // so a selection in the log survives.
  const cur = t ? chats.get(t.id) : null;
  const pick = chatChoice();
  const key = JSON.stringify([t && t.id, t && t.url && t.url.full, cur && cur.log.length, cur && cur.busy, cur && cur.chatId, pick && pick.p + pick.id]);
  if (key === chatKey) return;
  chatKey = key;
  renderChatModels();
  const log = $('chat-log');
  const atEnd = log.scrollTop + log.clientHeight >= log.scrollHeight - 24;
  log.replaceChildren();
  const c = t && (t.url || (cur && cur.sticky)) ? chatFor(t) : null;
  $('chat-context').hidden = !(c && c.context);
  $('chat-context').textContent = c ? c.context : '';
  $('chat-input').placeholder = c && c.word ? 'Ask about this word' : 'Ask about this page';
  if (!c) {
    $('chat-cost').textContent = '';
    $('chat-send').disabled = true;
    return;
  }
  for (const m of c.log) {
    const li = el('li', m.role, m.text);
    if (m.meta) li.appendChild(el('span', 'meta', m.meta));
    log.appendChild(li);
  }
  if (c.busy) log.appendChild(el('li', 'assistant pending', '…'));
  const tokens = c.input + c.output;
  $('chat-cost').textContent = tokens ? `${c.unpriced ? '≥ ' : '≈ '}${money(c.cost)} · ${tokens.toLocaleString()} tokens` : '';
  $('chat-send').disabled = c.busy || !pick;
  if (atEnd || c.busy) log.scrollTop = log.scrollHeight;
}

// Browser chrome (see core.js): The chat archive and Define.
'use strict';

// ---- archive: earlier chats, to read again, continue or delete ----------

function setArchive(open) {
  archiveOpen = open;
  setModelMenu(false);
  if (open) loadArchive();
  chatKey = '';
  renderChat();
}

function loadArchive() {
  const list = $('chat-archive-list');
  T.core.invoke('list_chats').then((items) => {
    list.replaceChildren();
    for (const it of items) {
      const li = el('li');
      const open = el('button', 'open');
      open.appendChild(el('span', 't', it.first || it.title || it.host));
      const cost = it.cost_micros || it.unpriced ? ` · ${it.unpriced ? '≥ ' : ''}${money(it.cost_micros)}` : '';
      open.appendChild(el('span', 'u', `${it.word ? 'Definition' : it.host} · ${dateLabel(it.updated_ms)}${cost}`));
      open.addEventListener('click', () => openArchived(it.id));
      const rm = el('button', 'remove', '×');
      rm.setAttribute('aria-label', 'Delete this chat');
      rm.addEventListener('click', () => {
        for (const [tab, c] of chats) if (c.chatId === it.id) chats.delete(tab);
        invoke('delete_chat', { id: it.id }).then(loadArchive);
      });
      li.append(open, rm);
      list.appendChild(li);
    }
    if (!items.length) list.appendChild(el('li', 'muted empty', '—'));
  }).catch((e) => toast(String(e)));
}

// Continues an archived chat in this tab, with the context it began with.
function openArchived(id) {
  const t = activeTab();
  if (!t) return;
  T.core.invoke('get_chat', { id }).then((v) => {
    if (!v) { loadArchive(); return; }
    const c = newChat(t.url ? t.url.full : '');
    c.sticky = true;
    c.chatId = v.id;
    c.word = v.word || '';
    c.context = c.word
      ? `Definition · “${c.word}” · ${dateLabel(v.created_ms)}`
      : `${v.host} · ${dateLabel(v.created_ms)}` + (v.selection ? ` · “${v.selection.length > 120 ? v.selection.slice(0, 120) + '…' : v.selection}”` : '');
    c.messages = v.messages.map((m) => ({ role: m.role, text: m.text }));
    c.log = c.messages.map((m) => ({ role: m.role, text: m.text }));
    c.input = v.input;
    c.output = v.output;
    c.cost = v.cost_micros;
    c.unpriced = v.unpriced;
    chats.set(t.id, c);
    setArchive(false);
    focusInput($('chat-input'));
  }).catch((e) => toast(String(e)));
}

// Shows or hides the chat panel (open, and the right panels not hidden).
function applyChat() {
  const shown = chatShown();
  $('chat').hidden = !shown;
  $('chat').classList.toggle('dock', shown && !narrow);
  if (!shown) $('chat').classList.remove('split-lower');
}

function setChat(open) {
  chatOpen = open;
  if (open) rightHidden = false;
  if (!open) { archiveOpen = false; setModelMenu(false); }
  applyChat();
  applyNotebook();
  if (open) {
    loadAi().then(() => { chatKey = ''; renderChat(); });
    focusInput($('chat-input'));
  }
  renderViewMode();
  reportLayout();
}

function sendChat() {
  const t = activeTab();
  const input = $('chat-input');
  const text = input.value.trim();
  if (!t || !text) return;
  if (askChat(t, chatFor(t), text)) input.value = '';
}

// Sends `text` as the next message of chat `c` in tab `t`; false if it
// cannot be sent now (no model, a reply pending, nothing to ask about).
function askChat(t, c, text) {
  const choice = chatChoice();
  if (!choice || c.busy || (!t.url && !c.chatId && !c.word)) return false;
  c.messages.push({ role: 'user', text });
  c.log.push({ role: 'user', text });
  c.busy = true;
  const effort = $('chat-effort').value;
  renderChat();
  T.core.invoke('ai_chat', { tab: t.id, provider: choice.p, model: choice.id, effort, messages: c.messages, chatId: c.chatId, word: c.word || null })
    .then((r) => {
      c.messages.push({ role: 'assistant', text: r.text });
      if (r.chat_id) c.chatId = r.chat_id;
      c.input += r.input;
      c.output += r.output;
      if (r.cost_micros === null || r.cost_micros === undefined) c.unpriced = true; else c.cost += r.cost_micros;
      const cost = r.cost_micros === null || r.cost_micros === undefined ? 'price unknown' : '≈ ' + money(r.cost_micros);
      c.log.push({ role: 'assistant', text: r.text, meta: `${choice.name} · ${(r.input + r.output).toLocaleString()} tokens · ${cost}` });
      loadUsage();
    })
    .catch((e) => {
      // Not part of the conversation: the next message is sent without it.
      c.messages.pop();
      c.log.push({ role: 'error', text: String(e) });
    })
    .finally(() => { c.busy = false; renderChat(); });
  return true;
}

// The start page's Define row (owner request 2026-11-09): the definition
// appears in a card above the start page's input, not in the chat panel.
// The first request asks the model to check the spelling; a likely
// misspelling is answered with the word it thinks was meant, and the
// card asks which one to define (nothing is archived until then, so only
// the definition stays in the chat archive). Rust: `ai::define_request`;
// no page is read. The definition becomes the tab's chat, so "Ask a
// follow-up" continues it in the chat panel.
// tab id -> { word, asked, state: 'pending' | 'ask' | 'done' | 'error',
//             text, suggestion, meta } (`defines`, declared at the top)

function defineWord(t, word, exact = false) {
  if (!t) return;
  loadAi().then(() => {
    const choice = chatChoice();
    if (!choice) { toast('Connect an AI provider in Settings → AI to define words.'); return; }
    const d = { word, asked: word, state: 'pending', text: '', suggestion: '', meta: '' };
    defines.set(t.id, d);
    renderDefine();
    const messages = [{ role: 'user', text: `Define “${word}”.` }];
    const effort = $('chat-effort').value;
    T.core.invoke('ai_chat', { tab: t.id, provider: choice.p, model: choice.id, effort, messages, chatId: null, word, exact })
      .then((r) => {
        loadUsage();
        if (defines.get(t.id) !== d) return;
        if (r.suggestion) {
          d.state = 'ask';
          d.suggestion = r.suggestion;
          renderDefine();
          $('define-yes').focus({ preventScroll: true });
          return;
        }
        const cost = r.cost_micros === null || r.cost_micros === undefined ? 'price unknown' : '≈ ' + money(r.cost_micros);
        d.state = 'done';
        d.text = r.text;
        d.meta = `${choice.name} · ${cost}`;
        // The tab's chat: a follow-up continues this definition.
        const c = newChat(t.url ? t.url.full : '');
        c.sticky = true;
        c.word = word;
        c.context = `Definition · “${word}”`;
        c.chatId = r.chat_id || null;
        c.messages = [...messages, { role: 'assistant', text: r.text }];
        c.log = [messages[0], { role: 'assistant', text: r.text, meta: `${choice.name} · ${(r.input + r.output).toLocaleString()} tokens · ${cost}` }];
        c.input = r.input;
        c.output = r.output;
        if (r.cost_micros === null || r.cost_micros === undefined) c.unpriced = true; else c.cost = r.cost_micros;
        chats.set(t.id, c);
        chatKey = '';
        renderChat();
        renderDefine();
      })
      .catch((e) => {
        if (defines.get(t.id) !== d) return;
        d.state = 'error';
        d.text = String(e);
        renderDefine();
      });
  });
}

// The card above the start page's input, for the start page's tab.
function renderDefine() {
  const t = activeTab();
  const d = t && startShown && !t.url ? defines.get(t.id) : null;
  const card = $('define-card');
  card.hidden = !d;
  if (!d) return;
  $('define-word').textContent = d.state === 'ask' ? d.asked : d.word;
  const body = $('define-body');
  body.className = 'define-body' + (d.state === 'pending' ? ' pending' : d.state === 'error' ? ' error' : '');
  body.textContent = d.state === 'pending' ? 'Defining…' : d.state === 'ask' ? `Did you mean “${d.suggestion}”?` : d.text;
  $('define-ask').hidden = d.state !== 'ask';
  if (d.state === 'ask') {
    $('define-yes').textContent = `Define “${d.suggestion}”`;
    $('define-no').textContent = `Define “${d.asked}”`;
  }
  $('define-foot').hidden = d.state !== 'done';
  $('define-meta').textContent = d.meta;
}

function closeDefine() {
  const t = activeTab();
  if (t) defines.delete(t.id);
  renderDefine();
  if (startShown) focusInput($('palette-input'));
}
$('define-close').addEventListener('click', closeDefine);
$('define-yes').addEventListener('click', () => {
  const t = activeTab();
  const d = t && defines.get(t.id);
  if (d && d.state === 'ask') defineWord(t, d.suggestion, true);
});
$('define-no').addEventListener('click', () => {
  const t = activeTab();
  const d = t && defines.get(t.id);
  if (d && d.state === 'ask') defineWord(t, d.asked, true);
});
$('define-more').addEventListener('click', () => {
  if (archiveOpen) setArchive(false);
  setChat(true);
});

$('chat-close').addEventListener('click', () => setAi(false));
$('chat-new').addEventListener('click', () => {
  const t = activeTab();
  if (t) chats.set(t.id, newChat(t.url ? t.url.full : ''));
  if (archiveOpen) setArchive(false);
  chatKey = '';
  renderChat();
  focusInput($('chat-input'));
});
$('chat-archive').addEventListener('click', () => setArchive(!archiveOpen));
$('chat-form').addEventListener('submit', (ev) => { ev.preventDefault(); sendChat(); });
$('chat-input').addEventListener('keydown', (ev) => {
  if (ev.key === 'Enter' && !ev.shiftKey && !ev.isComposing) { ev.preventDefault(); sendChat(); }
  else if (ev.key === 'Escape') { ev.preventDefault(); setChat(false); }
});
document.addEventListener('pointerdown', (ev) => {
  if (!$('chat-model-menu').hidden && !$('chat-model-menu').contains(ev.target) && ev.target !== $('chat-model-btn')) setModelMenu(false);
});

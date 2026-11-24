// Browser chrome (see core.js): research sessions (owner request
// 2026-11-24). A session is a fresh sidebar (no pins, no bookmarks: only
// its own tabs) and its own notebook, shown open by default. Its topic
// heads the tab list beside a back button to everyday browsing; pointing
// at the topic offers Rename. Every session is a row at the foot of the
// sidebar, above its buttons. Menu → New Research Session makes one. Each
// session has a menu (≡ on its row while pointed at, and right of its
// topic): Export Notebook, Send Tabs to Main, Delete.
'use strict';

// The session whose sidebar and notebook are set up (null: everyday).
let researchShown = null;
// A session just made: its topic is ready to be typed once it shows.
let researchRenamePending = null;
let researchListKey = '';

function researchById(id) {
  return ((state && state.research) || []).find((r) => r.id === id) || null;
}

function renderResearch() {
  const space = (state && state.space) || null;
  document.body.classList.toggle('research', !!space);
  renderResearchHead(space);
  renderResearchList(space);
  if (space !== researchShown) {
    researchShown = space;
    clearChosen();
    switchNotebookSpace(space);
  }
  // After this render (the new session's start page asks for the focus
  // in it too).
  if (researchRenamePending && researchRenamePending === space) {
    researchRenamePending = null;
    setTimeout(renameResearch, 0);
  }
}

// ---- the topic at the top of the tab list ---------------------------------------

function renderResearchHead(space) {
  const head = $('research-head');
  const r = space && researchById(space);
  head.hidden = !r;
  if (!r || head.querySelector('input')) return;
  if (!head.childElementCount) {
    const back = el('button', 'icon rs-back');
    back.setAttribute('aria-label', 'Back to everyday browsing');
    back.title = 'Back to everyday browsing';
    back.appendChild(svgIcon('M15 5l-7 7 7 7'));
    back.addEventListener('click', () => invoke('enter_research', { space: null }));
    const name = el('button', 'rs-topic');
    name.title = 'Rename';
    name.addEventListener('click', renameResearch);
    const pen = el('button', 'icon rs-rename');
    pen.setAttribute('aria-label', 'Rename this research session');
    pen.title = 'Rename';
    pen.appendChild(svgIcon('M4 20h4L19 9l-4-4L4 16v4zM14 6l4 4'));
    pen.addEventListener('click', renameResearch);
    const menu = researchMenuButton(() => researchById(state && state.space), head);
    head.append(back, name, pen, menu);
  }
  head.querySelector('.rs-topic').textContent = r.name;
}

// The topic becomes a field: ↩ keeps the new name, Escape the old one.
function renameResearch() {
  const space = state && state.space;
  const r = space && researchById(space);
  const head = $('research-head');
  const label = head.querySelector('.rs-topic');
  if (!r || !label || head.querySelector('input')) return;
  const field = el('input', 'rs-field');
  field.value = r.name;
  field.maxLength = 80;
  field.setAttribute('aria-label', 'Research topic');
  let done = false;
  const finish = (keep) => {
    if (done) return;
    done = true;
    const name = field.value.trim();
    field.replaceWith(label);
    if (keep && name && name !== r.name) {
      label.textContent = name;
      invoke('rename_research', { id: r.id, name });
    }
  };
  field.addEventListener('keydown', (ev) => {
    ev.stopPropagation();
    if (ev.key === 'Enter') finish(true);
    else if (ev.key === 'Escape') finish(false);
  });
  field.addEventListener('blur', () => finish(true));
  label.replaceWith(field);
  focusInput(field, true);
}

// ---- the sessions at the foot of the sidebar --------------------------------------

function renderResearchList(space) {
  const list = $('research-list');
  const all = (state && state.research) || [];
  const key = JSON.stringify([space, all]);
  if (key === researchListKey) return;
  researchListKey = key;
  list.replaceChildren();
  list.hidden = !all.length;
  for (const r of all) {
    const row = el('div', 'rs-row' + (r.id === space ? ' active' : ''));
    row.dataset.space = r.id;
    row.setAttribute('role', 'tab');
    row.setAttribute('aria-selected', String(r.id === space));
    const open = el('button', 'open');
    open.appendChild(svgIcon('M10.5 4a6.5 6.5 0 1 1 0 13 6.5 6.5 0 0 1 0-13zM15.5 15.5 20 20'));
    open.appendChild(el('span', 't', r.name));
    open.appendChild(el('span', 'rs-count', String(r.tabs)));
    open.title = `${r.name}: ${r.tabs === 1 ? '1 tab' : `${r.tabs} tabs`}`;
    open.addEventListener('click', () => invoke('enter_research', { space: r.id === space ? null : r.id }));
    const acts = el('span', 'row-actions');
    acts.appendChild(researchMenuButton(() => r, row));
    row.append(open, acts);
    row.addEventListener('contextmenu', (ev) => {
      ev.preventDefault();
      openResearchMenu(r, row);
    });
    list.appendChild(row);
  }
}

function deleteResearch(r) {
  const anchor = () => document.querySelector(`.rs-row[data-space="${CSS.escape(r.id)}"]`);
  const tabs = r.tabs === 1 ? 'its tab' : `its ${r.tabs} tabs`;
  askConfirm(`Delete “${r.name}”, ${tabs} and its notebook?`, 'Delete', null, () => {
    try { localStorage.removeItem('nbClosed:' + r.id); } catch (e) { /* storage unavailable */ }
    invoke('delete_research', { id: r.id });
  }, null, { anchor });
}

// The session's menu button (≡): on its row (shown while pointed at) and
// right of its topic. `session()` is the session when pressed.
function researchMenuButton(session, anchor) {
  const b = el('button', 'icon rs-menu');
  b.setAttribute('aria-label', 'Research session menu');
  b.setAttribute('aria-haspopup', 'menu');
  b.title = 'Export Notebook, Send Tabs to Main, Delete';
  b.appendChild(svgIcon('M5 7h14M5 12h14M5 17h14'));
  // A press outside the menu closes it first (wiring.js): a second press
  // on the button that opened it only closes it.
  let wasOpen = false;
  b.addEventListener('pointerdown', () => {
    const r = session();
    wasOpen = !!r && !$('pin-pop').hidden && $('pin-pop').dataset.space === r.id;
  });
  b.addEventListener('click', (ev) => {
    ev.stopPropagation();
    const r = session();
    if (!r || wasOpen) { wasOpen = false; return; }
    openResearchMenu(r, anchor);
  });
  return b;
}

// A session's menu (its ≡, or right-click on its row): Export Notebook,
// Send Tabs to Main, Delete.
function openResearchMenu(r, anchor) {
  openPinPop({ url: '', id: null }, anchor, 'group');
  $('pin-pop').dataset.space = r.id;
  for (const id of ['item-copy', 'item-edit', 'item-original', 'pin-to-bookmark']) $(id).hidden = true;
  for (const e of $('pin-pop').querySelectorAll('.pin-only, .item-only')) e.hidden = true;
  const more = $('item-more');
  more.replaceChildren(el('div', 'menu-label', r.name));
  more.appendChild(menuButton('Export Notebook', () => exportResearchNotebook(r)));
  more.appendChild(menuButton('Send Tabs to Main', () => sendResearchTabsHome(r)));
  more.appendChild(menuButton('Delete', () => deleteResearch(r)));
  placePinPop(anchor);
}

// The session's notebook as one page, saved where the user chooses (the
// one shown is saved first, so the export has its last strokes).
async function exportResearchNotebook(r) {
  try {
    if (notebook && notebookSpace === r.id) await notebook.flush();
    await T.core.invoke('export_notebook', { space: r.id, date: new Date().toLocaleDateString() });
  } catch (e) {
    toast(String(e));
  }
}

// A tab (or a pin's or bookmark's page) dropped on a session's row: a
// link in that session's notebook (the tab stays where it is). The
// notebook shown takes it at once; another is changed where it is kept.
function addToSessionNotebook(space, item, page) {
  const r = researchById(space);
  if (!r) return;
  const done = () => toast(`Added to the notebook of “${r.name}”.`);
  if (notebookSpace === space && (notebook || notebookLoading)) {
    ensureNotebook().then((nb) => { nb.addQuote(page); done(); });
    return;
  }
  const args = item.kind === 'tab' ? { space, tab: item.id, item: null } : { space, tab: null, item: item.url };
  T.core.invoke('notebook_add_page', args)
    .then((added) => (added ? done() : toast('Nothing to add yet.')))
    .catch((e) => toast(String(e)));
}

// Every tab of the session goes to everyday browsing; from inside the
// session, everyday browsing is shown with the first of them.
async function sendResearchTabsHome(r) {
  const tabs = (state ? state.tabs : []).filter((t) => t.space === r.id && t.sheet_over === null && !t.saved && t.launcher === null);
  const pages = tabs.filter((t) => t.url);
  if (!pages.length) { toast(`“${r.name}” has no tabs to send.`); return; }
  const shown = state.space === r.id;
  try {
    await T.core.invoke('send_tabs', { tabs: pages.map((t) => t.id), to: null });
    if (shown) await T.core.invoke('activate_tab', { tab: pages[0].id });
    toast(`${pages.length === 1 ? '1 tab' : `${pages.length} tabs`} sent to everyday browsing.`);
  } catch (e) {
    toast(String(e));
  }
}

$('menu-new-research').addEventListener('click', () => {
  closeMenu();
  if (narrow) setSidebar(true);
  else if (!sidebarShown) setSidebarState(sidebarMode === 'flyout' ? 'attached' : sidebarMode, true);
  T.core.invoke('new_research', { name: 'New Research' })
    .then((id) => { researchRenamePending = id; if (state && state.space === id) renderResearch(); })
    .catch((e) => toast(String(e)));
});

// ---- each session's notebook -------------------------------------------------------
//
// One notebook per session (Rust: `notebook_in`): moving between sessions
// saves the one shown and loads the other's. A research session's notebook
// is open unless it was closed there; everyday browsing's is open or
// closed as last left (`notebookOpen`).

let notebookSpace = null;

function notebookOpenFor(space) {
  return space ? pref('nbClosed:' + space) !== '1' : pref('notebookOpen') === '1';
}

function rememberNotebookOpen(open) {
  if (notebookSpace) setPref('nbClosed:' + notebookSpace, open ? '0' : '1');
  else setPref('notebookOpen', open ? '1' : '0');
}

function switchNotebookSpace(space) {
  if (space === notebookSpace) return;
  if (notebook) {
    notebook.hidden();
    notebook.flush();
  }
  notebook = null;
  notebookLoading = null;
  notebookEpoch++;
  notebookPage = '';
  const nb = $('notebook');
  nb.replaceChildren();
  nb.className = 'sheet';
  notebookSpace = space;
  notebookOpen = notebookOpenFor(space);
  if (notebookOpen) {
    rightHidden = false;
    ensureNotebook();
  }
  applyNotebook();
  applyChat();
  renderSummary();
  reportLayout();
}

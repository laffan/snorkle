// Browser chrome (see core.js): Downloads: the list, progress in the toast, prompts.
'use strict';

// ---- downloads ----------------------------------------------------------
//
// Files a page downloads stay in the app's own folder (listed here, with
// progress while they run) until the user saves them elsewhere (Files on
// iPadOS, a save panel on macOS), shares or deletes them. Nothing is
// opened. The list comes from Rust (`downloads` events); names and hosts
// are shown with textContent.
let downloads = [];
let downloadsClearArmed = false;
const dlRows = new Map();

// Downloads, the one panel left over the page (History is in the
// sidebar since 2026-11-23).
function closePanel() {
  panel = null;
  render();
  downloadProgressToast(); // running downloads show in the toast again
}

function openDownloads() {
  closeMenu();
  panel = 'downloads';
  downloadsClearArmed = false;
  $('downloads-clear').textContent = 'Clear';
  $('downloads-note').textContent = macOS
    ? 'Downloaded files stay in Snorkle until you save them elsewhere or delete them.'
    : 'Downloaded files stay in Snorkle until you save them to Files or delete them.';
  T.core.invoke('list_downloads').then(setDownloads).catch((e) => toast(String(e)));
  if (narrow) setSidebar(false);
  render();
  downloadProgressToast(); // the panel shows the progress instead
}

function downloadStatus(d) {
  const of = d.expected ? `${fmtBytes(d.received)} of ${fmtBytes(d.expected)}` : fmtBytes(d.received);
  if (d.state === 'active') return d.expected ? `${of} · ${Math.floor((100 * d.received) / d.expected)}%` : of;
  if (d.state === 'done') return fmtBytes(d.expected || d.received);
  if (d.state === 'cancelled') return 'Cancelled';
  return d.error ? `Failed: ${d.error}` : 'Failed';
}

function dlRow(d) {
  const li = el('li', 'dl');
  const info = el('div', 'info');
  const t = el('span', 't');
  const meta = el('span', 'meta');
  const bar = el('div', 'bar');
  bar.appendChild(el('i'));
  info.append(t, meta, bar);
  const acts = el('div', 'acts');
  li.append(info, acts);
  li._parts = { t, meta, bar, acts, state: null };
  return li;
}

function dlButton(cls, label, run) {
  const b = el('button', cls, label);
  b.addEventListener('click', (ev) => { ev.stopPropagation(); run(b); });
  return b;
}

function fillDlRow(li, d) {
  const p = li._parts;
  p.t.textContent = d.name;
  const from = d.source ? d.source.registrable : 'From the page';
  p.meta.textContent = `${from}${d.private ? ' · Private' : ''} · ${downloadStatus(d)}`;
  p.meta.classList.toggle('err', d.state === 'failed');
  p.bar.hidden = d.state !== 'active';
  p.bar.classList.toggle('indeterminate', d.state === 'active' && !d.expected);
  if (d.state === 'active' && d.expected) p.bar.firstChild.style.width = `${Math.min(100, (100 * d.received) / d.expected)}%`;
  if (p.state === d.state) return; // buttons only change with the state
  p.state = d.state;
  p.acts.replaceChildren();
  const rectOf = (b) => { const r = b.getBoundingClientRect(); return { x: r.left, y: r.top, w: r.width, h: r.height }; };
  if (d.state === 'active') {
    p.acts.appendChild(dlButton('button', 'Cancel', () => invoke('cancel_download', { id: d.id })));
  } else {
    if (d.state === 'done') {
      p.acts.appendChild(dlButton('button primary', macOS ? 'Save…' : 'Save to Files…', () => invoke('export_download', { id: d.id })));
      p.acts.appendChild(dlButton('button', 'Share…', (b) => invoke('share_download', { id: d.id, ...rectOf(b) })));
    }
    const x = dlButton('x', '×', () => invoke('delete_download', { id: d.id }));
    x.setAttribute('aria-label', d.state === 'done' ? 'Delete file' : 'Remove from list');
    x.title = d.state === 'done' ? 'Delete file' : 'Remove from list';
    p.acts.appendChild(x);
  }
}

function renderDownloads() {
  const running = downloads.filter((d) => d.state === 'active').length;
  $('menu').classList.toggle('busy', running > 0);
  $('menu-downloads').textContent = running ? `Downloads (${running})` : 'Downloads';
  if (panel !== 'downloads') return;
  const list = $('downloads-list');
  if (!downloads.length) {
    dlRows.clear();
    list.replaceChildren(el('li', 'muted', 'No downloads yet.'));
    return;
  }
  const keep = new Set(downloads.map((d) => d.id));
  for (const [id, li] of dlRows) if (!keep.has(id)) { li.remove(); dlRows.delete(id); }
  list.querySelectorAll('li.muted').forEach((n) => n.remove());
  downloads.forEach((d, i) => {
    let li = dlRows.get(d.id);
    if (!li) { li = dlRow(d); dlRows.set(d.id, li); }
    fillDlRow(li, d);
    if (list.children[i] !== li) list.insertBefore(li, list.children[i] || null);
  });
}

// Downloads in the toast (2026-11-19; before, Rust sent "Downloading …"
// and "Downloaded …" toasts). While files download and the Downloads
// panel is closed, the toast shows how far they are: the name (or how
// many), the percentage and a bar (indeterminate without a size). When
// one finishes or fails, the toast says so with a link to the panel.
let dlSeen = null; // id → state in the previous list; null until the first
function setDownloads(list) {
  downloads = list || [];
  const prev = dlSeen;
  dlSeen = new Map(downloads.map((d) => [d.id, d.state]));
  renderDownloads();
  if (!prev) return; // the first list (launch) announces nothing
  // Newly ended: running in the previous list (Rust sends every start).
  // Cancelling is the user's own doing and needs no message.
  const ended = downloads.filter((d) => (d.state === 'done' || d.state === 'failed') && prev.get(d.id) === 'active');
  if (ended.length) downloadEndedToast(ended);
  else downloadProgressToast();
}

function downloadEndedToast(ended) {
  const failed = ended.filter((d) => d.state === 'failed').length;
  let msg;
  if (ended.length === 1) msg = failed ? `Download failed: ${ended[0].name}` : `Downloaded “${ended[0].name}”`;
  else msg = failed ? `${ended.length} downloads ended, ${failed} failed` : `${ended.length} downloads finished`;
  // The panel open already shows them.
  toast(msg, panel === 'downloads' ? null : { label: 'Show in Downloads', run: openDownloads });
}

// Shows (or updates) the running downloads' progress in the toast, if
// the toast is free; clears it when none run or the panel is open.
function downloadProgressToast() {
  const running = downloads.filter((d) => d.state === 'active');
  if (!running.length || panel === 'downloads') {
    if (toastKind === 'download') clearToast();
    return;
  }
  if (toastKind && toastKind !== 'download') return; // handed back by hideToast
  const t = $('toast');
  let parts = toastKind === 'download' ? t._dl : null;
  if (!parts) {
    parts = { name: el('span', 'toast-name'), pct: el('span', 'toast-pct'), bar: el('span', 'toast-bar') };
    // The spaces are for screen readers; the flex gap spaces them visually.
    t.replaceChildren(el('span', null, 'Downloading'), ' ', parts.name, ' ', parts.pct, parts.bar);
    t._dl = parts;
    t.className = 'toast progress dl';
    t.hidden = false;
    toastKind = 'download';
    clearTimeout(toastTimer);
  }
  parts.name.textContent = running.length === 1 ? `“${running[0].name}”` : `${running.length} files`;
  const expected = running.every((d) => d.expected) ? running.reduce((n, d) => n + d.expected, 0) : 0;
  const received = running.reduce((n, d) => n + d.received, 0);
  const pct = expected ? Math.min(100, Math.floor((100 * received) / expected)) : null;
  parts.pct.textContent = pct === null ? fmtBytes(received) : `${pct}%`;
  parts.bar.classList.toggle('indeterminate', pct === null);
  parts.bar.style.width = pct === null ? '' : pct + '%';
  reportLayout();
}

$('downloads-close').addEventListener('click', closePanel);
$('downloads-clear').addEventListener('click', () => {
  if (!downloads.some((d) => d.state !== 'active')) return;
  if (!downloadsClearArmed) { downloadsClearArmed = true; $('downloads-clear').textContent = 'Tap again to delete'; return; }
  downloadsClearArmed = false;
  $('downloads-clear').textContent = 'Clear';
  invoke('clear_downloads');
});
T.event.listen('downloads', (e) => setDownloads(e.payload));
setTimeout(() => T.core.invoke('list_downloads').then(setDownloads).catch(() => {}), 250); // after the first frame

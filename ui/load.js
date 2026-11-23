// Loads the chrome's scripts, in order (ui/chrome/, one shared scope; see
// chrome/core.js). Inserted scripts with `async = false` run in insertion
// order, as `defer` scripts do; this file itself is deferred, so the
// document is parsed first. The list lives here rather than in index.html
// (the 700-line rule). The Notebook's parts (ui/notebook/, each its own
// closure; see notebook/base.js) load only when it is first needed
// (`snorkleLoadNotebook`, 2026-11-23): 2,700 lines the launch no longer
// parses while the notebook is closed.
'use strict';

(() => {
  const NOTEBOOK = ['base', 'ink', 'markdown', 'chips', 'lasso', 'texts', 'search', 'surface', 'minimap', 'notebook'];
  const CHROME = [
    'core', 'render', 'later', 'downloads', 'history', 'items', 'drag', 'sidebar', 'settings', 'layout', 'modes',
    'ai', 'archive', 'notebook-dock', 'console', 'palette', 'shortcuts', 'find', 'keys', 'selection', 'wiring',
  ];
  const add = (src) => {
    const s = document.createElement('script');
    s.src = src;
    s.async = false;
    document.head.appendChild(s);
    return s;
  };
  // Resolves once the notebook's last part has run (they run in order).
  let notebookLoaded = null;
  window.snorkleLoadNotebook = () => {
    if (!notebookLoaded) {
      notebookLoaded = new Promise((resolve, reject) => {
        let last = null;
        for (const name of NOTEBOOK) last = add(`notebook/${name}.js`);
        last.addEventListener('load', () => resolve());
        last.addEventListener('error', () => reject(new Error('The notebook could not load.')));
      });
    }
    return notebookLoaded;
  };
  for (const name of CHROME) add(`chrome/${name}.js`);
})();

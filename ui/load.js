// Loads the chrome's scripts, in order: the Notebook's parts (ui/notebook/,
// each its own closure; see notebook/base.js), then the chrome's
// (ui/chrome/, one shared scope; see chrome/core.js). Inserted scripts with
// `async = false` run in insertion order, as `defer` scripts do; this file
// itself is deferred, so the document is parsed first. The list lives here
// rather than in index.html (the 700-line rule).
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
  };
  for (const name of NOTEBOOK) add(`notebook/${name}.js`);
  for (const name of CHROME) add(`chrome/${name}.js`);
})();

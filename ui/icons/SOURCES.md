# Bundled site icons

Google product artwork identifies pinned sites, tabs and bookmarks. The SVGs
are bundled locally and displayed in their original colours through `<img>`;
no icon CDN is contacted by the chrome. Trademarks belong to Google LLC.

- Gmail, Drive, Calendar, Keep, Meet, Maps, Photos and YouTube: the colour logos
  in `@iconify-json/logos` 1.2.19 (SVG Logos by Gil Barbara, CC0-1.0),
  https://github.com/gilbarbara/logos. Imported bodies unchanged; an SVG wrapper
  supplies the collection's dimensions. The `*-2020` variants are used where
  available; Maps uses `google-maps`, YouTube uses `youtube-icon`.
- Docs, Sheets, Slides, Forms and Chat: `svg/google-*.svg` from
  https://github.com/homarr-labs/dashboard-icons/tree/16c42dc580c144246651d322a079e535cb2b69f8/svg
  (Apache-2.0, reproduced in `LICENSE-dashboard-icons.txt`). Artwork unchanged;
  added missing `viewBox` attributes to the four document icons so they scale
  correctly. Copyright (c) 2024 Bjorn Lammers, Meier Lukas, Thomas Camlong and
  Homarr Labs.

These public catalogs redistribute Google's product logos; the files were not
retrieved directly from Google in this environment. Each imported SVG was
checked for scripts, event attributes, external references and embedded images.
Only vector shapes, gradients, masks and local fragment references are present.

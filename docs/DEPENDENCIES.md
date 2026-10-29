# Dependencies

Policy:
- Every external crate is pinned with `=` in `Cargo.toml` and `Cargo.lock` is committed.
- The objc2 crates (`objc2`, `block2`, `objc2-foundation`, `objc2-ui-kit`, `objc2-web-kit`,
  `objc2-core-foundation`) must stay at exactly the versions wry resolves. Two copies would
  register duplicate Objective-C classes. `scripts/check-deps.sh` fails if they are duplicated.
- No Tauri plugins (each adds an IPC surface). No JavaScript packages: `ui/` is plain JS with no
  build step, and the check script fails if `ui/package.json` appears.
- Adding or bumping a dependency: explain why in this file, run `scripts/check-deps.sh`
  (`cargo deny check` + `cargo audit` + graph checks) and the full test suite.

## Direct dependencies

| Crate | Version | Used by | Why |
|---|---|---|---|
| tauri | =2.12.5 (default features off; `wry`, `compression`) | src-tauri | App shell, chrome webview, IPC/ACL. `dynamic-acl` deliberately off. |
| tauri-build | =2.7.4 | src-tauri (build) | ACL manifest, context generation |
| raw-window-handle | =0.6.2 | src-tauri | Root `UIView` for the content host (already in Tauri's graph) |
| url | =2.5.8 | core, content | WHATWG URL parsing (already in Tauri's graph) |
| idna | =1.1.0 | core | UTS #46 to-Unicode for display (dependency of `url`) |
| psl | =2.1.245 | core | Public Suffix List, compiled in; no runtime parsing |
| unicode-script | =0.5.8 | core | Script property for the IDN policy; no dependencies |
| serde / serde_json | =1.0.229 / =1.0.154 | core, src-tauri | State serialisation, storage (already in Tauri's graph) |
| objc2 / block2 | =0.6.5 / =0.6.2 | content (iOS) | ObjC runtime, blocks (same versions as wry) |
| objc2-foundation / -ui-kit / -web-kit / -core-foundation | =0.3.2 | content (iOS) | Framework bindings (same versions as wry) |
| libfuzzer-sys | =0.4.13 | fuzz (separate workspace) | cargo-fuzz harness |

Graph size (normal edges): `snorkle-core` 44 crates; the iOS app 235 crates, almost all from
Tauri/wry. Our own additions beyond Tauri's graph: `psl`, `psl-types`, `unicode-script`.

## Audit results (2026-10-29)

- `cargo audit` (RustSec DB, 1,386 advisories, 404 crates in the lockfile): **0 vulnerabilities.**
  2 warnings, both Linux-only (desktop dev/test build via GTK, not in the iOS graph):
  RUSTSEC-2024-0370 (`proc-macro-error` unmaintained), RUSTSEC-2024-0429 (`glib` unsound iterator).
- `cargo deny check`: advisories ok, bans ok, licences ok, sources ok (crates.io only).
- No embedded JS engine or alternative web engine in the iOS graph (`javascriptcore-rs` appears
  only on Linux through WebKitGTK).

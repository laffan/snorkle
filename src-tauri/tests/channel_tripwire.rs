//! Security suite: tripwires for new page-to-native channels.
//!
//! Any new way for web content to send data to native code must be
//! security-reviewed and signed off (docs/SECURITY_REVIEW.md). These tests
//! fail when code that could create such a channel appears anywhere in the
//! native crates or the chrome UI, so the change cannot land unnoticed.
//! If one fires for a reviewed change, update the allowlist in the same PR
//! as the sign-off.

use std::fs;
use std::path::{Path, PathBuf};

fn workspace() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).parent().unwrap().to_path_buf()
}

fn files(dir: &Path, ext: &str, out: &mut Vec<PathBuf>) {
    for e in fs::read_dir(dir).unwrap() {
        let p = e.unwrap().path();
        if p.is_dir() {
            files(&p, ext, out);
        } else if p.extension().map(|x| x == ext).unwrap_or(false) {
            out.push(p);
        }
    }
}

/// Source with `//` comment lines removed (docs may name the APIs).
fn code(p: &Path) -> String {
    fs::read_to_string(p).unwrap().lines().filter(|l| !l.trim_start().starts_with("//")).collect::<Vec<_>>().join("\n")
}

fn scan(dirs: &[&str], ext: &str, forbidden: &[&str]) {
    let mut hits = Vec::new();
    for d in dirs {
        let mut fs_ = Vec::new();
        files(&workspace().join(d), ext, &mut fs_);
        assert!(!fs_.is_empty(), "no .{ext} files under {d}");
        for f in fs_ {
            let src = code(&f);
            for tok in forbidden {
                if src.contains(tok) {
                    hits.push(format!("{}: {tok}", f.display()));
                }
            }
        }
    }
    assert!(hits.is_empty(), "security-review tripwire:\n{}", hits.join("\n"));
}

#[test]
fn no_webkit_page_to_native_channels() {
    scan(
        &["crates/snorkle-content/src", "src-tauri/src"],
        "rs",
        &[
            // WKUserContentController / script message channels
            "addScriptMessageHandler",
            "WKScriptMessageHandler",
            "addUserScript",
            "WKUserScript",
            "messageHandlers",
            // Custom URL schemes served to content
            "setURLSchemeHandler",
            "WKURLSchemeHandler",
            // Script evaluation in content returns page-controlled values
            "evaluateJavaScript",
            "callAsyncJavaScript",
            // Web extensions
            "WKWebExtension",
            // Downloads are a file-system channel; not supported yet
            "WKDownload",
        ],
    );
}

#[test]
fn no_extra_tauri_webviews_or_ipc_hooks() {
    scan(
        &["src-tauri/src", "crates/snorkle-content/src"],
        "rs",
        &[
            "WebviewWindowBuilder",
            "WebviewBuilder",
            "add_child",
            "initialization_script",
            "on_web_resource_request",
            "register_uri_scheme_protocol",
            "register_asynchronous_uri_scheme_protocol",
            "with_ipc_handler",
            "add_capability",
            "tauri::ipc::Channel",
        ],
    );
}

#[test]
fn chrome_ui_never_parses_markup_or_loads_remote_code() {
    scan(
        &["ui"],
        "js",
        &[
            "innerHTML",
            "outerHTML",
            "insertAdjacentHTML",
            "document.write",
            "eval(",
            "new Function",
            "import(",
            "http://",
            "https://",
            "setAttribute('on",
            "setAttribute(\"on",
            "srcdoc",
            "<iframe",
        ],
    );
    scan(&["ui"], "html", &["http://", "https://", "<iframe", "onclick=", "onload=", "<script>"]);
}

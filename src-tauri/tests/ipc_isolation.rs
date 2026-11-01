//! Security suite: page-to-app IPC isolation.
//!
//! Builds the real app (real command handlers, real tauri.conf.json and
//! capabilities/, compiled ACL) on Tauri's MockRuntime and sends IPC
//! requests the way a page would. Requirements under test:
//!
//! 1. No remote origin can invoke ANY app command or core/plugin command.
//! 2. Only the chrome webview (label "main") at the local origin can invoke
//!    app commands; any other webview label is denied even when local.
//! 3. The chrome itself has least privilege: only the listed app commands
//!    and event listen/unlisten; window/webview/path/etc. are denied.
//! 4. A request without the per-launch invoke key is ignored.
//!
//! Note: on iOS, content WKWebViews are not Tauri webviews at all and have
//! no IPC bridge injected; this suite covers the defence-in-depth layer
//! (Tauri's ACL) in case a remote page ever ends up in a Tauri webview.
//! The on-device probe in tests/device/ covers the content views.

use std::sync::Arc;

use snorkle_core::os::OsSupport;
use snorkle_lib::{make_browser, register, Shared, CHROME_LABEL, COMMANDS};
use tauri::ipc::{CallbackFn, InvokeBody};
use tauri::test::{get_ipc_response, mock_builder, MockRuntime, INVOKE_KEY};
use tauri::webview::InvokeRequest;
use tauri::{App, Manager, WebviewUrl, WebviewWindow, WebviewWindowBuilder};

fn app() -> App<MockRuntime> {
    let app = register(mock_builder()).build(tauri::generate_context!(test = true)).expect("build app");
    let shared = Shared::new(make_browser(OsSupport::Supported, 8 << 30), None, Box::new(|_| {}));
    app.manage(shared);
    app
}

fn window(app: &App<MockRuntime>, label: &str) -> WebviewWindow<MockRuntime> {
    match app.get_webview_window(label) {
        Some(w) => w,
        None => WebviewWindowBuilder::new(app, label, WebviewUrl::default()).build().expect("window"),
    }
}

/// Plausible arguments for each command, so that an allowed call fails
/// only for ACL reasons, never for argument deserialisation.
fn args(cmd: &str) -> serde_json::Value {
    use serde_json::json;
    match cmd {
        "navigate" => json!({ "tab": 1, "input": "https://example.com/" }),
        "new_tab" => json!({ "private": false }),
        "set_chrome_layout" => json!({ "top": 80.0, "left": 264.0, "right": 0.0, "bottom": 0.0, "overlay": false }),
        "set_console_capture" => json!({ "on": false }),
        "clear_website_data" => json!({ "kind": "cache" }),
        "ai_set_key" => json!({ "provider": "openai", "key": "sk-test" }),
        "ai_delete_key" | "ai_test" => json!({ "provider": "anthropic" }),
        "ai_write_script" => json!({ "provider": "anthropic", "model": "claude-opus-5-5", "domain": "example.com", "css": "", "js": "", "prompt": "x" }),
        "open_launcher" => json!({ "url": "https://example.com/" }),
        "remove_bookmark" => json!({ "url": "https://example.com/" }),
        "list_history" => json!({ "limit": 10 }),
        "bookmark_tab" => json!({ "tab": 1, "index": 0 }),
        "move_bookmark" => json!({ "url": "https://example.com/", "index": 0 }),
        "split_tabs" => json!({ "tab": 1, "with": 2 }),
        "set_view_mode" => json!({ "mode": "desaturate", "on": true }),
        "set_tab_view_exempt" => json!({ "tab": 1, "mode": "headers", "exempt": true }),
        "scroll_to_heading" => json!({ "tab": 1, "index": 0 }),
        "set_tab_headers" => json!({ "tab": 1, "on": true }),
        "delete_user_script" => json!({ "domain": "example.com" }),
        "save_user_script" => json!({ "domain": "example.com", "css": "", "js": "", "enabled": false }),
        "get_view_mode" | "get_console" | "clear_console" | "ai_status" | "list_user_scripts" | "list_pins" => json!({}),
        "pin_tab" => json!({ "tab": 1, "index": 0 }),
        "pin_bookmark" | "move_pin" => json!({ "url": "https://example.com/", "index": 0 }),
        "unpin" => json!({ "url": "https://example.com/", "bookmarkIndex": null }),
        "get_state" | "list_bookmarks" | "clear_history" => json!({}),
        _ => json!({ "tab": 1 }),
    }
}

fn request(cmd: &str, url: &str, body: serde_json::Value, key: &str) -> InvokeRequest {
    InvokeRequest {
        cmd: cmd.into(),
        callback: CallbackFn(0),
        error: CallbackFn(1),
        url: url.parse().expect("url"),
        body: InvokeBody::Json(body),
        headers: Default::default(),
        invoke_key: key.into(),
    }
}

fn is_acl_denial(err: &serde_json::Value) -> bool {
    let s = err.to_string();
    s.contains("not allowed")
}

/// Origins a malicious page could have. Includes look-alikes of the local
/// origin and of Tauri's internal hosts.
const REMOTE_ORIGINS: &[&str] = &[
    "https://evil.example/",
    "http://evil.example/",
    "https://localhost/",
    "http://localhost:1420/",
    "http://127.0.0.1/",
    "https://tauri.localhost.evil.example/",
    "https://evil.example/tauri://localhost/",
    "tauri://localhost.evil.example/",
    "tauri://evil.example/",
    "https://ipc.localhost.evil.example/",
];

/// Core plugin commands a hostile page would try first.
const PLUGIN_COMMANDS: &[&str] = &[
    "plugin:event|listen",
    "plugin:event|unlisten",
    "plugin:event|emit",
    "plugin:event|emit_to",
    "plugin:webview|create_webview",
    "plugin:webview|create_webview_window",
    "plugin:webview|webview_close",
    "plugin:webview|set_webview_size",
    "plugin:window|create",
    "plugin:window|close",
    "plugin:window|set_size",
    "plugin:window|set_always_on_top",
    "plugin:path|resolve_directory",
    "plugin:path|resolve",
    "plugin:app|version",
    "plugin:app|name",
    "plugin:app|app_hide",
    "plugin:resources|close",
    "plugin:image|new",
    "plugin:menu|new",
    "plugin:tray|new",
];

#[test]
fn remote_origins_cannot_invoke_any_app_command() {
    let app = app();
    let chrome = window(&app, CHROME_LABEL);
    for origin in REMOTE_ORIGINS {
        for cmd in COMMANDS {
            let res = get_ipc_response(&chrome, request(cmd, origin, args(cmd), INVOKE_KEY));
            match res {
                Err(e) => assert!(is_acl_denial(&e), "{cmd} from {origin}: unexpected error {e}"),
                Ok(_) => panic!("{cmd} was invocable from remote origin {origin}"),
            }
        }
    }
}

#[test]
fn remote_origins_cannot_invoke_plugin_commands() {
    let app = app();
    let chrome = window(&app, CHROME_LABEL);
    for origin in REMOTE_ORIGINS {
        for cmd in PLUGIN_COMMANDS {
            let res = get_ipc_response(&chrome, request(cmd, origin, serde_json::json!({}), INVOKE_KEY));
            assert!(res.is_err(), "{cmd} was invocable from remote origin {origin}");
            assert!(is_acl_denial(&res.unwrap_err()), "{cmd} from {origin} not denied by ACL");
        }
    }
}

#[test]
fn other_webviews_cannot_invoke_even_from_local_origin() {
    let app = app();
    for label in ["content", "probe", "main2", "MAIN", "main-1"] {
        let w = window(&app, label);
        for cmd in COMMANDS.iter().copied().chain(PLUGIN_COMMANDS.iter().copied()) {
            let res = get_ipc_response(&w, request(cmd, "tauri://localhost/", args(cmd), INVOKE_KEY));
            assert!(res.is_err(), "{cmd} was invocable from webview {label:?}");
            assert!(is_acl_denial(&res.unwrap_err()), "{cmd} in {label:?} not denied by ACL");
        }
    }
}

#[test]
fn chrome_can_invoke_exactly_its_commands() {
    let app = app();
    let chrome = window(&app, CHROME_LABEL);
    for cmd in COMMANDS {
        if let Err(e) = get_ipc_response(&chrome, request(cmd, "tauri://localhost/", args(cmd), INVOKE_KEY)) {
            assert!(!is_acl_denial(&e), "chrome was denied {cmd}: {e}");
        }
    }
}

#[test]
fn chrome_has_least_privilege_for_core_plugins() {
    let app = app();
    let chrome = window(&app, CHROME_LABEL);
    for cmd in PLUGIN_COMMANDS {
        let res = get_ipc_response(&chrome, request(cmd, "tauri://localhost/", serde_json::json!({}), INVOKE_KEY));
        let granted = matches!(*cmd, "plugin:event|listen" | "plugin:event|unlisten");
        match res {
            Err(e) if is_acl_denial(&e) => assert!(!granted, "chrome should be allowed {cmd}"),
            _ => assert!(granted, "chrome must not be allowed {cmd}"),
        }
    }
}

#[test]
fn unknown_commands_are_denied() {
    let app = app();
    let chrome = window(&app, CHROME_LABEL);
    for cmd in [
        "eval",
        "__cmd__",
        "navigate ",
        "Navigate",
        "plugin:snorkle|navigate",
        "plugin:shell|execute",
        "plugin:fs|read_file",
    ] {
        let res = get_ipc_response(&chrome, request(cmd, "tauri://localhost/", serde_json::json!({}), INVOKE_KEY));
        assert!(res.is_err(), "{cmd} unexpectedly succeeded");
    }
}

#[test]
fn wrong_invoke_key_is_ignored() {
    let app = app();
    let chrome = window(&app, CHROME_LABEL);
    // Tauri drops the request without responding; the mock helper surfaces
    // that as a panic/hang guard. We assert no state change instead.
    let shared = app.state::<Arc<Shared>>();
    let before = shared.view().tabs.len();
    let req = request("new_tab", "tauri://localhost/", args("new_tab"), "not-the-key");
    let handle = std::thread::spawn({
        let chrome = chrome.clone();
        move || {
            let _ = get_ipc_response(&chrome, req);
        }
    });
    std::thread::sleep(std::time::Duration::from_millis(300));
    assert_eq!(shared.view().tabs.len(), before, "request with wrong invoke key was executed");
    drop(handle);
}

//! Security suite: static checks on configuration that controls the
//! page-to-native boundary. A failure here means a security-relevant
//! setting changed and needs human review (docs/SECURITY_REVIEW.md).

use std::collections::BTreeSet;
use std::fs;
use std::path::Path;

use serde_json::Value;
use snorkle_lib::COMMANDS;

fn root() -> &'static Path {
    Path::new(env!("CARGO_MANIFEST_DIR"))
}

fn json(rel: &str) -> Value {
    serde_json::from_str(&fs::read_to_string(root().join(rel)).unwrap()).unwrap()
}

fn conf() -> Value {
    json("tauri.conf.json")
}

#[test]
fn only_the_chrome_capability_exists_and_is_enabled() {
    let files: BTreeSet<String> = fs::read_dir(root().join("capabilities"))
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    assert_eq!(files, BTreeSet::from(["chrome.json".to_string()]), "new capability files need security review");
    assert_eq!(conf()["app"]["security"]["capabilities"], serde_json::json!(["chrome"]));
}

#[test]
fn chrome_capability_is_local_only_and_scoped_to_main_webview() {
    let cap = json("capabilities/chrome.json");
    assert!(cap.get("remote").is_none(), "capabilities must never grant remote origins");
    assert_eq!(cap["local"], Value::Bool(true));
    assert_eq!(cap["webviews"], serde_json::json!(["main"]));
    assert_eq!(cap["windows"], serde_json::json!([]), "window-label grants would cover any webview in the window");
    assert!(cap.get("platforms").is_none() || cap["platforms"].as_array().unwrap().iter().all(|p| p != "*"));
}

#[test]
fn chrome_permissions_are_exactly_the_reviewed_set() {
    let cap = json("capabilities/chrome.json");
    let got: BTreeSet<String> =
        cap["permissions"].as_array().unwrap().iter().map(|p| p.as_str().unwrap().to_string()).collect();
    let mut want: BTreeSet<String> = COMMANDS.iter().map(|c| format!("allow-{}", c.replace('_', "-"))).collect();
    want.insert("core:event:allow-listen".into());
    want.insert("core:event:allow-unlisten".into());
    assert_eq!(got, want);
    assert!(got.iter().all(|p| !p.ends_with(":default") && !p.contains('*')));
}

#[test]
fn csp_is_strict() {
    let csp = &conf()["app"]["security"]["csp"];
    let get = |k: &str| csp[k].as_str().unwrap_or_else(|| panic!("CSP missing {k}")).to_string();
    assert_eq!(get("default-src"), "'none'");
    assert_eq!(get("script-src"), "'self'");
    assert_eq!(get("style-src"), "'self'");
    assert_eq!(get("frame-src"), "'none'");
    assert_eq!(get("object-src"), "'none'");
    assert_eq!(get("base-uri"), "'none'");
    assert_eq!(get("form-action"), "'none'");
    assert_eq!(get("connect-src"), "ipc: http://ipc.localhost");
    for (k, v) in csp.as_object().unwrap() {
        let v = v.as_str().unwrap();
        for bad in
            ["unsafe-inline", "unsafe-eval", "unsafe-hashes", "*", "https:", "data:", "blob:", "wasm-unsafe-eval"]
        {
            assert!(!v.split_whitespace().any(|t| t.contains(bad)), "CSP {k} contains {bad}");
        }
    }
}

#[test]
fn tauri_security_switches() {
    let c = conf();
    let sec = &c["app"]["security"];
    assert_eq!(sec["freezePrototype"], Value::Bool(true));
    assert_eq!(sec["dangerousDisableAssetCspModification"], Value::Bool(false));
    assert_eq!(sec["assetProtocol"]["enable"], Value::Bool(false));
    assert!(sec.get("headers").is_none());
    assert!(sec.get("pattern").is_none() || sec["pattern"]["use"] == "brownfield");
    // No dev server origin is ever trusted, no plugins configured.
    assert!(c["build"].get("devUrl").is_none());
    assert!(c.get("plugins").is_none());
    // Exactly one Tauri window/webview: the chrome.
    let windows = c["app"]["windows"].as_array().unwrap();
    assert_eq!(windows.len(), 1);
    assert_eq!(windows[0]["label"], "main");
    assert_eq!(windows[0]["url"], "index.html");
}

#[test]
fn minimum_ios_matches_core_policy() {
    let min = conf()["bundle"]["iOS"]["minimumSystemVersion"].as_str().unwrap().to_string();
    let m = snorkle_core::os::MINIMUM;
    assert_eq!(min, format!("{}.{}", m.major, m.minor));
}

#[test]
fn tauri_dependency_has_no_dynamic_acl() {
    let toml = fs::read_to_string(root().join("Cargo.toml")).unwrap();
    let line = toml.lines().find(|l| l.starts_with("tauri = ")).expect("tauri dependency");
    assert!(line.contains("default-features = false"), "default features include dynamic-acl");
    assert!(!line.contains("dynamic-acl"));
    assert!(!toml.contains("tauri-plugin-"), "Tauri plugins add IPC surface; review required");
}

#[test]
fn ios_plist_keeps_ats_for_app_and_requests_no_restricted_data() {
    let plist = fs::read_to_string(root().join("Info.ios.plist")).unwrap();
    assert!(plist.contains("<key>NSAllowsArbitraryLoadsInWebContent</key>"));
    assert!(!plist.contains("<key>NSAllowsArbitraryLoads</key>"));
    assert!(!plist.contains("NSExceptionDomains"));
    // Keys that get browser-entitlement apps rejected (Apple: "Preparing
    // your app to be the default web browser").
    for key in [
        "NSPhotoLibraryUsageDescription",
        "NSLocationAlwaysUsageDescription",
        "NSLocationAlwaysAndWhenInUseUsageDescription",
        "NSHomeKitUsageDescription",
        "NSBluetoothAlwaysUsageDescription",
        "NSHealthShareUsageDescription",
        "NSHealthUpdateUsageDescription",
    ] {
        assert!(!plist.contains(key), "{key} is not allowed for browser apps");
    }
    for scheme in ["<string>http</string>", "<string>https</string>"] {
        assert!(plist.contains(scheme), "default browsers must declare {scheme}");
    }
}

#[test]
fn entitlements_protect_data_at_rest() {
    let e = fs::read_to_string(root().join("ios/Entitlements.plist")).unwrap();
    assert!(e.contains("<key>com.apple.developer.default-data-protection</key>"));
    assert!(e.contains("NSFileProtectionComplete"));
    // Never the alternative-engine entitlements (BrowserEngineKit).
    assert!(!e.contains("web-browser-engine"));
}

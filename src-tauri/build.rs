include!("src/command_list.rs");

fn main() {
    // Declaring an app manifest makes Tauri check *every* app command against
    // the capabilities, including from local origins (without it, app
    // commands are allowed for any local webview by default).
    tauri_build::try_build(
        tauri_build::Attributes::new().app_manifest(tauri_build::AppManifest::new().commands(COMMANDS)),
    )
    .expect("failed to run tauri-build");
}

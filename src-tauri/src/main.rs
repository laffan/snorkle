// Prevents an extra console window on Windows release builds (desktop dev only).
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

fn main() {
    snorkle_lib::run()
}

//! The 700-line rule (owner decision 2026-11-23): no code or style file in
//! the repository is longer than 700 lines, so each stays readable on its
//! own and growth goes into new modules rather than longer files. Covers
//! Rust, JavaScript, CSS, HTML and shell scripts that are part of the
//! project (not generated files, build output, vendored filter lists or
//! icons).

use std::fs;
use std::path::{Path, PathBuf};

const MAX_LINES: usize = 700;
const EXTENSIONS: &[&str] = &["rs", "js", "css", "html", "sh"];
/// Folders searched, from the repository root.
const ROOTS: &[&str] = &["crates", "src-tauri/src", "src-tauri/tests", "src-tauri/build.rs", "ui", "scripts", "fuzz", "tests"];
/// Never searched: build output, generated projects, dependencies.
const SKIP_DIRS: &[&str] = &["target", "gen", "node_modules", "corpus", "artifacts", "coverage", ".git"];

fn repo_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).parent().expect("repository root").to_path_buf()
}

fn collect(path: &Path, out: &mut Vec<PathBuf>) {
    if path.is_file() {
        if path.extension().and_then(|e| e.to_str()).is_some_and(|e| EXTENSIONS.contains(&e)) {
            out.push(path.to_path_buf());
        }
        return;
    }
    let Ok(entries) = fs::read_dir(path) else { return };
    for entry in entries.flatten() {
        let p = entry.path();
        let name = entry.file_name();
        if p.is_dir() && SKIP_DIRS.iter().any(|s| name == *s) {
            continue;
        }
        collect(&p, out);
    }
}

#[test]
fn no_code_or_style_file_is_longer_than_700_lines() {
    let root = repo_root();
    let mut files = Vec::new();
    for r in ROOTS {
        collect(&root.join(r), &mut files);
    }
    assert!(files.len() > 50, "the walk found the sources ({} files)", files.len());
    let mut long: Vec<(usize, String)> = files
        .iter()
        .filter_map(|f| {
            let n = fs::read_to_string(f).ok()?.lines().count();
            (n > MAX_LINES).then(|| (n, f.strip_prefix(&root).unwrap_or(f).display().to_string()))
        })
        .collect();
    long.sort();
    assert!(
        long.is_empty(),
        "files over {MAX_LINES} lines (split them into modules):\n{}",
        long.iter().map(|(n, f)| format!("  {n:>5}  {f}")).collect::<Vec<_>>().join("\n")
    );
}

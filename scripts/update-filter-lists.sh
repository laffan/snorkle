#!/bin/sh
# Refreshes the bundled EasyList / EasyPrivacy filter lists and records their
# provenance in src-tauri/resources/filters/SOURCES.md. Review the diff (and
# the rule counts printed at the end) before committing.
set -eu
cd "$(dirname "$0")/.."
dir=src-tauri/resources/filters
tmp=$(mktemp -d)
trap 'rm -rf "$tmp"' EXIT
for name in easylist easyprivacy; do
  # Primary: easylist.to. Fallback: the EasyList project's GitHub Pages
  # branch, which is what easylist.to serves.
  curl -fsSL --max-time 60 -o "$tmp/$name.txt" "https://easylist.to/easylist/$name.txt" ||
    curl -fsSL --max-time 60 -o "$tmp/$name.txt" "https://raw.githubusercontent.com/easylist/easylist/gh-pages/$name.txt"
  head -1 "$tmp/$name.txt" | grep -q '^\[Adblock Plus' || { echo "$name: unexpected format" >&2; exit 1; }
  mv "$tmp/$name.txt" "$dir/$name.txt"
done
{
  echo "# Bundled filter lists"
  echo
  echo "| File | Version | Last modified | SHA-256 |"
  echo "|---|---|---|---|"
  for name in easylist easyprivacy; do
    f="$dir/$name.txt"
    v=$(grep -m1 '^! Version:' "$f" | cut -d' ' -f3-)
    m=$(grep -m1 '^! Last modified:' "$f" | cut -d' ' -f4-)
    h=$(shasum -a 256 "$f" 2>/dev/null || sha256sum "$f")
    echo "| $name.txt | $v | $m | ${h%% *} |"
  done
  echo
  cat "$dir/LICENSE.md"
} > "$dir/SOURCES.md"
cargo run -q --release -p snorkle-core --example compile_lists -- "$dir/easylist.txt" "$dir/easyprivacy.txt"

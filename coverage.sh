#!/usr/bin/env bash
# Measure how much of Headway the tests exercise.
#
# Runs every test with coverage switched on, plus the real program a few times
# (first launch, saving a picture, resetting a sample) in throwaway settings
# folders, then prints a per-file report. Needs llvm-profdata/llvm-cov matching
# the Rust compiler's LLVM version. Usage: ./coverage.sh [html]
set -euo pipefail
cd "$(dirname "$0")"

out=target/coverage
rm -rf "$out/profraw" "$out/html"
mkdir -p "$out/profraw"
export RUSTFLAGS="-C instrument-coverage"
export CARGO_TARGET_DIR="$out"
export LLVM_PROFILE_FILE="$PWD/$out/profraw/headway-%p-%m.profraw"

cargo test --quiet 2>&1 | grep -E "test result|FAILED|panicked" || true
cargo build --quiet

# The real program, with its own settings and data folders.
tmp=$(mktemp -d)
trap 'rm -rf "$tmp"' EXIT
export XDG_CONFIG_HOME="$tmp/config" XDG_DATA_HOME="$tmp/data"
"$out/debug/headway" --help >/dev/null
"$out/debug/headway" --screenshot "$tmp/first-run.png"      # first launch: builds the samples
"$out/debug/headway" --screenshot "$tmp/second-run.png"     # normal launch
"$out/debug/headway" --reset-sample web-shop >/dev/null
"$out/debug/headway" --reset-sample no-such-sample 2>/dev/null || true
"$out/debug/headway" --not-an-option --help >/dev/null                  # unknown options are ignored
"$out/debug/headway" --screenshot "/proc/no-such-dir/x.png" 2>/dev/null || true  # can't save: reports it
echo '{"workspaces":[],"current":0}' > "$XDG_CONFIG_HOME/headway/workspaces.json"
"$out/debug/headway" --screenshot "$tmp/empty-settings.png"                      # empty settings: starts fresh

llvm-profdata merge -sparse "$out"/profraw/*.profraw -o "$out/headway.profdata"
# Ask cargo which file is the test program (the plain build has a similar name).
tests=$(cargo test --no-run --message-format=json 2>/dev/null \
    | grep -o '"executable":"[^"]*"' | tail -1 | cut -d'"' -f4)
objects=(-object "$out/debug/headway" -object "$tests")
ignore='(\.cargo|rustc|/usr/|ui_tests\.rs)'
if [[ "${1:-}" == html ]]; then
    llvm-cov show "${objects[@]}" --instr-profile="$out/headway.profdata" --ignore-filename-regex="$ignore" \
        --format=html --output-dir="$out/html" --show-line-counts-or-regions
    echo "Report: $out/html/index.html"
fi
llvm-cov report "${objects[@]}" --instr-profile="$out/headway.profdata" --ignore-filename-regex="$ignore"

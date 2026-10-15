#!/usr/bin/env bash
# The gate: every change passes this before it counts as done (docs/DESIGN.md section 13).
# Run from anywhere: tools/gate.sh   (add --no-firmware to skip the Arduino build)
set -euo pipefail
cd "$(dirname "$0")/.."

step() { printf '\n== %s\n' "$1"; }

step "Rust formatting"
cargo fmt --all --check

step "Clippy (warnings are errors)"
cargo clippy --workspace --all-targets --quiet -- -D warnings

step "Rust tests"
cargo test --workspace --quiet

step "Firmware host tests"
out="$(mktemp -d)"
g++ -std=c++17 -Wall -Wextra -Werror -Ifirmware/scana-bridge \
    firmware/scana-bridge/test/test_bridge.cpp -o "$out/test_bridge"
"$out/test_bridge"
rm -rf "$out"

if [[ "${1:-}" != "--no-firmware" ]]; then
    step "Firmware build for the Uno (RAM must stay under 75%)"
    build="$(arduino-cli compile --fqbn arduino:avr:uno firmware/scana-bridge 2>&1)"
    echo "$build" | grep -E "^(Sketch|Global)"
    pct="$(echo "$build" | sed -n 's/^Global variables use [0-9]* bytes (\([0-9]*\)%).*/\1/p')"
    if [[ -z "$pct" || "$pct" -ge 75 ]]; then
        echo "RAM use ${pct:-unknown}% is not under 75%"
        exit 1
    fi
fi

step "Gate passed"

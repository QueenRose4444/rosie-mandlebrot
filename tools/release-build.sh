#!/usr/bin/env bash
# Release build with no local paths baked into the binary. Panic messages and
# debug info otherwise carry the full path of every source file, including the
# builder's home directory. Extra arguments go to `cargo build`.
set -euo pipefail
cd "$(dirname "$0")/.."

native() { if command -v cygpath >/dev/null 2>&1; then cygpath -w "$1"; else printf '%s' "$1"; fi; }
root=$(native "$PWD")
cargo_home=$(native "${CARGO_HOME:-$HOME/.cargo}")
rustup_home=$(native "${RUSTUP_HOME:-$HOME/.rustup}")

export RUSTFLAGS="${RUSTFLAGS:-} --remap-path-prefix=$root=. --remap-path-prefix=$cargo_home=cargo --remap-path-prefix=$rustup_home=rustup"
cargo build --release --locked "$@"

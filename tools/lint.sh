#!/bin/sh
# Check formatting and lints (rustfmt.toml, [lints] in Cargo.toml); warnings fail.
# Usage: tools/lint.sh [--fix]   (--fix formats and applies clippy's machine-applicable fixes first)
set -e
cd "$(dirname "$0")/.."
if [ "$1" = "--fix" ]; then
    cargo fmt
    cargo clippy --all-targets --fix --allow-dirty --allow-staged
fi
cargo fmt --check
cargo clippy --all-targets -- -D warnings

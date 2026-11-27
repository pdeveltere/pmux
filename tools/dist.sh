#!/bin/sh
# Build a shareable pmux.app zip: target/dist/pmux-<version>-macos-<arch>.zip
# Usage: tools/dist.sh
set -eu
cd "$(dirname "$0")/.."
VERSION=$(sed -n 's/^version = "\(.*\)"/\1/p' Cargo.toml | head -1)
ARCH=$(uname -m)
OUT=target/dist
ZIP="$OUT/pmux-$VERSION-macos-$ARCH.zip"

# Own target dir, with build paths remapped so the binary doesn't carry this machine's home or checkout path.
RUSTFLAGS="${RUSTFLAGS:-} --remap-path-prefix=$PWD=pmux --remap-path-prefix=$HOME=~" \
  cargo build --release --target-dir target/dist-build

rm -rf "$OUT/pmux.app"
mkdir -p "$OUT"
PMUX_INSTALL_BIN=target/dist-build/release/pmux PMUX_NO_REGISTER=1 tools/install-app.sh "$OUT"
rm -f "$ZIP"
ditto -c -k --norsrc --noextattr --keepParent "$OUT/pmux.app" "$ZIP"
(cd "$OUT" && shasum -a 256 "$(basename "$ZIP")" > "$(basename "$ZIP").sha256")
echo "built $ZIP"

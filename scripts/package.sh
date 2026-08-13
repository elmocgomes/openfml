#!/bin/sh
# Build a self-contained, installable OpenFML bundle:
#   scripts/package.sh            →  dist/openfml-<version>-<target>.tar.gz
# The bundle carries the three binaries, the web UI, the ACME example,
# a deployment template, an installer and the systemd unit.
set -e
cd "$(dirname "$0")/.."

VERSION=$(grep -m1 '^version' Cargo.toml | cut -d'"' -f2)
TARGET=$(rustc -vV | grep host | cut -d' ' -f2)
NAME="openfml-$VERSION-$TARGET"
OUT="dist/$NAME"

echo "building $NAME"
cargo build --release
cargo build --release --target wasm32-unknown-unknown
cp target/wasm32-unknown-unknown/release/openfml.wasm www/openfml.wasm

rm -rf "$OUT"
mkdir -p "$OUT/bin" "$OUT/www" "$OUT/deploy-template" "$OUT/examples/acme"
cp target/release/openfml target/release/openfml-server target/release/openfml-lsp "$OUT/bin/"
cp www/*.html www/*.wasm www/*.fml www/*.csv "$OUT/www/"
cp -R deploy-template/. "$OUT/deploy-template/"
cp -R models/acme/. "$OUT/examples/acme/"
cp scripts/openfml-server.service "$OUT/"
cp scripts/install.sh "$OUT/install.sh"
chmod +x "$OUT/install.sh" "$OUT/bin/"*
cp INSTALL.md "$OUT/INSTALL.md"

mkdir -p dist
tar -czf "dist/$NAME.tar.gz" -C dist "$NAME"
echo "→ dist/$NAME.tar.gz"

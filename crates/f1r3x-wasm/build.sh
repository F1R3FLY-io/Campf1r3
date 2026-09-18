#!/bin/sh
# Build the page's module. No wasm-bindgen, no toolchain beyond cargo.
set -e
cd "$(dirname "$0")"
rustup target add wasm32-unknown-unknown 2>/dev/null || true
cargo build --release --target wasm32-unknown-unknown -p f1r3x-wasm
cp ../../target/wasm32-unknown-unknown/release/f1r3x_wasm.wasm web/
echo "built web/f1r3x_wasm.wasm"
echo "serve web/ over http (a module cannot be fetched from file://):"
echo "    python3 -m http.server -d web 8080"

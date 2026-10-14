#!/bin/sh
# Builds DTF Prep and refreshes the intentionally committed ready-to-install binary.
# Needs the wasm32-unknown-unknown target: `rustup target add wasm32-unknown-unknown`.
set -eu
cd "$(dirname "$0")"
cargo test
cargo build --release --target wasm32-unknown-unknown
out=target/wasm32-unknown-unknown/release/photocraft_plugin_dtf_prep.wasm
mkdir -p ../../../dist
cp "$out" ../../../dist/dtf-prep.wasm
echo "built $out ($(wc -c < "$out") bytes) -> dist/dtf-prep.wasm"

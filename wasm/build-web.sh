#!/usr/bin/env bash
# Build the release wasm and place it where the site serves it.
set -euo pipefail
cd "$(dirname "$0")"
./spike.sh
mkdir -p ../site/public
cp target/wasm32-wasip1/release/pgsafe-wasm.wasm ../site/public/pgsafe.wasm
echo "wrote site/public/pgsafe.wasm ($(du -h ../site/public/pgsafe.wasm | cut -f1))"

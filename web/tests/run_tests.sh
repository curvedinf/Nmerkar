#!/bin/sh
# Build the wasm + web layer, then run the JS parity gate.
set -e
cd "$(dirname "$0")/../.."
sh web/build.sh
node web/tests/run_js.cjs

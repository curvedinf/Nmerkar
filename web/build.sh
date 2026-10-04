#!/bin/sh
# Build the web layer in place:
#   1. cargo build the compiler for wasm32-unknown-unknown (nk.wasm)
#   2. inline it (base64) into web/nk.wrapper.js  -> web/nk.js
#   3. bake the example sources into web/example.template.html
#                                        -> web/example.html
#
# The built files are the deliverable: web/example.html + web/nk.js work
# from file:// (the selector lists the repo's existing example programs,
# baked in at build time). web/nk.wrapper.js and web/example.template.html
# are the build inputs (templates with placeholders).
set -e
cd "$(dirname "$0")/.."

# --lib: the cdylib is the artifact (nk.wasm). Building the bin too collides
# with the lib's output filename on this target.
cargo build --release --lib --target wasm32-unknown-unknown --manifest-path comp/Cargo.toml

python3 - <<'PY'
import base64, json, pathlib

root = pathlib.Path(".")
wasm = root / "comp" / "target" / "wasm32-unknown-unknown" / "release" / "nk.wasm"
b64 = base64.b64encode(wasm.read_bytes()).decode("ascii")

nk = (root / "web" / "nk.wrapper.js").read_text().replace("__WASM_B64__", b64)
(root / "web" / "nk.js").write_text(nk)

# The selector's sample set: existing repo programs that run in the browser
# (no shell, no file I/O, no wall-clock, no infinite loops). shelltest needs
# node's child_process; text/chat runs forever; examples/gnu needs file I/O.
SAMPLES = [
    "examples/hello.n",
    "examples/fib.n",
    "examples/dot.n",
    "examples/maptest.n",
    "examples/matmul.n",
    "examples/memtest.n",
    "examples/ring.n",
    "examples/strtest.n",
    "examples/weave.n",
    "examples/text/matmul.n",
]
samples = {}
for p in SAMPLES:
    samples[p[: -len(".n")]] = (root / p).read_text()
payload = json.dumps(samples).replace("</", "<\\/")
page = (root / "web" / "example.template.html").read_text()
page = page.replace("__EX_SAMPLES__", payload)
(root / "web" / "example.html").write_text(page)
print("wrote", root / "web" / "nk.js", "and", root / "web" / "example.html")
PY

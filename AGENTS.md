# AGENTS.md — Nmerkar

Nmerkar: a language based on a managed hidden stack, compiled to C then native via `cc`; the same source can also be compiled to JavaScript via `nk --emit-js` for in-browser execution (SPEC "JavaScript target"). Designed for LLM-authored scripts (low token count, fast, reliable). Current revision is **v15** — **not backward compatible** with v14.1 (bare names are implicit loads — the `@` varget suffix is gone; dense label defs need the 🏷 marker; dense struct immediates use the 📏 📍 📦 🎭 tag glyphs, see SPEC "Names, labels, and variables" and "Dense tag glyphs"). Debugging: every runtime error carries its source site (`nk: L<line> [label] <msg>`), plus the `dbg`/`check`/`timer`/`memstats` ops and the `--profile`/`--coverage` cost-center flags (native target; SPEC "Debugging and profiling"). Full language spec in `SPEC.md` (current specification only; per-operation history is in `SPEC_HISTORY.md`); v13 design rationale in `SPEC_V13_PROPOSAL.md`.

## Repository Layout

```
comp/      Rust compiler (→ C → native, or → JavaScript). Std-only modules in src/, no external crates.
web/       JavaScript target delivery: nk.js + example.html (committed built deliverables, openable from file://), nk.wrapper.js + example.template.html (build-input templates; nk.wrapper.js has the __WASM_B64__ placeholder, the template has __EX_SAMPLES__), build.sh (regenerates nk.js + example.html in place from the templates), tests/ (78-gate native-vs-JS parity suite: run_tests.sh = build + `node run_js.cjs`).
trans/     C→Nmerkar transpiler (Rust crate, std-only; emits text encoding).
examples/  Sample programs in both encodings.
mods/      FFI binding manifests (.ufm).
bench/     Cross-language benchmark suite.
SPEC.md    Normative language spec (~1200 lines, ~170+ ops).
README.md  Quickstart and project intro.
SPEC_V11_PROPOSAL.md, WEAVE_SPEC_PROPOSAL.md  Design proposals.
```

Compiler source map: `main.rs`/`nks.rs` (thin bin roots), `driver.rs` (CLI/cache/cc + sandbox/compute wiring), `lex.rs` (lexers + glyph/mnemonic tables), `parse.rs` (parser, label resolution, WEAVE DAG, label parameters/destructuring, strict arity check, v14 scope pass: shared-var declaration + name-collision checks + atomic RMW rewrite, v15 implicit-load resolution (bare Ident → macro-or-load), v14.1 strictness pass: static strict/loose taint + implicit-coercion rejection, `_cast` literal folding), `ast.rs` (types), `gen.rs` (C codegen + optimizations), `gen_js.rs` (JavaScript-target codegen: full-op emission, segment emission, for/while inlining semantics matching the C target, weave Kahn ordering), `js_prelude.rs` (embedded JS runtime: 1:1 op table, cooperative threads, FFI/libc emulation, node shell shim), `wasm_api.rs` (C-ABI exports for the WASM compiler build + panic-abort message protocol), `lib.rs` (library root: `compile_js(src) -> String`), `emit.rs` (encoding conversion), `prelude.rs` (embedded C runtime: GC, containers, opcodes, threading, coercion, smart print, sandbox gates, Vulkan offload + staging pool), `sandbox.rs` (capability configs/policies), `compute.rs` (Vulkan compute backend, static shader library, weave-task + elementwise-region fusion), `build.rs` (bakes NK_SANDBOX_CONFIG).

## Build

```sh
cd comp && cargo build --release      # binaries at comp/target/release/nk and nks
cargo install --path comp             # install nk + nks to ~/.cargo/bin (on PATH via rustup)
```

The web bundle is regenerated in place by `bash web/build.sh`: it
cross-builds the compiler for `wasm32-unknown-unknown`, inlines it (base64)
into `web/nk.js`, and bakes the example sources into `web/example.html` —
the committed deliverables (self-contained, openable from `file://`).

`nk` is the unrestricted compiler. `nks` is the sandboxed build: it always
bakes a capability config (repo default `comp/sandbox.ufs`; override both
binaries with `NK_SANDBOX_CONFIG=<file.ufs>` at build time). See SPEC.md
"Sandboxing and capabilities" for policies (`pure`/`data`/`web`/`build`),
`.ufs` config files, `--policy/--sandbox/--workspace/--caps`, and the
filesystem workspace. GPU compute offloading (Vulkan) is automatic when glslc
is present; `--device cpu` disables it (SPEC.md "GPU compute offloading").

No external Rust crates. Edition 2021, release profile `opt-level = 2`. Debug build: `cargo build` → `comp/target/debug/nk`.

## Running Programs

`nk --help` has the full CLI. Common forms: `nk prog.n` (compile+run), `nk -c prog.n -o bin`, `nk --emit-c prog.n`, `nk --emit-js prog.n` (single input; `-o` appends `.js`; emits a runnable JS module with a `__NK_API` handle), `nk --to-text`/`--to-dense` (encoding conversion), directory mode (auto-discovers `main.n` + `init.n` subdirs), inline source, multi-TU (multiple files in one invocation). See also `README.md`.

Runtime flags: `--gc-threshold N`, `--gc-off`, `--mt`.

## Testing

**No automated test runner, no Rust unit tests.** All tests are manual — compile and run, verify output by eye.

- **Integration tests**: `comp/tests/t01_basic.n` … `t09_weave.n`, plus `t12_matrix.n` (polymorphic matrices), `t13_compute.n` (GPU offloading; run with `--gc-threshold 200000000`), `t14_task_gpu.n` (fused weave-task GPU kernels), `t15_shared.n` (v14 shared vars: 4 spawned workers × atomic `cnt++`, exact totals every run), `t16_auto_device.n` (auto first-run CPU vs GPU policy: N=512 matches `--device cpu`; N=2048 may use GPU), `t17_strict.n` (v14.1 strictness + casting: strict/loose taint, contagion through ops/calls/callbacks, explicit conversion via `cast`/`parse_int`, static `_cast` folding), `t18_debug.n` (debugging quartet: `dbg` stderr peek, `check` assert, `timer` bounds, `memstats` — parity of the `dbg L<line> [label]` site format), and `t19_checkfail.n` (failing `check`: rc=1 + byte-identical site message on both targets). One per feature area. Run: `nk comp/tests/tNN_*.n`
- **Transpiler tests**: `bash trans/run_tests.sh` — two gated pathways: C→Nmerkar (`trans/trans`) → `nk` → run, compared against system binaries (echo/true/false/wc/yes) or expected-output files (hello, mini_gen); and `trans/tests/ops/` per-operation gates (transpile+compile+run with rc/stdout/stderr checks). All 23 pass.
- **JavaScript target gate**: `bash web/tests/run_tests.sh` — rebuilds the web bundle, then runs the full 78-gate parity suite (`node web/tests/run_js.cjs`): `comp/tests` t01–t19, all `examples/` (incl. the GNU single-file transpiler targets and `text/`), all nine `bench/src` programs, and the `web/tests/*.n` micro-gates (`neg_*.n` must be rejected by both targets). Each gate compares rc/stdout/stderr of the native `nk` oracle and the dist bundle, with per-gate compiler flags/`argv`/`stdin`/fixture `fs`. Corpus exclusions: the multi-TU fixtures (`examples/mtu/`, `examples/dirtest/`) and `text/chat.n` (compile-only; it loops forever); shell ops run under node. All 78 gates pass.
- **GNU multi-file tools**: `bash trans/tests/gnu/run.sh` — six multi-file coreutils adaptations (cat, head, nl, tee, cksum, base64) transpiled in one multi-file invocation and gated against the system binaries; transpiled `.n` exported to `examples/gnu/`.
- **Benchmarks**: `cd bench && python3 run.py` (needs `.bench-venv/` with `transformers`; data in `bench/data/` is gitignored). Nine benchmarks × 5 languages: logextract, analytics, mandelbrot, spectralnorm, nqueens (iterative backtracking), bfs (packed CSR), and dynamicgraph (hash map + dynamic neighbor lists) are CPU-oriented; matmul and blackscholes also include a GPU-on column (Nmerkar auto device; see `bench/SPEC.md`).

## Language & FFI Reference

All semantics, control flow, variable scoping (v15: `x!` stores with pass-through, bare names are implicit loads, `x@` is removed; shared vars are plain names declared by a top-level assignment — atomic, thread-safe), structured concurrency (`spawn`/`channel`/`weave`), container protocol, string escapes, module system (`USE`/`import`/`extern`/`MOD`/`PUB`), directory mode, universal coercion, strictness (`strict`/`loose`, SPEC "Strictness"), casting (`_cast` static / `cast` dynamic, SPEC "Casting"), and smart `print` are in **`SPEC.md`**. Opcode→glyph/mnemonic tables are in `comp/src/lex.rs` (`OP_NAMES`, `OP_GLYPHS`, `text_mnemonic`).

Key gotchas not to re-derive: string escapes limited to `\n \t \r \0 \\ \"`. Linking is always `-lpthread -lm` plus `-l<name>` per `USE`. Immediate-operand opcodes (`_call`, `_addr`, `_syscall`, `_lit`, `_str`, `_size_of`, `_offset`, `_obj`, `_cast`, `_array`, `_tensor`) are `_`-prefixed in text mode — they consume the next source token at compile time. Every label body must end with `ret`. Identifiers may not start with `_`.

## Development Notes

- No CI/CD, no formatter/linter.
- Experimental, under active development.
- Text (`.n`) is the default source encoding; dense (`.nd`) is experimental — glyphs optimized for Qwen3-0.6B tokenizer (single-token per glyph).
- Transpiler is a Rust crate (`cd trans && cargo build --release` → `target/release/trans`); unit tests via `cargo test`, behavior via `bash trans/run_tests.sh`.
- Compiler cache key includes its own binary mtime — rebuilding auto-invalidates old cached outputs.
- **Any change to language semantics, opcodes, encodings, or behavior must be documented in `SPEC.md` in the same changeset.**

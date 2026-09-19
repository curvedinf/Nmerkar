# AGENTS.md — Enmerkar

Enmerkar: a language based on a managed hidden stack, compiled to C then native via `cc`. Designed for LLM-authored scripts (low token count, fast, reliable). Current revision is **v13** — **not backward compatible** with v12. Full language spec in `SPEC.md`; v13 design rationale in `SPEC_V13_PROPOSAL.md`.

## Repository Layout

```
comp/      Rust compiler (→ C → native). Std-only modules in src/ (11 files), no external crates.
trans/     C→Enmerkar transpiler (Rust crate, std-only; emits text encoding).
examples/  Sample programs in both encodings.
mods/      FFI binding manifests (.ufm).
bench/     Cross-language benchmark suite.
SPEC.md    Normative language spec (~1200 lines, ~170+ ops).
README.md  Quickstart and project intro.
SPEC_V11_PROPOSAL.md, WEAVE_SPEC_PROPOSAL.md  Design proposals.
```

Compiler source map: `main.rs`/`nkrsb.rs` (thin bin roots), `driver.rs` (CLI/cache/cc + sandbox/compute wiring), `lex.rs` (lexers + glyph/mnemonic tables), `parse.rs` (parser, label resolution, WEAVE DAG, v13 label parameters/destructuring, strict arity check), `ast.rs` (types), `gen.rs` (C codegen + optimizations), `emit.rs` (encoding conversion), `prelude.rs` (embedded C runtime: GC, containers, opcodes, threading, coercion, smart print, sandbox gates, Vulkan offload + staging pool), `sandbox.rs` (capability configs/policies), `compute.rs` (Vulkan compute backend, static shader library, weave-task + elementwise-region fusion), `build.rs` (bakes NKR_SANDBOX_CONFIG).

## Build

```sh
cd comp && cargo build --release      # binaries at comp/target/release/nkr and nkrsb
cargo install --path comp             # install nkr + nkrsb to ~/.cargo/bin (on PATH via rustup)
```

`nkr` is the unrestricted compiler. `nkrsb` is the sandboxed build: it always
bakes a capability config (repo default `comp/sandbox.ufs`; override both
binaries with `NKR_SANDBOX_CONFIG=<file.ufs>` at build time). See SPEC.md
"Sandboxing and capabilities" for policies (`pure`/`data`/`web`/`build`),
`.ufs` config files, `--policy/--sandbox/--workspace/--caps`, and the
filesystem workspace. GPU compute offloading (Vulkan) is automatic when glslc
is present; `--device cpu` disables it (SPEC.md "GPU compute offloading").

No external Rust crates. Edition 2021, release profile `opt-level = 2`. Debug build: `cargo build` → `comp/target/debug/nkr`.

## Running Programs

`nkr --help` has the full CLI. Common forms: `nkr prog.en` (compile+run), `nkr -c prog.en -o bin`, `nkr --emit-c prog.en`, `nkr --to-text`/`--to-dense` (encoding conversion), directory mode (auto-discovers `main.en` + `init.en` subdirs), inline source, multi-TU (multiple files in one invocation). See also `README.md`.

Runtime flags: `--gc-threshold N`, `--gc-off`, `--mt`.

## Testing

**No automated test runner, no Rust unit tests.** All tests are manual — compile and run, verify output by eye.

- **Integration tests**: `comp/tests/t01_basic.ent` … `t09_weave.ent`, plus `t12_matrix.ent` (polymorphic matrices), `t13_compute.ent` (GPU offloading; run with `--gc-threshold 200000000`) and `t14_task_gpu.ent` (fused weave-task GPU kernels). One per feature area. Run: `nkr comp/tests/tNN_*.ent`
- **Transpiler tests**: `bash trans/run_tests.sh` — two gated pathways: C→Enmerkar (`trans/trans`) → `nkr` → run, compared against system binaries (echo/true/false/wc/yes) or expected-output files (hello, mini_gen); and `trans/tests/ops/` per-operation gates (transpile+compile+run with rc/stdout/stderr checks). All 21 pass.
- **GNU multi-file tools**: `bash trans/tests/gnu/run.sh` — six multi-file coreutils adaptations (cat, head, nl, tee, cksum, base64) transpiled in one multi-file invocation and gated against the system binaries; transpiled `.ent` exported to `examples/gnu/`.
- **Benchmarks**: `cd bench && python3 run.py` (needs `.bench-venv/` with `transformers`; data in `bench/data/` is gitignored). Six benchmarks × 5 languages: logextract, analytics, mandelbrot, spectralnorm (all CPU-only; Enmerkar pinned with `--device cpu`) plus the GPU-oriented matmul and blackscholes with a GPU-on column (Enmerkar auto device; see `bench/SPEC.md`).

## Language & FFI Reference

All semantics, control flow, variable scoping (`x!`/`x@` locals with pass-through assignment, `^x!`/`^x@` globals), structured concurrency (`spawn`/`chan`/`weave`), container protocol, string escapes, module system (`USE`/`import`/`extern`/`MOD`/`PUB`), directory mode, universal coercion, and smart `print` are in **`SPEC.md`**. Opcode→glyph/mnemonic tables are in `comp/src/lex.rs` (`OP_NAMES`, `OP_GLYPHS`, `text_mnemonic`).

Key gotchas not to re-derive: raw jumps (`jmp`/`jz`/`je`) and stack-manipulation opcodes (`dup`/`ovr`/`drop`/`swp`/`pick`) are removed — compile errors. Inline math symbols (`+ - * &`) are removed — use `add sub mul and`. String escapes limited to `\n \t \r \0 \\ \"`. Linking is always `-lpthread -lm` plus `-l<name>` per `USE`. Immediate-operand opcodes (`_call`, `_addr`, `_syscall`, `_lit`, `_str`, `_size_of`, `_offset`, `_obj`, `_cast`, `_array`, `_tensor`) are `_`-prefixed in text mode — they consume the next source token at compile time. Every label body must end with `ret`. Identifiers may not start with `_`.

## Development Notes

- No CI/CD, no formatter/linter.
- Experimental, under active development.
- Dense glyphs optimized for Qwen3-0.6B tokenizer (single-token per glyph).
- Transpiler is a Rust crate (`cd trans && cargo build --release` → `target/release/trans`); unit tests via `cargo test`, behavior via `bash trans/run_tests.sh`.
- Compiler cache key includes its own binary mtime — rebuilding auto-invalidates old cached outputs.
- **Any change to language semantics, opcodes, encodings, or behavior must be documented in `SPEC.md` in the same changeset.**

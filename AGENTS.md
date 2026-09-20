# AGENTS.md — Nmerkar

Nmerkar: a language based on a managed hidden stack, compiled to C then native via `cc`. Designed for LLM-authored scripts (low token count, fast, reliable). Current revision is **v14** — **not backward compatible** with v13 (shared vars are plain names, see SPEC "Shared variables"). Full language spec in `SPEC.md`; v13 design rationale in `SPEC_V13_PROPOSAL.md`.

## Repository Layout

```
comp/      Rust compiler (→ C → native). Std-only modules in src/ (11 files), no external crates.
trans/     C→Nmerkar transpiler (Rust crate, std-only; emits text encoding).
examples/  Sample programs in both encodings.
mods/      FFI binding manifests (.ufm).
bench/     Cross-language benchmark suite.
SPEC.md    Normative language spec (~1200 lines, ~170+ ops).
README.md  Quickstart and project intro.
SPEC_V11_PROPOSAL.md, WEAVE_SPEC_PROPOSAL.md  Design proposals.
```

Compiler source map: `main.rs`/`nks.rs` (thin bin roots), `driver.rs` (CLI/cache/cc + sandbox/compute wiring), `lex.rs` (lexers + glyph/mnemonic tables), `parse.rs` (parser, label resolution, WEAVE DAG, label parameters/destructuring, strict arity check, v14 scope pass: shared-var declaration + name-collision checks + atomic RMW rewrite, v14.1 strictness pass: static strict/loose taint + implicit-coercion rejection, `_cast` literal folding), `ast.rs` (types), `gen.rs` (C codegen + optimizations), `emit.rs` (encoding conversion), `prelude.rs` (embedded C runtime: GC, containers, opcodes, threading, coercion, smart print, sandbox gates, Vulkan offload + staging pool), `sandbox.rs` (capability configs/policies), `compute.rs` (Vulkan compute backend, static shader library, weave-task + elementwise-region fusion), `build.rs` (bakes NK_SANDBOX_CONFIG).

## Build

```sh
cd comp && cargo build --release      # binaries at comp/target/release/nk and nks
cargo install --path comp             # install nk + nks to ~/.cargo/bin (on PATH via rustup)
```

`nk` is the unrestricted compiler. `nks` is the sandboxed build: it always
bakes a capability config (repo default `comp/sandbox.ufs`; override both
binaries with `NK_SANDBOX_CONFIG=<file.ufs>` at build time). See SPEC.md
"Sandboxing and capabilities" for policies (`pure`/`data`/`web`/`build`),
`.ufs` config files, `--policy/--sandbox/--workspace/--caps`, and the
filesystem workspace. GPU compute offloading (Vulkan) is automatic when glslc
is present; `--device cpu` disables it (SPEC.md "GPU compute offloading").

No external Rust crates. Edition 2021, release profile `opt-level = 2`. Debug build: `cargo build` → `comp/target/debug/nk`.

## Running Programs

`nk --help` has the full CLI. Common forms: `nk prog.n` (compile+run), `nk -c prog.n -o bin`, `nk --emit-c prog.n`, `nk --to-text`/`--to-dense` (encoding conversion), directory mode (auto-discovers `main.n` + `init.n` subdirs), inline source, multi-TU (multiple files in one invocation). See also `README.md`.

Runtime flags: `--gc-threshold N`, `--gc-off`, `--mt`.

## Testing

**No automated test runner, no Rust unit tests.** All tests are manual — compile and run, verify output by eye.

- **Integration tests**: `comp/tests/t01_basic.n` … `t09_weave.n`, plus `t12_matrix.n` (polymorphic matrices), `t13_compute.n` (GPU offloading; run with `--gc-threshold 200000000`), `t14_task_gpu.n` (fused weave-task GPU kernels), `t15_shared.n` (v14 shared vars: 4 spawned workers × atomic `cnt++`, exact totals every run), `t16_auto_device.n` (auto first-run CPU vs GPU policy: N=512 matches `--device cpu`; N=2048 may use GPU), and `t17_strict.n` (v14.1 strictness + casting: strict/loose taint, contagion through ops/calls/callbacks, explicit conversion via `cast`/`parse_int`, static `_cast` folding). One per feature area. Run: `nk comp/tests/tNN_*.n`
- **Transpiler tests**: `bash trans/run_tests.sh` — two gated pathways: C→Nmerkar (`trans/trans`) → `nk` → run, compared against system binaries (echo/true/false/wc/yes) or expected-output files (hello, mini_gen); and `trans/tests/ops/` per-operation gates (transpile+compile+run with rc/stdout/stderr checks). All 23 pass.
- **GNU multi-file tools**: `bash trans/tests/gnu/run.sh` — six multi-file coreutils adaptations (cat, head, nl, tee, cksum, base64) transpiled in one multi-file invocation and gated against the system binaries; transpiled `.n` exported to `examples/gnu/`.
- **Benchmarks**: `cd bench && python3 run.py` (needs `.bench-venv/` with `transformers`; data in `bench/data/` is gitignored). Nine benchmarks × 5 languages: logextract, analytics, mandelbrot, spectralnorm, nqueens (iterative backtracking), bfs (packed CSR), and dynamicgraph (hash map + dynamic neighbor lists) are CPU-oriented; matmul and blackscholes also include a GPU-on column (Nmerkar auto device; see `bench/SPEC.md`).

## Language & FFI Reference

All semantics, control flow, variable scoping (v14: `x!`/`x@` locals with pass-through assignment; shared vars are plain names declared by a top-level assignment — atomic, thread-safe), structured concurrency (`spawn`/`channel`/`weave`), container protocol, string escapes, module system (`USE`/`import`/`extern`/`MOD`/`PUB`), directory mode, universal coercion, strictness (`strict`/`loose`, SPEC "Strictness"), casting (`_cast` static / `cast` dynamic, SPEC "Casting"), and smart `print` are in **`SPEC.md`**. Opcode→glyph/mnemonic tables are in `comp/src/lex.rs` (`OP_NAMES`, `OP_GLYPHS`, `text_mnemonic`).

Key gotchas not to re-derive: string escapes limited to `\n \t \r \0 \\ \"`. Linking is always `-lpthread -lm` plus `-l<name>` per `USE`. Immediate-operand opcodes (`_call`, `_addr`, `_syscall`, `_lit`, `_str`, `_size_of`, `_offset`, `_obj`, `_cast`, `_array`, `_tensor`) are `_`-prefixed in text mode — they consume the next source token at compile time. Every label body must end with `ret`. Identifiers may not start with `_`.

## Development Notes

- No CI/CD, no formatter/linter.
- Experimental, under active development.
- Text (`.n`) is the default source encoding; dense (`.nd`) is experimental — glyphs optimized for Qwen3-0.6B tokenizer (single-token per glyph).
- Transpiler is a Rust crate (`cd trans && cargo build --release` → `target/release/trans`); unit tests via `cargo test`, behavior via `bash trans/run_tests.sh`.
- Compiler cache key includes its own binary mtime — rebuilding auto-invalidates old cached outputs.
- **Any change to language semantics, opcodes, encodings, or behavior must be documented in `SPEC.md` in the same changeset.**

# µFlux

A scripting language that gives you **Python convenience at near-native speed**.

When an LLM writes Python for a data task, it's slow. When it writes C++,
it spends 3× the tokens on boilerplate. µFlux splits the difference: fewer
tokens than C++/Rust, 3–4× faster than Python, built-in ops for everything.

## Language Attributes

| Attribute | µFlux |
|---|---|
| Execution | Compiled (µFlux → C → native binary) |
| Typing | Dynamic and weak |
| Memory | Garbage collected |
| Primitives | int, float, str, ptr |
| Data structures | list, dict, arr, tensor, chan, atom, obj, bitmap, bloom, iter |
| C Imports | Zero-overhead, no glue |
| Concurrency | Channels, threads, dataflow DAGs with fanout |
| Built-ins | Regex, JSON, shell, I/O, streaming |

### Tokens to write (fewer = cheaper LLM calls)

| | µFlux | C++ | Rust | Python | Node.js |
|---|---|---|---|---|---|
| logextract | 332 | 846 | 667 | 329 | 437 |
| analytics | 348 | 793 | 756 | 366 | 471 |
| mandelbrot | 175 | 189 | 216 | 163 | 165 |
| spectralnorm | 442 | 350 | 487 | 258 | 390 |
| matmul | 157 | 244 | 278 | 172 | 235 |
| blackscholes | 1368 | 764 | 834 | 748 | 749 |
| **total (4 CPU benches)** | **1297** | **2178** | **2126** | **1116** | **1463** |
| **total (all 6)** | **2822** | **3186** | **3238** | **2036** | **2447** |

**33% fewer tokens than C++, 32% fewer than Rust** — the languages that match
its speed. More than Python, but Python is 3–4× slower.

Token counts use the **Qwen3-0.6B** tokenizer (vocab = 151,643). µFlux dense glyphs are each a single token in this vocab.

### Speed (seconds, lower = faster)

| | µFlux | C++ | Rust | Python | Node.js |
|---|---|---|---|---|---|
| logextract (510 MB) | **1.05** | 0.42 | 0.72 | 3.82 | 3.25 |
| analytics (512 MB) | **1.19** | 1.05 | 1.82 | 4.09 | 3.66 |
| mandelbrot | **0.10** | 0.05 | 0.05 | 3.98 | 0.07 |
| spectralnorm | **1.14** | 1.12 | 1.10 | 136.4 | 1.64 |
| matmul (N=512, CPU) | 0.69 | **0.05** | 0.03 | 0.16 | 0.14 |
| blackscholes (N=2M, CPU) | 0.72 | **0.05** | 0.04 | 0.16 | 0.07 |
| **total (4 CPU benches)** | **3.48** | **2.64** | **3.69** | **148.3** | **8.62** |

**3.7× faster than Python on data tasks**, within 1.1–2.5× of hand-tuned C++.
The two GPU-oriented rows run CPU-only in this table for comparability.

### GPU offloading (automatic, v13.1)

Same µFlux sources, default `auto` device (Radeon RX 7900 XTX, float64
kernels), steady-state timings — no code changes between the columns:

| workload | CPU | GPU-on |
|---|---|---|
| matmul N=512 | 0.07s | 0.10s |
| matmul N=1024 | 0.49s | **0.28s** |
| matmul N=2048 | 4.38s | **1.02s** |
| blackscholes N=2M (weave, fused task) | 0.97s | **0.42s** |

Large single-op workloads win big (4.3× at N=2048, crossover ≈ N=1024). And
**weave tasks compile to fused kernels**: the same black-scholes chain wrapped
in a weave task (`comp/tests/t14_task_gpu.uft`) runs as a handful of fused
kernels instead of ~60 per-op launches — 0.42s on GPU vs 0.97s CPU and 7.3s
unfused. Top-level op chains still pay per-op staging (device-resident
intermediates remains future work, bench/SPEC.md).

## Quick start

```sh
cd comp && cargo build --release
./comp/target/release/uf '"Hello!\n" print'
cargo install --path comp       # optional: install uf to PATH
```

## Usage

```sh
uf '"Hello µFlux!" print'      # run an inline program (cached)
uf prog.uf                     # compile + run (cached)
uf -c prog.uf -o hello         # compile to standalone binary
uf --emit-c prog.uf            # dump the generated C
uf --to-text prog.uf           # convert dense → text
uf somedir/                    # directory mode (auto-discovers main + init threads)
```

## Example

128×128 matrix multiply, text encoding — no headers, no memory management,
no imports, no declarations:

```
fi:  row! 128 'fe for ret
fe:  col! row@ 128 mul col@ add ix!
     ^A@ ix@ row@ col@ add set
     ^B@ ix@ row@ col@ sub set ret
cr:  row! 128 'dc for ret
dc:  col! 0 acc! 128 'ij for
     ^C@ row@ 128 mul col@ add acc@ set ret
ij:  j! ^A@ row@ 128 mul j@ add get
     ^B@ j@ 128 mul col@ add get
     mul acc@ add acc! ret
entry:
  16384 int array ^A! 16384 int array ^B! 16384 int array ^C!
  128 'fi for
  128 'cr for
  ret
```

## Examples

| File | What it shows |
|------|---------------|
| `examples/hello.uf` | Minimal print |
| `examples/fib.uft` | Iterative Fibonacci |
| `examples/dot.uf` | Typed arrays, vectorized dot product |
| `examples/maptest.uf` | Dict operations |
| `examples/ring.uft` | Channels and `spawn` |
| `examples/shelltest.uf` | Shell, streaming pipelines, `exec` |
| `examples/text/matmul.uft` | Matrix multiply (text encoding) |
| `examples/text/chat.uft` | Multi-user HTTP chat server (concurrency, FFI, SSE) |

## Benchmarks

Six cross-language benchmarks (µFlux, C++, Rust, Python, Node.js) in
[`bench/`](bench/) — contracts in [`bench/SPEC.md`](bench/SPEC.md):

| Benchmark | Kind | Notes |
|---|---|---|
| `logextract` | 500MB access-log analytics | string parsing, dicts |
| `analytics` | 500MB sales CSV analytics | parsing, aggregation |
| `mandelbrot` | numeric kernel (argv N) | scalar loops |
| `spectralnorm` | numeric kernel (argv N) | float loops |
| `matmul` | N×N double matmul (argv N, GPU-oriented) | exercises polymorphic `mul` / Vulkan offload |
| `blackscholes` | option pricing, pinned polynomial CDF (argv N, GPU-oriented) | elementwise arithmetic / offload |

Run (needs `.bench-venv/` for the tokenizer; `bench/data/` holds the 500MB
inputs, gitignored):

```sh
cd bench && python3 run.py
```

The four **legacy** benchmarks run CPU-only (µFlux entries pinned with
`--device cpu`) so numbers stay comparable across revisions; each table also
shows a **GPU-on** column — the same µFlux sources under the default `auto`
Vulkan device. The two GPU-oriented benchmarks also ship CPU + GPU variants
for the other languages (shared launcher in `bench/src/_gpu/`; see
bench/SPEC.md for status).

## Documentation

Full language spec (every opcode, semantics, encoding rules) in
[`SPEC.md`](SPEC.md). Benchmark details in [`bench/SPEC.md`](bench/SPEC.md).

## Status

Experimental, under active development. Current revision: **v13** (+ the
v13.1 addenda: strict label arity, sandboxing/`ufsb`, `cap`/`caps`,
polymorphic matrices, automatic GPU offloading).

## Sandboxed builds (v13.1)

`cargo build --release` produces **two** compilers: `uf` (unrestricted) and
`ufsb` (sandboxed — safe for agent-authored scripts). `ufsb` bakes capability
policies and defaults to **data** (sandboxed filesystem, no network, no
subprocesses); `--policy pure|data|web|build` switches among them:

```sh
ufsb --caps                       # show effective capabilities + workspace roots
ufsb --policy web fetch.uft       # sandboxed fs + HTTPS modules only
uf  --sandbox strict.ufs prog.uft # tighten even the unrestricted build
```

Capability configs are simple `.ufs` files (deny/allow/workspace/allow-module)
usable at build time (`UF_SANDBOX_CONFIG=... cargo build`) and at run time
(`--sandbox FILE`, tighten-only). File IO is confined to a workspace; denied
ops are descriptive compile errors. Programs can query the environment with
`"proc" cap` and `caps` (see SPEC.md).

## Polymorphic matrices + automatic GPU offloading (v13.1)

`[rows cols] type tensor` builds matrices; `mul` on two matrices is matmul,
matrix·vector is matvec, `add`/`sub`/`div` are elementwise or broadcast, and
`transpose` swaps shape (SPEC.md "Polymorphic matrices and arithmetic").

When a Vulkan toolchain is present, eligible ops at scale (arrays, matmul,
matvec, reductions, transpose, sqrt) automatically dispatch to prebuilt
float64 compute kernels — the device with the most free VRAM wins by default:

```sh
uf bigmatmul.uft          # auto: uses the GPU when one exists
uf --device cpu prog.uft  # never offload (v13 behavior)
uf --device vk0 prog.uft  # pin a specific Vulkan device
```

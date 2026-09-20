# Nmerkar (nk)

A programming language that gives AI agents **Python brevity at native speed**.
Nmerkar includes a powerful suite of built-ins which simplify and accelerate data
processing, while retaining full compatibility with C's ecosystem. When a GPU
is available, Nmerkar automatically accelerates code by compiling optimized
fused vulkan kernels. Ask your agent if they'd rather use Nmerkar.

# Who was Enmerkar?

Enmerkar was a Sumerian ruler credited with inventing writing. The legend goes
that he sent his messenger to a nearby lord to ask for a favor. The lord told the 
messenger to return to Enmekar with three riddles. Upon returning, Enmerkar solved 
the riddles, but his solutions were too long for the messenger to remember. So
Enmerkar wrote the words on clay to help the messenger remember, and sent
him back to the lord. The lord was astounded and granted Enmerkar the favors.

## Language Attributes

| Attribute | Nmerkar |
|---|---|
| Specialization | Throwaway data processing tools |
| Execution | Compiled (Nmerkar → C → native binary) |
| Style | Forth-like postfix |
| Block control | Implicit (no brackets) |
| Typing | Dynamic and weak |
| Memory | Garbage collected |
| C Imports | Direct ABI, no glue |
| Primitives | int, float, str, ptr |
| Data structures | list, dict, arr, tensor, chan, atom, obj, bitmap, bloom, iter |
| Concurrency | Channels, threads, dataflow DAGs with fanout |
| Built-ins | Regex, JSON, shell, I/O, streaming, tensor math |
| GPU Acceleration | Full source automatic fusion |

### Tokens to write (fewer = cheaper LLM calls)

| | Nmerkar | C++ | Rust | Python | Node.js |
|---|---|---|---|---|---|
| logextract | **303** | 846 | 667 | 329 | 437 |
| analytics | **251** | 793 | 756 | 366 | 471 |
| mandelbrot | 186 | 189 | 216 | **163** | 165 |
| spectralnorm | 370 | 350 | 487 | **258** | 390 |
| matmul | **143** | 244 | 278 | 172 | 235 |
| blackscholes | **675** | 764 | 834 | 748 | 749 |
| nqueens | **254** | 309 | 349 | 267 | 274 |
| bfs | **386** | 480 | 497 | 397 | 436 |
| dynamicgraph | 355 | 436 | 423 | **303** | 369 |
| **total** | **2923** | 4411 | 4507 | 3003 | 3526 |

Token counts use the **Qwen3** tokenizer (151,643 vocab). The first six
benchmarks are data/tensor-shaped (Nmerkar's home turf); nqueens stresses
control flow, bfs uses packed CSR arrays, and dynamicgraph isolates hash-map
and small-list allocation costs.

### Speed (CPU only, seconds, lower = faster)

| | Nmerkar | C++ | Rust | Python | Node.js |
|---|---|---|---|---|---|
| logextract 510 MB | 0.48 | **0.41** | 0.69 | 3.77 | 2.94 |
| analytics 512 MB | **1.03** | **1.03** | 1.72 | 4.48 | 3.51 |
| mandelbrot | 0.07 | **0.05** | **0.05** | 3.87 | 0.07 |
| spectralnorm | 1.09 | 1.10 | **1.08** | 134.38 | 1.59 |
| nqueens N=11 | 0.03 | 0.01 | **0.01** | 1.61 | 0.04 |
| bfs CSR n=1M | 0.11 | **0.03** | 0.06 | 1.42 | 0.09 |
| dynamicgraph n=1M | 0.52 | **0.16** | 0.31 | 1.69 | 0.41 |
| matmul N=512 | 0.04 | 0.03 | **0.03** | 0.16 | 0.13 |
| blackscholes N=2M | 0.13 | 0.04 | **0.04** | 0.18 | 0.07 |
| **total** | 3.49 | **2.87** | 3.98 | 151.56 | 8.84 |

### GPU offloading (seconds, lower = faster)

| workload | CPU (`--device cpu`) | GPU (`--device vk0`) |
|---|---|---|
| matmul N=2048 | 1.63s | **0.26s** |
| blackscholes N=32M | 2.14s | **1.64s** |

## Quick start

```sh
cd comp && cargo build --release
./comp/target/release/nk '"Hello!\n" print'
cargo install --path comp       # optional: install nk to PATH
```

## Usage

```sh
nk '"Hello Nmerkar!" print'      # run an inline program (cached)
nk prog.n                      # compile + run (cached)
nk -c prog.n -o hello          # compile to standalone binary
nk --emit-c prog.n             # dump the generated C
nk --to-text prog.nd           # convert dense → text
nk somedir/                    # directory mode (auto-discovers main + init threads)
```

Text (`.n`) is the default encoding; dense (`.nd`) is an experimental
token-optimized encoding.

## Example

128×128 matrix multiply, text encoding — no headers, no memory management,
no imports, no declarations:

```
fi:  row! 128 'fe for ret
fe:  col! row@ 128 mul col@ add ix!
     A@ ix@ row@ col@ add set
     B@ ix@ row@ col@ sub set ret
cr:  row! 128 'dc for ret
dc:  col! 0 acc! 128 'ij for
     C@ row@ 128 mul col@ add acc@ set ret
ij:  j! A@ row@ 128 mul j@ add get
     B@ j@ 128 mul col@ add get
     mul acc@ add acc! ret
entry:
  16384 int array A! 16384 int array B! 16384 int array C!
  128 'fi for
  128 'cr for
  ret
```

Use `nk` inline to efficiently process data with `bash`:

```
$ grep ',Retail,' bench/data/sales.csv | nk 'dict p! dict c! "/dev/stdin" "," 0 '\''r file_split_lines z! p@ 5 top_n print c@ 5 top_n print ret r: a! n! p@ 4 10 field_float field_add_to c@ 3 10 field_float field_add_to ret a@'

> [["Bundle B",23912165787.440258],["Refurb Unit",23788674041.119873],["Gadget X1",23781641142.379787],["Spare Part",23776965024.160465],["Widget Pro",23759658166.90052]]
[["Mexico",13218417280.809916],["USA",13212495563.130194],["Canada",13160810726.84009],["Colombia",9948195871.5800667],["Egypt",9948038758.9899693]]
```

## Documentation

Full language spec (every opcode, semantics, encoding rules) in
[`SPEC.md`](SPEC.md).

## Status

Experimental, under active development.

## Sandboxing

Building Nmerkar by default produces two compilers: `nk` and
`nks`. `nks` is a sandboxed version of `nk` that prevents agent-authored
scripts from performing unsafe operations.

```sh
nks --caps                       # show effective capabilities + workspace roots
nks --policy web fetch.n       # use a preconfigred policy
```

`nks` defaults to the `data` policy, or set of disabled features, but ships
with other policies. You can customize the policy at runtime.

## Automatic GPU Acceleration

When a Vulkan toolchain is present, Nmerkar by default, automatically hardware
accelerates applicable segments of code.

```sh
nk bigmatmul.n          # auto: GPU when the static estimate says it wins
nk --device cpu prog.n  # never offload
nk --device vk0 prog.n  # pin a specific Vulkan device
```

## Transpiler

Nmerkar's project includes a transpiler which converts simple C programs and
libraries to Nmerkar-native versions. This allows automated creation of training
data for language models.

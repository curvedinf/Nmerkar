# Enmerkar

A programming language that gives AI agents **Python brevity at native speed**.
Enmerkar includes a powerful suite of built-ins which simplify and accelerate data
processing, while retaining full compatibility with C's ecosystem. When a GPU
is available, Enmerkar automatically accelerates code by compiling optimized
fused vulkan kernels. Ask your agent if they'd rather use Enmerkar.

# Who was Enmerkar?

Enmerkar was a Sumerian ruler credited with inventing writing. The legend goes
that he sent his messenger to a nearby lord to ask for a favor. The lord told the 
messenger to return to Enmekar with three riddles. Upon returning, Enmerkar solved 
the riddles, but his solutions were too long for the messenger to remember. So
Enmerkar wrote the words on clay to help the messenger remember, and sent
him back to the lord. The lord was astounded and granted Enmerkar the favors.

## Language Attributes

| Attribute | Enmerkar |
|---|---|
| Execution | Compiled (Enmerkar → C → native binary) |
| Typing | Dynamic and weak |
| Memory | Garbage collected |
| Primitives | int, float, str, ptr |
| Data structures | list, dict, arr, tensor, chan, atom, obj, bitmap, bloom, iter |
| GPU Acceleration | automatic |
| C Imports | Zero-overhead, no glue |
| Concurrency | Channels, threads, dataflow DAGs with fanout |
| Built-ins | Regex, JSON, shell, I/O, streaming, tensor math |

### Tokens to write (fewer = cheaper LLM calls)

| | Enmerkar | C++ | Rust | Python | Node.js |
|---|---|---|---|---|---|
| logextract | **321** | 846 | 667 | 329 | 437 |
| analytics | **362** | 793 | 756 | 366 | 471 |
| mandelbrot | 205 | **189** | 216 | 163 | 165 |
| spectralnorm | 406 | 350 | 487 | **258** | 390 |
| matmul | **157** | 244 | 278 | 172 | 235 |
| blackscholes | 697 | 764 | 834 | 748 | 749 |
| **total** | **2148** | 3186 | 3238 | 2036 | 2447 |

Token counts use the **Qwen3-0.6B** tokenizer (151,643 vocab).

### Speed (CPU only, seconds, lower = faster)

| | Enmerkar | C++ | Rust | Python | Node.js |
|---|---|---|---|---|---|
| logextract 510 MB | 0.96 | **0.41** | 0.70 | 3.72 | 2.98 |
| analytics 512 MB | 1.60 | **1.03** | 1.79 | 4.49 | 3.72 |
| mandelbrot | 0.38 | **0.05** | 0.05 | 3.86 | 0.07 |
| spectralnorm | 16.6 | 1.12 | **1.08** | 135.9 | 1.60 |
| matmul N=512 | 0.04 | 0.05 | **0.02** | 0.16 | 0.15 |
| blackscholes N=2M | 0.15 | **0.05** | 0.04 | 0.18 | 0.07 |
| **total (4 CPU benches)** | 19.5 | **2.61** | 3.62 | 148.0 | 8.37 |

### GPU offloading (seconds, lower = faster)

| workload | CPU (--device cpu) | GPU |
|---|---|---|
| matmul N=512 | **0.04s** | 0.05s |
| matmul N=1024 | 0.26s | **0.12s** |
| matmul N=2048 | 2.74s | **0.42s** |
| blackscholes N=2M | **0.14s** | 0.57s |

## Quick start

```sh
cd comp && cargo build --release
./comp/target/release/nkr '"Hello!\n" print'
cargo install --path comp       # optional: install nkr to PATH
```

## Usage

```sh
nkr '"Hello Enmerkar!" print'      # run an inline program (cached)
nkr prog.en                     # compile + run (cached)
nkr -c prog.en -o hello         # compile to standalone binary
nkr --emit-c prog.en            # dump the generated C
nkr --to-text prog.en           # convert dense → text
nkr somedir/                    # directory mode (auto-discovers main + init threads)
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

Use `nkr` inline to efficiently process data with `bash`:

```
$ grep ',Retail,' bench/data/sales.csv | nkr 'dict ^p! dict ^c! "/dev/stdin" "," 0 '\''r file_split_lines z! ^p@ 5 top_n print ^c@ 5 top_n print ret r: a! n! ^p@ 4 10 field_float field_add_to ^c@ 3 10 field_float field_add_to ret a@'

> [["Bundle B",23912165787.440258],["Refurb Unit",23788674041.119873],["Gadget X1",23781641142.379787],["Spare Part",23776965024.160465],["Widget Pro",23759658166.90052]]
[["Mexico",13218417280.809916],["USA",13212495563.130194],["Canada",13160810726.84009],["Colombia",9948195871.5800667],["Egypt",9948038758.9899693]]
```

## Documentation

Full language spec (every opcode, semantics, encoding rules) in
[`SPEC.md`](SPEC.md). Benchmark details in [`bench/SPEC.md`](bench/SPEC.md).

## Status

Experimental, under active development.

## Sandboxing

Building Enmerkar by default produces two compilers: `nkr` and
`nkrsb`. `nkrsb` is a sandboxed version of `nkr` that prevents agent-authored 
scripts from performing unsafe operations.

```sh
nkrsb --caps                       # show effective capabilities + workspace roots
nkrsb --policy web fetch.ent       # use a preconfigred policy
```

`nkrsb` defaults to the `data` policy, or set of disabled features, but ships
with other policies. You can customize the policy at runtime.

## Automatic GPU Acceleration

When a Vulkan toolchain is present, Enmerkar by default automatically hardware 
accelerates applicable segments of code.

```sh
nkr bigmatmul.ent          # auto: uses the GPU when one exists
nkr --device cpu prog.ent  # never offload (v13 behavior)
nkr --device vk0 prog.ent  # pin a specific Vulkan device
```

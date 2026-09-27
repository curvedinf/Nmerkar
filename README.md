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
| logextract | **301** | 846 | 667 | 329 | 437 |
| analytics | **241** | 793 | 756 | 366 | 471 |
| mandelbrot | 179 | 189 | 216 | **163** | 165 |
| spectralnorm | 376 | 350 | 487 | **333** | 390 |
| matmul | **138** | 244 | 278 | 172 | 235 |
| blackscholes | **664** | 764 | 834 | 748 | 749 |
| nqueens | **257** | 309 | 349 | 267 | 274 |
| bfs | **381** | 480 | 497 | 397 | 436 |
| dynamicgraph | 349 | 436 | 423 | **303** | 369 |
| **total** | **2886** | 4411 | 4507 | 3078 | 3526 |

Token counts use the **Qwen3** tokenizer (151,643 vocab). The first six
benchmarks are data/tensor-shaped (Nmerkar's home turf); nqueens stresses
control flow, bfs uses packed CSR arrays, and dynamicgraph isolates hash-map
and small-list allocation costs.

### Speed (CPU only, seconds, lower = faster)

| | Nmerkar | C++ | Rust | Python | Node.js |
|---|---|---|---|---|---|
| logextract 510 MB | 0.47 | **0.41** | 0.70 | 3.67 | 2.95 |
| analytics 512 MB | 1.04 | **1.02** | 1.72 | 4.50 | 3.42 |
| mandelbrot | 0.06 | **0.05** | 0.05 | 4.24 | 0.07 |
| spectralnorm | 1.09 | 1.11 | **1.08** | 48.83 | 1.57 |
| nqueens N=11 | 0.02 | 0.01 | **0.01** | 1.46 | 0.04 |
| bfs CSR n=1M | 0.09 | **0.03** | 0.05 | 1.31 | 0.07 |
| dynamicgraph n=1M | 0.44 | **0.16** | 0.30 | 1.51 | 0.39 |
| matmul N=512 | 0.03 | 0.03 | **0.02** | 0.15 | 0.13 |
| blackscholes N=2M | 0.05 | 0.04 | **0.04** | 0.17 | 0.07 |
| **total** | 3.33 | **2.85** | 3.97 | 65.84 | 8.70 |

CPU is an AMD Ryzen 7900x3D.

### GPU offloading (seconds, lower = faster)

| workload | CPU (`--device cpu`) | GPU (`--device vk0`) |
|---|---|---|
| matmul N=2048 | 1.56s | **0.26s** |
| blackscholes N=32M | 0.61s | **0.21s** |

GPU is an AMD Radeon 7900 XTX.

## Quick start

```sh
curl -fsSL https://raw.githubusercontent.com/curvedinf/Nmerkar/main/install.sh | sh
export PATH="$HOME/.local/bin:$PATH"
nk '"Hello!\n" print'
```

The installer supports Linux (x86_64, ARM64, ARMv7) and macOS (ARM64, Intel).
It installs and verifies both `nk` and `nks`, installing a C compiler and its
development libraries if needed. Binaries go to `~/.local/bin` by default. An existing
GitHub release is used when available; before the first release the installer
builds from source and installs Rust if necessary. GPU/Vulkan tools and drivers
are optional and are not installed by the CPU installer.

To build manually: `cd comp && cargo build --release`, then run
`./target/release/nk '"Hello!\n" print'`.

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

## Key Concepts

**Read left to right.** Nmerkar uses postfix operations: `3 4 add print`
adds two values, then prints `7`. Each operation consumes its inputs and passes
its result to the next one. The compiler manages the underlying stack; `total!`
stores a value and `total` reads it. A `;` starts a comment.

**Work with data.** Integers, floats, and strings can live in lists, dictionaries,
objects, or typed arrays and tensors. The same `get`, `set`, and `length`
operations work across several containers. Managed values are garbage collected,
and built-ins handle JSON, text, files, and regular expressions.

**Compose behavior.** `if`, `while`, and `for` call labeled blocks such as
`'step`; the block is defined with `step:` and can bind inputs to named variables.
Blocks also serve as callbacks for operations such as `filter`, and `ret` returns
a value. Channels and threads support concurrent programs.

**Compile and run.** `nk` translates a program to C, compiles it with the system
C compiler, and runs the native result; it can also emit C or a standalone
binary. Eligible compute work can use a Vulkan GPU automatically when the
toolchain and device are available, while `--device cpu` keeps it on the CPU.
Use `nks` to run programs under a capability policy.

## Examples

A shopping list stores items in order:

```nmerkar
["apples" "bread" "milk"] groceries!      ; Create a shopping list with a list literal.
groceries "eggs" append groceries!        ; Add eggs to the existing list.
groceries length print                    ; Print how many items to buy.
groceries 0 get print                     ; Print the first item.
groceries print                           ; Print the complete shopping list.
```

A loop calls a helper to price three orders of increasing size:

```nmerkar
0 total!                                   ; Start the running total at zero.
3 'add_order for                            ; Run add_order with indexes 0, 1, and 2.
total print                                ; Print the total cost of all three orders.
ret                                        ; Finish the main program before the helper blocks.
add_order: index!                          ; Give each loop index a name.
  index 1 add _call price total add total! ; Price one more item than the index, then add it to the total.
  ret                                      ; Return to the loop.
price: quantity!                           ; Define a reusable helper that receives a quantity.
  ret quantity 5 mul                       ; Return the quantity times the $5 unit price.
```

Use `nk` inline to efficiently process data with `bash`:

```
$ grep ',Retail,' bench/data/sales.csv | nk 'dict p! dict c! "/dev/stdin" "," 0 '\''r file_split_lines z! p 5 top_n print c 5 top_n print ret r: a! n! p 4 10 field_float field_add_to c 3 10 field_float field_add_to ret a'

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

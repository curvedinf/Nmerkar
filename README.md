# Nmerkar (nk)

A programming language that gives AI agents **Python token count at C speed**. Nmerkar includes a powerful suite of built-ins which simplify data processing, while retaining full compatibility with C's ecosystem. Garbage collection and simplified scoping speed development, while a sandboxed mode prevents agents from accidentally deleting your whole computer. Nmerkar doesnt have any parenthesis or brackets to forget to close, and has a weird emoji language encoding for absolutely minimal token count. When a GPU is available, Nmerkar automatically accelerates code by compiling optimized fused vulkan kernels. There are many more interesting features your agent will appreciate, so point it here and ask if it would rather use Nmerkar.

## Quick start

```sh
curl -fsSL https://raw.githubusercontent.com/curvedinf/Nmerkar/main/install.sh | sh
nk '"Hello!" print'
```

Supports Linux (x86_64, ARM64, ARMv7) and macOS (ARM64, Intel). Installs `nk`,
`nks`, and the C compiler and libraries required to run Nmerkar programs.

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
| Style | Forth-like postfix but no stack manipulation |
| Block control | Implicit (no brackets) |
| Typing | Dynamic and weak default, optional per value strictness |
| Memory | Garbage collected |
| Library Ecosystem | C / C++ / Rust full compatibility |
| Primitives | int, float, bool, byte, str, ptr |
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

Token counts use the **Qwen3** tokenizer (151,643 vocab).

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

Performed on AMD Ryzen 7900x3D.

### GPU offloading (seconds, lower = faster)

| workload | CPU (`--device cpu`) | GPU (`--device vk0`) |
|---|---|---|
| matmul N=2048 | 1.56s | **0.26s** |
| blackscholes N=32M | 0.61s | **0.21s** |

Performed on AMD Radeon 7900 XTX.

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

`true` and `false` are boolean values, distinct from `1` and `0` (use
`structural_equal` to compare types as well as values). `parse_json` preserves
JSON booleans through lists and dicts, and `to_json` writes them back as
`true`/`false`. Use `bool cast` to convert a value explicitly.

## Language Basics

The mental model in eleven rules — function-level details and the full op
reference are in [`SPEC.md`](SPEC.md).

**1. Postfix dataflow over a hidden stack.** Read left to right: literals and
variable loads push values; each operation consumes the values above it and
leaves its result for the next operation. You never manipulate the stack —
values reach an operation from literals, named variables, or the previous
operation's result. `3 4 add print` prints `7`.

**2. Every store passes its value through.** `d!` stores into the variable
*and* leaves the value on the stack — local or shared alike, one token, one
effect. The value flows on; a leftover below never disturbs later lines
(operations read from the top, and bodies drain at `ret`). Square with
`5 3 sub d! d mul` — the pass-through *is* the operand. One caution: don't
place a store *between* pushing a call's arguments — the leftover splices
into the argument window (`100 3 x! 5 _call add2` passes 3 and 5, not 100
and 5). Keep side effects on their own lines, or `consume` the leftover.

**3. Destructuring is explicit `#`.** `result out# err# code#` binds the
parts of the structure on the stack; the structure itself stays there.
`x#` takes the next positional slot, `x#key` takes a dict key or object
field, `_#` skips a slot. A plain `x!` always stores the *whole* value —
`!` never reaches inside. A bind that cannot resolve (non-container,
out-of-bounds slot, missing key) dies at its source line.

**4. `set` returns the stored element, not the container.** `nums 1 99 set`
mutates `nums` in place and leaves `99`, so binding its result captures the
element — not the updated list. Read the container back by name. `get`
likewise returns the element.

**5. Render containers with `print` or `to_json`, not `%s`.** In `format`,
`%s` applies string coercion: a list becomes `1,2,3`, a dict
`[object Object]`, and a typed array or tensor renders as raw handle digits
(an address). `print` gives the type-aware representation (`[10, 99, 30]`);
`to_json` gives JSON.

**6. Labels are the functions.** `name:` defines, `'name` references,
`_call` invokes; parameters bind with `p!` directly after the definition's
colon; every code path ends in `ret`. A callee's entire stack is drained on
return — only the `ret` value reaches the caller, so callees cannot leak
operands.

**7. Named variables and comments.** `total!` stores a value and `total`
reads it back. A `;` starts a comment.

**8. Built-in containers and utilities.** Integers, floats, and strings can
live in lists, dictionaries, objects, or typed arrays and tensors. The same
`get`, `set`, and `length` operations work across several containers.
Managed values are garbage collected, and built-ins handle JSON, text,
files, and regular expressions.

**9. Blocks drive control flow and callbacks.** `if`, `while`, and `for`
call labeled blocks such as `'step`, and blocks also serve as callbacks for
operations such as `filter`.

**10. Parallel pipelines with `weave`.** Process files and large datasets
without managing threads or passing results between them by hand. Define
steps as `task` blocks; `run` executes independent steps in parallel and
passes their results to dependent steps. Add a worker count to spread
collection items across cores; eligible tensor work can use a fused Vulkan
GPU kernel.

**11. Compile and run.** `nk` translates a program to C, compiles it with
the system C compiler, and runs the native result; it can also emit C or a
standalone binary. Eligible compute work can use a Vulkan GPU automatically
when the toolchain and device are available, while `--device cpu` keeps it
on the CPU. `nks` runs programs in a sandbox.

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

A `weave` streams prices from a file, discounts them as a tensor, and totals the result.
Running it creates `weave-prices.txt` in the current directory:

```nmerkar
"weave-prices.txt" "10\n20\n30" write_file            ; Write three sample prices to a file.
weave                                               ; Begin a task graph.
  task prices:                                      ; Define the file-processing task.
    "weave-prices.txt" list 'collect_price file_fold_lines values! ; Stream file lines into a list.
    ret values float tensor                         ; Turn the parsed prices into a float tensor.
  collect_price: values! line!                       ; Receive the accumulated list and the next line.
    ret values line parse_float append              ; Parse the price and add it to the list.
  task discounted: prices!                          ; Wait for the prices task and receive its tensor.
    ret prices 0.9 mul                              ; Apply a 10 percent discount to each price.
  task total: discounted!                           ; Wait for the discounted tensor.
    ret discounted sum                              ; Sum the discounted prices.
run total "%.2f" format print                       ; Run the graph and print 54.00.
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

## Observability

Nmerkar is designed for agents that compile, run, and fix programs in a tight
loop, so observability is part of the language rather than an add-on. Every
runtime failure carries the source site of the instruction that failed, on both
the native and JavaScript targets:

```
nk: L<line> [label] <message>
```

In a multi-file program the owning file's stem is prefixed
(`nk: mb:L3 [pubf] DIV: division by zero`); instructions with no source line
(macro bodies, synthesized epilogues) fall back to the plain message.

Four debug ops work on both targets:

- `dbg` peeks the stack top and renders it on stderr like `print`
  (`dbg L8 [entry]: 200000`), without popping it
- `check` dies at that line with `check failed` when its operand is falsy
  (exit code 1)
- `timer` pushes the monotonic clock in nanoseconds (only differences are
  meaningful)
- `memstats` pushes the native GC's counters as a dict: `{gc, objs, live, alloc}`

```nmerkar
; the four debug ops in one small program
entry:
  100000 v0!          ; v0 = a value to observe
  timer v1!           ; v1 = monotonic clock, nanoseconds
  v0 2 mul v0!        ; some work
  timer v2!
  v2 v1 sub "elapsed=%d ns" format print
  v0 dbg              ; peek at v0 on stderr
  v0 0 gt check       ; die "check failed" if v0 is falsy
  memstats v3!        ; v3 = {gc, objs, live, alloc}
  v3 "gc" get "gc collections=%d" format print
  ret
```

```
$ nk debug.n
dbg L8 [entry]: 200000
elapsed=270 ns
gc collections=0
```

The native target adds two report flags. Both are compiled in only when
requested (zero cost otherwise) and write to stderr after `main` returns, so a
program that dies exits before the report; together they print both tables:

```sh
nk --profile prog.n     # per-label call counts and time
nk --coverage prog.n    # per-label hit counts
```

```
--- nk profile ---
label         line   calls            ns     %
v3              13      21          1020  51.3
v4              16      20           970  48.7
entry            2       1             0   0.0
total                               1990 100.0

--- nk coverage ---
label         line  hits
entry            2     1
v3              13    21
v4              16    20
0 of 3 labels uncovered
```

`nk -D prog.n` extends the crash dump: after the failing thread's stack, one
best-effort block per other live thread (`thread #1:` / `#0  bump (pc=17)`).
The four debug ops and error sites also work on the JavaScript target;
`--profile`, `--coverage`, and the all-thread dump are native-only (see
Javascript Compiler).

Normative semantics: SPEC.md, "Debugging and profiling".

## Javascript Compiler

The same source compiles either to a native binary or to plain JavaScript.
`--emit-js` writes a standalone `.js` file that runs directly under Node —
stdout, stderr, exit code, and die messages match the native target:

```sh
nk --emit-js prog.n -o prog    # writes prog.js
node prog.js
```

For the browser, `web/nk.js` (rebuild with `sh web/build.sh`) is a single-file
bundle of the compiler. `web/example.html` is a working example page that
opens directly from disk, with a selector for several sample programs:

```html
<script src="nk.js"></script>
<script>
  nk.compile(src).then(function (prog) { return prog.run(); })
    .then(function (r) { out.textContent = r.output + r.err; });
</script>
```

The same bundle works under Node: `nk.compile(source)` resolves to a program
handle (rejecting with `nk: <error>` on compile failure), and
`prog.run()` — optionally passed an env object carrying `argv`, `stdin`, and
`fs` hooks — resolves to `{output, err, error, rc}`: captured stdout, captured
stderr, the die message (without the `nk: ` prefix), and the exit code.

Boundaries:

- Full op coverage: every op, including the four debug ops, works on the
  JavaScript target. The parity suite (`bash web/tests/run_tests.sh`, 84
  gates) compiles and runs every `comp/tests` gate, every example (including
  the GNU transpiler outputs), and every benchmark program on both targets and
  compares the captured streams byte for byte.
- `--profile`, `--coverage`, and the all-thread crash dump are native target
  only; `memstats` reads zeros on the JS target (the host manages the heap).

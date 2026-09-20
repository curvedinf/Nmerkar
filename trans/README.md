# trans — C → Nmerkar transpiler (Rust)

`trans` reads a C-subset source file and prints Nmerkar **text-encoding**
source (`.n`, per `../SPEC.md`) to stdout. It is a standalone Rust crate
(std-only, no external dependencies) split into logical modules:

```
src/
  main.rs    CLI: usage, file IO, pipeline wiring, exit codes
  error.rs   positioned diagnostics (line:col) + Result alias
  token.rs   token kinds and the token record
  lexer.rs   hand-rolled scanner; skips whitespace/comments/preprocessor
  ast.rs     the C-subset syntax tree (expressions, statements, functions)
  parser.rs  recursive descent with C precedence for the supported subset
  emit.rs    AST → Nmerkar text (the translation proper)
```

Build and run:

```
cargo build --release            # binary at trans/target/release/trans
./target/release/trans prog.c > prog.n
../comp/target/release/nk prog.n                 # compile + run
# or: ../comp/target/release/nk -c prog.n -o prog && ./prog -- args...
```

`cargo test` runs the crate's unit tests (lexer, parser precedence, emitter
shape checks). `bash run_tests.sh` runs the behavioral suite.

## Test suite

Three gated pathways:

`bash run_tests.sh` → `pass=23 fail=0` (per-operation + coreutils round-trips):

- **Round-trips** (`tests/*.c`): C → trans → nk → run, compared against
  system binaries (`echo`, `false`, `true`, `wc`, `yes`) or expected-output
  files (`hello`, `mini_gen`). Infinite streams (`yes`) compare the first
  50 lines and the timeout exit code.
- **Per-operation gates** (`tests/ops/*.c`): transpile + compile + run with
  gates on exit code, empty stderr at every stage, and stdout equal to the
  checked-in `.out` file. Covers arithmetic, comparisons, logic, `++`/`--`,
  compound assignment, if/else chains, all loop forms, `break`/`continue`,
  nested loops, recursion, early returns from inside loops, string
  indexing, char literals, `printf` formats, `argc`/`argv`, and an edge
  test (dead code after return, chained/compound assignment as expression
  values, side-effecting call arguments, `for(;;)`, compound for-post).
- **GNU multi-file tools** (`bash tests/gnu/run.sh` → `pass=6 fail=0`):
  six multi-file adaptations of GNU coreutils tools — `cat` (16 flag
  combos incl. -A/-v M-notation, squeeze, numbering), `head` (lines/bytes
  with b/K/M/G suffixes, old-style -N, headers), `nl` (-b a/t/n, -n
  ln/rn/rz, -w/-v/-i/-s), `tee` (chunked fan-out; the subset has no
  arrays, so files are re-opened per chunk), `cksum` (POSIX MSB-first
  CRC-32 with little-endian length feeding), `base64` (encode with -w
  wrap, decode with padding). Each tool's `.c` files (concatenated in
  `ls` order) are transpiled in one invocation — multi-file input — and
  gated against the system binaries. The transpiled `.n` of every tool
  is exported to `examples/gnu/` so the generated code is publicly
  browsable.

Multi-file input: `trans a.c b.c ...` concatenates the sources (in
argument order) before lexing; diagnostics report the originating file.
Reference builds (`tests/gnu/shim.h` + per-tool `gnu_main.c`, compiled
with gcc) let a bad adaptation be caught independently of the transpiler.

## Supported C subset

- Types: `int`, `char`, `char*` (all stored in 64-bit cells; `char` values
  are bytes). Functions return `int`/`char`/`char*`; parameters of those
  types. Single translation unit. No file-scope variables.
- Local declarations anywhere a statement is allowed, with optional
  initializer (`int x = expr;`), comma-separated declarators.
- Statements: `return expr;`, `if/else`, `while`, `do-while`, `for`
  (init/cond/post, any part optional), `break`, `continue`, `{ ... }`
  blocks, expression statements, empty `;`.
- Assignment: `=`, `+=`, `-=`, `*=` — as expressions (right-associative,
  value is the stored value).
- Expressions: int/char/string literals, identifiers, calls (including
  zero-arg and vararg `printf`), parenthesized, unary `- ! ~ +`, prefix and
  postfix `++`/`--` on variables, binary `* + -`, comparisons
  `< <= > >= == !=`, logical `&& ||` (both operands always evaluated — no
  short-circuit), with C precedence. `p[i]` on a `char*` reads byte `i`
  (via `strstr` raw-pointer idiom + `load`).
- Builtins: `argc`, `argv[i]` (via `extern "nk_argc"` / `extern "nk_argv"`),
  `__byte(p)` (first byte of `p`), `NULL` (0), `EOF` (-1).
- Comments (`/* ... */`, `//`) and blank lines are fine; preprocessor lines
  (`#include` etc.) are skipped. Call libc directly; the emitted preamble
  always IMPORTs `printf fprintf fputc ungetc putchar getchar fputs fwrite
  fread strcmp strncmp strcpy exit fopen fclose fgetc strstr` and declares
  `extern "stdout" "stdin" "stderr"`.
- **Native mappings** — these libc calls compile to Nmerkar ops instead of
  FFI imports (no libc symbol, works under any sandbox policy):
  `malloc(n)`→`malloc`, `free(p)`→`free`, `strlen(s)`→`length`,
  `strcat(a,b)`→`concat` (returns a NEW string; the C destination is not
  mutated — use the return value), `puts(s)`→`print` + newline (a `0`
  stands in for the C return value). `strstr` stays imported: the generated
  `p[i]` byte-index idiom uses it to obtain a string's raw data pointer
  (there is no native pointer-extraction op). Streaming stdio
  (`fopen/fread/fprintf/...`) has no native equivalent — `read_file`/
  `write_file` are whole-file and die on error, which would change tool
  behavior (exact "No such file" diagnostics, streaming of unseekable
  input), so those remain FFI.
- `return expr;` in `main` becomes the process exit code (`_call exit`).

## Deliberate omissions (parse-time or run-time errors)

- `/` and `%` (division/modulo): no DIV opcode in the subset; the parser
  aborts with a positioned error. Digit counting must use comparison
  chains (see `tests/wc.c`).
- `switch`, arrays (other than byte-indexing a `char*`), structs, enums,
  pointers other than `char*`, `long`/`float`/`double`, `goto`, the
  preprocessor.
- C `\x..` and `\0..` escapes **inside string/char literals**: Nmerkar
  string escapes are only `\n \t \r \0 \\ \"`; an unknown char escape is an
  error. Use numeric codes instead (the test programs compare against e.g.
  92 for backslash).
- `&&`/`||` do not short-circuit: both sides are evaluated (normalized via
  `not not` then combined). Guard out-of-bounds dereferences with nested
  `if`s.
- Recursion works: every `_call` entry gets a fresh local frame, so a
  callee (recursive or not) cannot clobber the caller's slots.

## Emission model

- A top-level register prologue `0 rv! 0 fr! 0 frv! 0 pt! ret` declares the
  four cross-body registers shared (v14: plain names assigned in several
  label bodies must be shared). `rv`/`fr` carry the early-return value and
  flag, `frv` is the epilogue temp, `pt` holds call results and inc/dec
  temps.
- Every C variable is a unique local slot `vN` (never reused), local to the
  function's body (construct labels merge into that body); `main` becomes
  the `entry:` label.
- Control flow emits `if`/`if_else`/`while` with quotation labels whose
  bodies are defined after the function body; nested construct labels
  follow their enclosing body's `ret`.
- Early `return` inside control flow binds `rv`, sets `fr`, and the
  remaining statements of the enclosing list are wrapped behind
  `fr@ 'c 'b if_else` (guard labels hoisted to function end); loop
  conditions are `fr@`-guarded so loops exit; each function epilogue
  returns `fr@ rv@ mul` (the early value when flagged, else 0) and
  resets the flag. Dead code after a top-level `return` is dropped.
- Calls are direct: arguments evaluate inline in the caller's body, then
  `_call name`. The callee's parameter binds pop the arguments, its `ret`
  drains the data stack and leaves the single return value — vararg
  counts (`printf`/`fprintf`) are pushed after the arguments. No
  save/restore wrapper is needed: callee frames are fresh (v14), and
  argument residue below the vararg window is impossible because local
  stores' pass-through cells stay under the argument values.
- A `for` post-expression becomes an `nN` label invoked by the loop body
  tail and by `continue` (`1 'nN if continue` — a quotation jump, same
  frame, so the post's slot writes land in the loop's locals; a `_call`
  would push a fresh frame and lose them). `do-while` uses a `dfN`
  first-pass flag because `'c 'b while` tests before the first body run.
- Post-`++`/`--` emits `x@ pt! x@ 1 add x! frv! pt@` — net exactly one
  cell (the old value): the increment's pass-through residue is sunk into
  `frv` (free outside the epilogue) so it cannot leak between a vararg
  format string and its arguments.

# trans — C → Enmerkar transpiler (Rust)

`trans` reads a C-subset source file and prints Enmerkar **text-encoding**
source (`.ent`, per `../SPEC.md`) to stdout. It is a standalone Rust crate
(std-only, no external dependencies) split into logical modules:

```
src/
  main.rs    CLI: usage, file IO, pipeline wiring, exit codes
  error.rs   positioned diagnostics (line:col) + Result alias
  token.rs   token kinds and the token record
  lexer.rs   hand-rolled scanner; skips whitespace/comments/preprocessor
  ast.rs     the C-subset syntax tree (expressions, statements, functions)
  parser.rs  recursive descent with C precedence for the supported subset
  emit.rs    AST → Enmerkar text (the translation proper)
```

Build and run:

```
cargo build --release            # binary at trans/target/release/trans
./target/release/trans prog.c > prog.ent
../comp/target/release/nkr prog.ent                 # compile + run
# or: ../comp/target/release/nkr -c prog.ent -o prog && ./prog -- args...
```

`cargo test` runs the crate's unit tests (lexer, parser precedence, emitter
shape checks). `bash run_tests.sh` runs the behavioral suite.

## Test suite

Three gated pathways:

`bash run_tests.sh` → `pass=22 fail=0` (per-operation + coreutils round-trips):

- **Round-trips** (`tests/*.c`): C → trans → nkr → run, compared against
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
  gated against the system binaries. The transpiled `.ent` of every tool
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
- Builtins: `argc`, `argv[i]` (via `extern "nkr_argc"` / `extern "nkr_argv"`),
  `__byte(p)` (first byte of `p`), `NULL` (0), `EOF` (-1).
- Comments (`/* ... */`, `//`) and blank lines are fine; preprocessor lines
  (`#include` etc.) are skipped. Call libc directly; the emitted preamble
  always IMPORTs `printf malloc free puts putchar getchar fputs fwrite
  strlen strcmp strncmp strcpy strcat exit fopen fclose fgetc strstr` and
  declares `extern "stdout"`.
- `return expr;` in `main` becomes the process exit code (`_call exit`).

## Deliberate omissions (parse-time or run-time errors)

- `/` and `%` (division/modulo): no DIV opcode in the subset; the parser
  aborts with a positioned error. Digit counting must use comparison
  chains (see `tests/wc.c`).
- `switch`, arrays (other than byte-indexing a `char*`), structs, enums,
  pointers other than `char*`, `long`/`float`/`double`, `goto`, the
  preprocessor.
- C `\x..` and `\0..` escapes **inside string/char literals**: Enmerkar
  string escapes are only `\n \t \r \0 \\ \"`; an unknown char escape is an
  error. Use numeric codes instead (the test programs compare against e.g.
  92 for backslash).
- `&&`/`||` do not short-circuit: both sides are evaluated (normalized via
  `not not` then combined). Guard out-of-bounds dereferences with nested
  `if`s.
- Recursion works: parameter slots are saved and restored around every call
  via generated `kN` wrapper labels (variables are still globals — a
  recursive callee's *non-parameter* locals can still collide with the
  caller's if the caller reads them after the call; parameters are
  protected).

## Emission model

- Every C variable is a unique global slot `vN` (never reused), tracked
  per function; `main` becomes the `entry:` label.
- Control flow emits `if`/`if_else`/`while` with quotation labels whose
  bodies are defined after the function body; nested construct labels
  follow their enclosing body's `ret`.
- Early `return` inside control flow binds `^rv`, sets `^fr`, and the
  remaining statements of the enclosing list are wrapped behind
  `^fr@ 'c 'b if_else` (guard labels hoisted to function end); loop
  conditions are `^fr@`-guarded so loops exit; each function epilogue
  returns `^fr@ ^rv@ mul` (the early value when flagged, else 0) and
  resets the flag. Dead code after a top-level `return` is dropped.
- Each call site emits `_call kN`; the `kN` wrapper label saves the
  caller's parameter slots on `^svst`, evaluates arguments, calls, and
  restores — recursion-correct and vararg-safe (exactly one value left on
  the stack). Wrappers are hoisted after all construct labels so
  Enmerkar's linear break/continue validation keeps `break` inside its
  loop's textual region.
- A `for` post-expression becomes an `nN` label invoked by the loop body
  tail and by `continue` (`_call nN continue`), so `continue` still
  increments. `do-while` uses a `dfN` first-pass flag because `'c 'b
  while` tests before the first body run.

# trans — C → Enmerkar transpiler, written in Enmerkar

`trans.en` is the first real program written in Enmerkar itself: a regex-based
lexer (the `match` op) plus a hand-rolled recursive-descent parser that reads a
C source file and prints **v11 text-encoding** Enmerkar source (per
`../SPEC.md`) to stdout.

> **v13.1 status: complete.** All round-trip tests and all per-operation
> gates pass (`bash run_tests.sh` → `pass=21 fail=0`): system-binary
> round-trips for `echo` (flag/escape handling), `false`, `true`, `wc`,
> `yes`, plus output-gated runs of `hello` and `mini_gen`, and a
> per-operation suite (`tests/ops/`) covering arithmetic, comparisons,
> logic, `++`/`--`, compound assignment, if/else chains, all loop forms,
> `break`/`continue`, nested loops, functions with recursion, early returns
> from inside loops, string indexing, char literals, `printf` formats, and
> `argc`/`argv`. Early returns from inside control flow use a flag-guarded
> skip mechanism; recursive calls save/restore parameter slots (variables
> are still globals) via generated per-call-site wrapper labels. Compiler
> bugs the port exposed were fixed in the same changeset (label-body type
> propagation, untracked-pointer truthiness, value-cache aliasing on
> global rebind, raw-pointer arithmetic, string-aware `load`, `%*d`,
> vararg string marshaling).

## Usage

```
../comp/target/release/uf -c trans.en -o trans
./trans prog.c > prog.ent
../comp/target/release/uf prog.ent            # compile + run
# or: ../comp/target/release/uf -c prog.ent -o prog && ./prog -- args...
```

Round-trip example: `tests/hello.c` produces output identical to `gcc`
(`hello, enmerkar` / `sum=55`). Larger adaptations of GNU coreutils programs
live in `tests/` (see `tests/README.md`).

## Supported C subset

- Types: `int`, `char`, `char*` (all stored in 64-bit cells; `char` values
  are bytes). Functions return `int`/`char`/`char*`; parameters of those
  types. Single translation unit. No file-scope variables.
- Local declarations anywhere a statement is allowed, with optional
  initializer (`int x = expr;`).
- Statements: `return expr;`, `if/else`, `while`, `do-while`, `for`
  (init/cond/post, any part optional), `break`, `continue`, `{ ... }`
  blocks, expression statements, empty `;`.
- Assignment: `=`, `+=`, `-=`, `*=`.
- Expressions: int/char literals, string literals, identifiers, calls
  (including zero-arg calls and vararg `printf`), parenthesized, unary
  `- ! ~ +`, prefix and postfix `++`/`--`, binary `* + -`, comparisons
  `< <= > >= == !=`, logical `&& ||` (both operands always evaluated — no
  short-circuit), with C precedence. `p[i]` on a `char*` reads byte `i`
  (`loadx 255 and`).
- Builtins: `argc`, `argv[i]` (via `extern "nkr_argc"` / `extern "nkr_argv"`),
  `__byte(p)` (first byte of `p`), `NULL` (0), `EOF` (-1).
- Comments (`/* ... */` and `//`) and blank lines are fine. No preprocessor
  lines (`#include` etc.) — call libc functions directly; the emitted
  preamble always IMPORTs `printf malloc free puts putchar getchar fputs
  fwrite strlen strcmp strncmp strcpy strcat exit fopen fclose fgetc` and
  declares `extern "stdout"`.
- `return expr;` in `main` becomes the process exit code (`call exit`).

## Deliberate omissions (parse-time or run-time errors)

- `/` and `%` (division/modulo): Enmerkar has no DIV opcode; the parser aborts.
  Digit counting etc. must use comparison chains (see `tests/wc.c`).
- `switch`, arrays (other than byte-indexing a `char*`), structs, enums,
  pointers other than `char*`, `long`/`float`/`double`, `goto`, the
  preprocessor.
- C `\x..` and `\0..` escapes **inside string/char literals**: Enmerkar string
  escapes are only `\n \t \r \0 \\ \"`, and an unknown escape drops the
  backslash. Use numeric codes instead (the test programs compare against
  e.g. 92 for backslash).
- Recursion works: parameter slots are saved and restored around every call
  via generated wrapper labels (variables are still globals — a recursive
  callee's *non-parameter* locals can still collide with the caller's if the
  caller reads them after the call; parameters are protected).
- `&&`/`||` do not short-circuit: both sides are evaluated (normalized via
  `not not` then combined with `and`/`or`). Guard out-of-bounds dereferences
  with nested `if`s.

## Internals

- Lexer: each token class is matched by the Enmerkar `match` regex op; tokens are
  kept in a list (`toks`) of typed records and indexed by position (`pi`).
- Parser: single-pass, emits Enmerkar text as it parses — expressions map
  directly onto the stack machine. Every C variable gets a unique global
  slot `v0, v1, ...` (emitted as `^v0!`/`^v0@`; tracked in a `vars` dict).
- Control flow emits v13 structured opcodes: `if`/`if_else`/`while` with
  quotation labels (`'iN`, `'cN`, `'bN`). Each C `if`/`while`/`for`/`do`
  generates condition and body quotations whose labels are deferred past the
  enclosing body's terminating `ret` (a three-level output scheme: direct,
  `qout` per-function, `douts` deferral buffers, and `flabels` for
  function-level hoisted labels). A label stack (`ls`) saves and restores
  `inq`/`emode`/label numbers across recursive parser calls; a loop-kind
  stack (`lstack`) tracks `for`-increments so `continue` runs them.
- Early `return` inside control flow: the return binds `^rv`, sets `^fr`,
  and remaining statements are wrapped behind an `^fr@ 'skip 'rest
  if_else` guard; loop conditions are `^fr@`-guarded so loops exit; each
  function epilogue returns `^fr@ ^rv@ mul` (the flag) and resets it.
- Calls: each call site emits `_call kN` where `kN` is a generated wrapper
  label that saves the current function's parameter slots on the `^svst`
  list, evaluates the arguments, calls, and restores — this keeps recursion
  correct and leaves exactly one value on the stack (vararg-safe).
- `for` posts are extracted into `nN` labels called by both the loop body
  tail and `continue` (`^fr@ not 'nN if`), so `continue` still increments.
- `break` and `continue` emit the native Enmerkar `break`/`cont` opcodes.
- C comparison operators map directly to v10 native opcodes: `==`→`eq`,
  `!=`→`eq not`, `<`→`lt`, `<=`→`gt not`, `>`→`gt`, `>=`→`lt not`,
  `!`→`not`, `&&`→`not not swp not not and`, `||`→`not not swp not not or`.
  No helper subroutines are needed (v11 has native comparison opcodes).
- The emitted program begins with a fixed preamble: the libc IMPORTs,
  `extern "stdout"`, then `entry:` (replacing the old `jmp main`).
- String-literal escapes in the C input are decoded by the lexer
  (`\n \t \r \\ \' \" \0`).

## Tests

- `tests/hello.c` — original round-trip test.
- `tests/true.c`, `false.c`, `yes.c` — GNU coreutils adaptations; behavior
  diffed against the system binaries (see `tests/README.md`).
- `tests/echo.c`, `wc.c` — require deep `else-if` chains that stress the
  Enmerkar GC's string-concatenation path; may crash the transpiler on very
  deep nesting (>6 levels of `else if` with nested loops).

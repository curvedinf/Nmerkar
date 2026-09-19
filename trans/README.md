# trans — C → µFlux transpiler, written in µFlux

`trans.uf` is the first real program written in µFlux itself: a regex-based
lexer (the `match` op) plus a hand-rolled recursive-descent parser that reads a
C source file and prints **v11 text-encoding** µFlux source (per
`../SPEC.md`) to stdout.

> **v13.1 status:** `trans/trans.uf` has been ported to v13.1 (no
> `dup`/`swp`/`drop` — bind-and-leave with per-body draining; `add/sub/mul`
> mnemonics; `if_else`; `ret <value>` form; label bodies end with `ret`).
> Compiler bugs the port exposed were fixed (oscillating label-body
> propagation in `resolve_locals`/`compute_local_types` — now union-find;
> untracked-pointer truthiness). Round-trips passing: declarations,
> assignments, `while`, `for`, functions/params/calls, simple `if`
> (`trans/run_tests.sh`; `true`/`false`/`mini_gen` pass). `echo`/`wc`/`yes`/
> `hello` hit a transpiler-side segfault in if/else emission (`pif_else`
> stack management) — in progress.

## Usage

```
../comp/target/release/uf -c trans.uf -o trans
./trans prog.c > prog.uft
../comp/target/release/uf prog.uft            # compile + run
# or: ../comp/target/release/uf -c prog.uft -o prog && ./prog -- args...
```

Round-trip example: `tests/hello.c` produces output identical to `gcc`
(`hello, uflux` / `sum=55`). Larger adaptations of GNU coreutils programs
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
- Builtins: `argc`, `argv[i]` (via `extern "uf_argc"` / `extern "uf_argv"`),
  `__byte(p)` (first byte of `p`), `NULL` (0), `EOF` (-1).
- Comments (`/* ... */` and `//`) and blank lines are fine. No preprocessor
  lines (`#include` etc.) — call libc functions directly; the emitted
  preamble always IMPORTs `printf malloc free puts putchar getchar fputs
  fwrite strlen strcmp strncmp strcpy strcat exit fopen fclose fgetc` and
  declares `extern "stdout"`.
- `return expr;` in `main` becomes the process exit code (`call exit`).

## Deliberate omissions (parse-time or run-time errors)

- `/` and `%` (division/modulo): µFlux has no DIV opcode; the parser aborts.
  Digit counting etc. must use comparison chains (see `tests/wc.c`).
- `switch`, arrays (other than byte-indexing a `char*`), structs, enums,
  pointers other than `char*`, `long`/`float`/`double`, `goto`, the
  preprocessor.
- C `\x..` and `\0..` escapes **inside string/char literals**: µFlux string
  escapes are only `\n \t \r \0 \\ \"`, and an unknown escape drops the
  backslash. Use numeric codes instead (the test programs compare against
  e.g. 92 for backslash).
- **No recursion with parameters/locals**: the transpiler emits all variables
  as globals (`^name`), so a recursive call clobbers the caller's variables.
  Recursion in leaf functions without locals works.
- `&&`/`||` do not short-circuit: both sides are evaluated (normalized via
  `not not` then combined with `and`/`or`). Guard out-of-bounds dereferences
  with nested `if`s.

## Internals

- Lexer: each token class is matched by the µFlux `match` regex op; tokens are
  kept in a list (`toks`) of typed records and indexed by position (`pi`).
- Parser: single-pass, emits µFlux text as it parses — expressions map
  directly onto the stack machine. Every C variable gets a unique global
  slot `v0, v1, ...` (emitted as `^v0!`/`^v0@` in v11 syntax; tracked in
  a `vars` dict via `getq`/`set`).
- Control flow emits v11 structured opcodes: `if`/`ifelse`/`while` with
  quotation labels (`'_iN`, `'_cN`, `'_bN`). Each C `if`/`while`/`for`/`do`
  generates condition and body quotations whose labels are defined after the
  calling code. A deferred-output mechanism (`inq` flag + `qout` buffer)
  accumulates quotation bodies and flushes them at the end of each function.
  A label stack (`ls`) saves and restores `inq` and label numbers across
  recursive parser calls so nested control flow works correctly.
- `break` and `continue` emit the native µFlux `break`/`cont` opcodes.
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
  µFlux GC's string-concatenation path; may crash the transpiler on very
  deep nesting (>6 levels of `else if` with nested loops).

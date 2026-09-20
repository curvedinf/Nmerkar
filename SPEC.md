# Enmerkar Specification v14

Normative for `comp/` (the `nkr` compiler). The `trans/` transpiler targets the
text encoding (see final section).

**v13 is not backward compatible with v12**. See the v13 changelog section
below for the full list of breaking changes. The project is
undeployed/developmental.

Enmerkar is a language based on a **managed hidden stack**, compiled to C then native
via `cc`, designed for LLM-authored one-off scripts (low token count, fast,
reliable). Values flow through named local/global variables, literal constants,
and the return value of the immediately preceding op. The data stack still exists
as the runtime execution substrate, but it is managed by the compiler and runtime;
the programmer never sees or manipulates it directly. Programs are postfix
(execution order), no expression syntax.

Design pillars: small orthogonal opcode set; uniform container protocol by tag
dispatch; algorithmic efficiency by default (typed arrays, autovectorized C
loops, open-addressing hashes, Timsort, streaming channels); structured control
flow; self-contained scripts (file I/O, argv, shell-out, regex, JSON in core);
garbage-collected (scripts never `free` managed objects).

## v13 changelog

The following changes are effective as of v13:

1. **Label parameters.** A label definition declares input bindings:
   `label: a! b!` binds the caller's first pushed cell to `a`, the second to
   `b`. Callers push arguments before `_call`; structured ops pass values
   implicitly. `_!` discards a cell (v14: `^name!` global binds are removed).
   Arity = number of
   declared bindings; labels with no bindings have arity 0 (v12-compatible).
   **Missing args are a compile error** (v13.1; formerly they bound `null`). Excess cells are discarded.

2. **Explicit `ret` with stack draining.** Every code path through a label body
   must end with `ret` (compile error otherwise; bodies ending in
   `break`/`continue`/`if_else` are exempt). `ret expr` returns the operand's
   value; `ret a b c` builds a list from the operand's three values; bare `ret`
   returns `null` (or, for v12 compat, the top of the real stack if the body
   pushed something). On return, the callee's data stack is drained back to the
   caller's saved pointer and the single return value is pushed on top — every
   call boundary is self-contained.

3. **Container literals.** `[expr …]` builds a list, `{ key val … }` builds a
   dict (odd element count is a compile error), `[expr …] type array` /
   `[expr …] type tensor` build typed arrays/tensors. The brackets are ASCII
   `[` `]` `{` `}` in both encodings.

4. **Text-opcode renames.** Most text mnemonics are renamed to full English
   words or `snake_case` phrases (`len`→`length`, `push`→`append`,
   `cat`→`concat`, `clone`→`copy`, …; the complete mapping is in the opcode
   reference). `seq`/`sne` become `structural_equal`/`structural_not_equal`.
   Immediate ops are renamed (`_arr`→`_array`, `_sys`→`_syscall`,
   `_sizeof`→`_size_of`). The dense glyphs are unchanged.

5. **Inline math symbols removed.** `+`, `-`, `*`, `&` are no longer operators
   (use `add`, `sub`, `mul`, `and`). `-` appears only in number literals
   (`-5`); the dense sign/SUB lookback rule is gone.

6. **Weave rework.** Task bodies are label-shaped: `task name:` with input
   bindings, ending in `ret` (`endt` removed). `run` replaces `wrun` and may
   name a terminal task; tasks not reachable backward from the terminal are
   orphans (callable via `'name`, results not auto-computed). The `shutdown`
   op drains a running weave.

7. **New ops.** `pow`, `sqrt`, `lte`, `gte` (arithmetic/comparison) and
   `shutdown` (weave control) — five new opcodes occupying the retired slots
   13–15, 22, 24.

8. **Struct-field stride fix.** obj fields are stored as full 16-byte Cells,
   so struct layouts stride by 16 (`_size_of`/`_offset` reflect the corrected
   layout).

## Source encodings

Two encodings, identical semantics, auto-detected:

- **Dense** (`.en`) — single-token emoji glyphs, one glyph per op. Optimized
  for the Qwen3-0.6B tokenizer (every opcode glyph, v-space atom, and l-space
  atom is a single token).
- **Text** (`.ent`) — lowercase ASCII mnemonics, whitespace-delimited.

**Detection:** any char ≥ U+13000 → dense; else text. `.ent` extension is
conventional, not required. Inline source and stdin use the same detection.
MTU files may mix encodings freely (per-TU auto-detection).

### Dense Unicode spaces

| space | range | contents |
|-------|-------|----------|
| opcodes | U+1F300+ (emoji) | 214 slots (0..213), 192 live |
| v-space | U+1F941+ interleaved with other emoji | variable/label name atoms, runs fold |
| l-space | U+1F130+ (enclosed alphanumerics) | base-64 digit atoms (self-evaluating numbers) |
| delimiters | U+13100..U+13108 | chat-template delimiters, stripped pre-compilation |
| type glyphs | U+13110..U+13117 | int, float, ptr, byte, void, handle, str, bool |

Glyph assignments are in `comp/src/lex.rs` (`OP_GLYPHS`, `V_SPACE`, `L_SPACE`).
The deprecated v7 sequential glyph aliases (U+13000..U+13037) are removed.

## Comments and strings

`;` starts a line comment in both encodings. `"..."` is a string literal with
escapes limited to `\n \t \r \0 \\ \"` (any other `\x` copies x literally).
A bare `"..."` self-evaluates (pushes a string handle).

## Numbers (l-space grammar)

l-space atoms are base-64 digits: atom U+133A4+i has digit value i (0..63).

```
number := ['-'] lrun ['.' lrun] ['e' ['-'] lrun]
lrun   := one or more l-space atoms, big-endian
```

- Bare `lrun`: self-evaluating big-endian base-64 unsigned int (becomes int
  cell; overflow past u64 is a compile error).
- `lrun '.' lrun`: fixed-point (`d1/64 + d2/4096 + …`), becomes f64 cell.
- `'e' ['-'] lrun`: multiplies by 10^exp (decimal scientific). Any number with
  `.` or `e` is a float cell.
- Leading `-`: negates (v13: always a sign — see Inline math symbols below).
- `LIT` also accepts ASCII decimal/hex (`0x..`)/float literals (including
  negatives) and type keywords, and also accepts an l-run number after it.
- `--to-dense` emits a float as l-space `lrun '.' lrun` when `ip + frac` is
  bit-identical to the f64; otherwise it uses `LIT` plus an ASCII literal.
- Two adjacent l-runs fold into one number; generators must put whitespace
  between distinct numeric literals.

A stray `.` is a lexer error; a `-` not followed by an l-run is a lexer error
("stray '-' — use `sub`"). `e` begins an exponent only immediately after an
l-run; elsewhere it begins an ASCII identifier.

## Inline math symbols (removed in v13)

v12 allowed `+`, `-`, `*`, and `&` as inline operators for `add`, `sub`,
`mul`, and `and`. v13 removes these symbol operators: all arithmetic and
bitwise ops are written as words (`add`, `sub`, `mul`, `and`, `pow`, …).

- `-` is now **only** part of a number literal. In dense mode `-` is always a
  sign and must be followed by an l-run (`-5`); a `-` followed by anything else
  is a lexer error ("stray '-'"). The v12 sign/SUB lookback rule — which
  decided between sign and SUB from the previous token's value-pushing status —
  is removed.
- In text mode a standalone `-` is a lexer error ("'-' removed in v13 — use
  `sub`; negative numbers still parse"). `-5` still parses as a negative
  number literal.
- `+`, `*`, `&` as standalone tokens are lexer errors in both encodings
  ("use `add` / `mul` / `and`").

## Names, labels, and variables (v-space)

A run of v-space atoms folds into one name. Whitespace must separate two
adjacent v-runs meant to be distinct.

Variable semantics (v14):
- `<name>!` — store into **local** variable (call-scoped, fresh frame per CALL)
  and leave the value on the hidden stack for chaining.
- `<name>@` — push **local** variable.
- A name assigned in a TU's straight-line top level (before its first label)
  declares a **shared** variable; inside label bodies the plain name then
  reads/writes that shared var. See "Shared variables" below.
- `^` is **removed** (v14): a caret anywhere is a compile error. Globals are
  gone; use shared variables.
- A v-run after CALL/ADDR/`'` is a label **reference**.
- Any other bare v-run is a label **definition** (no colon needed).
- ASCII `name:` still defines a label; ASCII names work as jump targets.
- IMPORT/EXPORT/EXTERN/MACRO/STRUCT names remain ASCII.

Local variables exist from first assignment until the nearest enclosing RET.
if/while/for body labels are continuations that share the caller's frame.
Compile-time scope checks (v14): a plain name assigned (non-param) in more
than one label body is an error — the bodies are distinct frames, so the
writes would not alias; declare it shared or rename. Reading a name that is
never assigned anywhere is an error, and using a body's local from another
body is an error. Label parameter binds shadow a shared name within their
own body only.

## Label parameters

A label definition declares input bindings in its definition; callers push
arguments before invoking the label. This makes labels user-defined ops with
the same postfix calling convention as built-ins: arguments first, then the
operation.

```
3 4 _call add2        ; 7
add2: a! b!
  ret a@ b@ add
```

Parameter bindings are the pass-through assignment tokens, written contiguously
after the label's colon:

- `name!` — bind one cell to local `name`.
- `name@!` — no longer exists; `^name!` global binds were removed in v14.
  Bind to a local and store to a shared var if needed.
- `_!` — bind and discard one cell.

The first binding receives the first cell the caller pushed, the second the
second, and so on. Bindings must be contiguous at the start of the definition;
the first non-binding token ends the parameter list.

- **Arity** = the number of declared bindings. Labels with no bindings have
  arity 0 — all caller cells are discarded on entry, preserving v12 behavior.
- **Strict binding (v13.1)**: too few arguments at a call site → **compile error** when the
  stack depth is statically known (`label 'x' declares N parameter(s) but the call site
  leaves only M value(s)`), and a descriptive runtime error otherwise. Excess cells are
  silently discarded after the bindings are satisfied. Destructuring binds after
  multi-value returns may still receive `null` for excess slots (a different mechanism).
- Structured ops pass values implicitly: `5 'body for` pushes the loop index,
  and a `body: i!` binding consumes it.

```
5 'body for
body: i!
  i@ print
  ret
```

## Return values and stack draining

Every code path through a label body must end with `ret` — falling off the end
of a label is a compile error. Bodies that end in `break`/`continue` (loop-exit
continuations) or `if_else` (which transfers control to one of two bodies) are
exempt.

`ret` takes the expression that follows it as its operand:

- `ret expr` — return the single value the operand expression leaves.
- `ret a b c` — the operand nets three values; they are combined into a list
  and that list is returned. The caller destructures it with a bind list:
  `"cmd" _call run_cmd out! err! code!`.
- Bare `ret` — return `null`; for v12 compat, if the body pushed onto the real
  stack (a value sits above the frame base), that top cell is returned instead.

```
add2: a! b!
  ret a@ b@ add

run_cmd: c!
  c@ shell out! err! code!
  ret out@ err@ code@
```

On return, the callee's **entire data stack is drained** back to the caller's
saved pointer and the single return value is pushed on top of it. The caller's
stack pointer is saved just below the arguments at every call boundary —
`_call`, structured-op callbacks, weave tasks, `spawn` bodies, `try`/`retry`
bodies, and `filter`/`array_reduce`/etc. callbacks. Callees can therefore never
leak transient values into the caller's stack.

```
0 _call foo
add             ; 0 + 0 -> 0, not 0 + 42
foo:
  42            ; pushed but never named in ret
  ret 0         ; caller sees only 0; the 42 is drained
```

Recursion uses the same call stack as structured control flow; a label calls
itself the same way it calls any other label.

## Shared variables (v14)

`^` globals are removed. A **shared** variable is declared by a plain-name
assignment in a TU's straight-line top level — the code before the TU's first
label definition:

```
0 count!                     ; declares shared `count`, initializes to 0
main:
  4 spawn-workers
  ...
worker: n!
  25000 'loop 'body while
  ret
loop: n@ count@ lt ret
body:
  count++                    ; atomic add — see below
  n@ 1 add n!
  ret
```

- **Plain names everywhere.** Inside label bodies, a name declared shared at
  top level resolves to the shared variable; writes and reads are the usual
  `name!` / `name@` tokens.
- **Declaration site.** In multi-TU/directory mode, *each* TU's own top-level
  prefix declares shared vars (an `init` TU publishes cross-thread state this
  way). Duplicate declarations across TUs of the same name are fine (one var).
- **Atomicity.** Each shared variable is a C11 `_Atomic` seqlock cell
  (`<stdatomic.h>` only — portable to any conforming C11 toolchain). Reads are
  snapshot reads (retry on writer-in-flight or torn sequence); `x++` and
  `x += k` compile to a single atomic read-modify-write, so racing increments
  from multiple threads serialize with **no lost updates** — `n` threads doing
  `x++` `k` times each yield exactly `n*k`.
- **Stores consume.** A shared store (`count!`) pops its value off the hidden
  stack (it does not pass through like a local store).
- **Spawn transparency.** Spawned bodies get a fresh frame; shared vars are the
  way to publish results or counters across threads. They are GC roots and are
  scanned by the collector.
- **Scope errors.** A plain name assigned in more than one label body is a
  compile error (the bodies are distinct frames — the writes would not alias);
  reading a never-assigned name, or using another body's local, is an error.
  The fix in both cases: declare the name shared at the top level, or rename.
- **No `^`.** The caret is a hard lex error: "use plain names; declare a
  shared var with a top-level assignment".

## Type glyphs

U+13110..U+13117 = int(0) float(1) ptr(2) byte(3) void(4) handle(5→2)
str(6→2) bool(7→3) — handle/str are ptr aliases, bool a byte alias; void is 4
(SIZEOF void = 8, not useful).

- After `LIT`: pushes the type id.
- Directly after the type-taking immediates (`_array`/`_tensor`/`_obj`/`_cast`/
  `_size_of` in text, the corresponding glyphs in dense): type **immediate** —
  pushes the id with no LIT. The id lands on top of any preceding length, so
  ARR/TENSOR consume `[len, type]` with type on top.
- Elsewhere a bare type glyph pushes its id (expression position).
- ASCII type keywords (`int float ptr handle byte`) work in all the same
  positions; in v13 a bare type keyword pushes its type id in text mode
  (mirroring the bare type glyph in dense). `str` is not a bare keyword —
  `_str` is the immediate string op — and `void`/`bool` have no text keyword
  form.
- `array`/`tensor` are also plain ops taking `[len_or_list, type]` from the
  stack (see the opcode reference).

## Type tags

0 int, 1 float, 2 ptr, 3 byte, 4 void, 5 arr, 6 tensor, 7 list, 8 dict,
9 str, 10 chan, 11 atom, 12 buf, 13 obj, 14 bitmap, 15 time, 16 dur,
17 bloom, 18 iter.

## Runtime cell

`typedef struct { int tag; int64_t i; } Cell;` (16 bytes with alignment).
FLOAT stores the double **bit pattern** in `i`; `uf_f()` converts tag-aware.
Pointer payloads live in `i` (cast at use sites). Tagged objects (arr/tensor/
list/dict/str/chan/atom/buf/obj/bitmap/bloom) have a `{tag, len, esz}` header
preceded by a GC header. Execution state (data stack, call stack, locals) lives
in a `Ctx` so weave tasks and spawned threads each get a fresh one.

## Struct fields

`struct Name { field:type, … }` declares a record layout (OBJ). v13 stores obj
fields as full 16-byte Cells (`{tag, i}`), so every field is laid out at a
16-byte stride: consecutive scalar fields sit at offsets 0, 16, 32, … and a
struct's total size is `16 × field-count` (16 bytes minimum). `_size_of`
reports the total size and `_offset Struct.field` the field offset; both
reflect the 16-byte stride. Nested struct fields occupy the nested type's
total size (still a multiple of 16). Field access by name or offset goes
through the container protocol (`get`/`set`/`get_or_zero` with a string or int
key).

## Uniform container protocol

Six ops cover all element access, dispatched on the handle's tag:

| op | dict | list/arr/tensor | str | obj |
|----|------|-----------------|-----|-----|
| `get` | value (missing: dies) | elem at idx (OOB: dies) | byte at idx | field by name or offset |
| `get_or_zero` | 0 on miss | 0 on OOB | 0 on OOB | 0 on missing field |
| `set` | put | idx set (OOB: dies) | byte set | field set |
| `remove` | remove key (tombstone) | — | — | — |
| `contains` | key present | idx in bounds | substring found | field exists |
| `keys` | keys | — | — | field names |

`length` and `type_of` complete the protocol. A null (0) handle returns 0 from
`get_or_zero`/`contains`, dies elsewhere.

## Container literals

v13 adds compile-time literals for lists, dicts, and typed arrays/tensors. The
brackets are ASCII `[` `]` `{` `}` in **both** encodings — a deviation from the
proposal's "four new glyphs" plan: ASCII is guaranteed single-token for the
Qwen tokenizer.

- `[ expr … ]` — **list literal**. Each element expression is evaluated on the
  hidden stack and must net exactly one cell; the closing bracket consumes the
  cells and leaves one list handle.
- `{ key val … }` — **dict literal** from alternating key/value expressions.
  The element count must be even; an odd count is a compile error.
- `[ expr … ] type array` / `[ expr … ] type tensor` — **typed array/tensor
  literal**: the list is built, then `array`/`tensor` copies its elements into
  a typed array of the given element type.

```
[1 2 3] l!                        ; list
{ "a" 1 "b" 2 } d!                ; dict
[0 1 2] int array nums!           ; int array
[1.0 2.0 3.0] float tensor t!     ; float tensor
[] e!                             ; empty list
[x@ y@ z@] triplet!               ; elements may be arbitrary expressions
```

Multi-value returns inside a literal are not automatically destructured; use a
destructuring bind on the previous line if needed.

`array` and `tensor` are polymorphic in v13:

- `len type array` — v12 behavior: allocate an empty typed array of `len`
  elements.
- `list type array` — v13 behavior: allocate a typed array and copy the
  elements of `list` into it.

The same applies to `tensor`. Elements are coerced to the element type
(int/float/byte) on copy.

## Opcode reference

214 slots (0..213); 20 retired (indices 1–5, 25, 30–32, 39, 57–61, 76–77,
86–87, 151). The five v13 additions — `pow`, `sqrt`, `lte`, `gte`,
`shutdown` — occupy the previously retired slots 13–15, 22, and 24; the
remaining retired indices stay unusable. Each live opcode has a unique dense glyph; the text mnemonics are
the v13 full-word/`snake_case` names listed below. Glyph assignments are 1:1
and final in `comp/src/lex.rs`.

### Core stack, memory, and I/O

| idx | | mn | stack | notes |
|----|---|----|----|-------|
| 0 | 🌀 | `_lit` | → v | immediate follows (number/type glyph/keyword) |
| 1 | 😀 | `dup` | — | **retired in v12** |
| 2 | 🚀 | `ovr` | — | **retired in v12** |
| 3 | 🤍 | `drop` | — | **retired in v12** |
| 4 | 🌁 | `swp` | — | **retired in v12** |
| 5 | 😁 | `pick` | — | **retired in v12** |
| 6 | 🚂 | `add` | a b → a+b | |
| 7 | 🤐 | `sub` | a b → a−b | |
| 8 | 🌂 | `mul` | a b → a*b | |
| 9 | 😂 | `and` | a b → a&b | |
| 10 | 🚃 | `shr` | a → a>>1 | |
| 11 | 🤑 | `inc` | a → a+1 | |
| 12 | 🌃 | `dec` | a → a−1 | |
| 13 | 🪔 | `pow` | a b → a^b | C/Python pow; coerces, returns float |
| 14 | 🪑 | `sqrt` | a → √a | coerces, returns float |
| 15 | 🪒 | `lte` | a b → 0/1 | a≤b; coerces both sides to Number, NaN → 0 |
| 16 | 😃 | `for` | count addr → | pushes k per iteration |
| 17 | 🚄 | `_call` | (label operand) | |
| 18 | 🤒 | `ret` | → | v13: explicit return, operand expression (see Return values) |
| 19 | 🌄 | `_obj` | → h | type immediate (struct id) |
| 20 | 😄 | `get` | h k → v | polymorphic (protocol above) |
| 21 | 🚅 | `set` | h k v → v | polymorphic; returns stored value (pass-through) |
| 22 | 🪐 | `gte` | a b → 0/1 | a≥b; coerces both sides to Number, NaN → 0 |
| 23 | 🤓 | `array` | len_or_list type → h | polymorphic (v13): top is a length (v12) or a list to copy; `_array <type>` immediate form; 64-aligned typed array |
| 24 | 🪫 | `shutdown` | → | graceful weave shutdown: sets the drain flag (see Concurrency) |
| 26 | 🌅 | `copy` | h → h' | deep copy (was `clone`) |
| 27 | 😅 | `_cast` | h type → h | checked downcast (struct id); dies on mismatch |
| 28 | 🚆 | `macro` | (directive) | `macro name { body }` |
| 29 | 🤔 | `tensor` | len_or_list type → h | as `array`; 64-aligned |
| 33 | 🌆 | `setv` | value → value | `<v>!` local (pass-through); shared store consumes (v14) |
| 34 | 😆 | `getv` | → value | `<v>@` local / shared read (snapshot for shared) |
| 35 | 🚇 | `_str` | → h | bare `"…"` preferred |
| 36 | 🤕 | `concat` | a b → h | tag-dispatched: str/arr/list concat (was `cat`) |
| 37 | 🌇 | `format` | args… fmt → h | (was `fmt`) |
| 38 | 😇 | `buffer` | size → ptr | raw (untracked) buffer (was `buf`) |
| 40 | 🚉 | `copy_memory` | dst src n → | (was `bufcopy`) |
| 41 | 🤖 | `_addr` | → code address | `'label` |
| 42 | 🌈 | `load` | addr → value | raw memory read (was `loadx`); a string operand reads its first byte (unsigned) |
| 43 | 😈 | `store` | value addr → | raw memory write (was `storex`) |
| 44 | 🚊 | `_size_of` | type → n | (was `_sizeof`) |
| 45 | 🤗 | `_offset` | → n | compile-time `Struct.field` |
| 46 | 🌉 | `struct` | (directive) | `struct Name { field:type, … }` |
| 47 | 😉 | `malloc` | size → ptr | raw (untracked) |
| 48 | 🚌 | `free` | ptr → | raw only — never on GC handles |
| 49 | 🤘 | `_syscall` | args… num → ret | syscall by number (was `_sys`) |
| 50 | 🌊 | `gc` | → | forces a full mark-sweep collection |
| 51 | 😊 | `import` | (directive) | `import c"fn"(types)->ret` |
| 52 | 🚍 | `export` | (directive) | `export "name"` before a label |
| 53 | 🤙 | `extern` | → address | `extern "symbol"` — global C symbol via `__asm__` |
| 54 | 🌋 | `print` | v → | type-aware recursive representation; top-level strings print raw |
| 55 | 😋 | `scan` | fmt → list | fscanf semantics; list holds values followed by count |
| 206 | 🛎 | `entry` | → | marks program entry; implicit jump from pc 0 |

### Containers

| idx | | mn | stack | notes |
|----|---|----|----|-------|
| 56 | 🚐 | `dict` | → h | open-addressing hash map (FNV-1a) |
| 62 | 🤚 | `list` | → h | growable cell vector |
| 63 | 🌌 | `append` | h v → h' | (was `push`) returns possibly-realloced handle |
| 64 | 😌 | `pop` | h → v | empty: dies |
| 65 | 🚑 | `channel` | cap → h | bounded MPSC ring (blocking) (was `chan`) |
| 66 | 🤛 | `enqueue` | h v → | blocks while full; dies on closed chan (was `enq`) |
| 67 | 🌍 | `dequeue` | h → v | blocks while empty; closed+empty → 0 (was `deq`) |
| 68 | 😍 | `close` | h → | |
| 69 | 🚒 | `atomic` | v → h | atomic i64 cell (was `atom`) |
| 70 | 🤜 | `atomic_get` | h → v | atomic load (was `aget`) |
| 71 | 🌎 | `atomic_set` | h v → | atomic store (was `aset`) |
| 72 | 😎 | `atomic_add` | h n → old | atomic fetch-add (was `aadd`) |
| 73 | 🚓 | `cas` | h old new → 0/1 | compare-and-swap |
| 74 | 🤝 | `type_of` | h → tag | v10+ tag numbering (was `typeof`) |
| 75 | 🌏 | `length` | h → n | generalized (arr/tensor/list/dict/chan/bitmap/str) (was `len`) |
| 119 | 🌜 | `get_or_zero` | h k → v_or_0 | never dies on absence (was `getq`) |
| 120 | 😙 | `contains` | h k → 0/1 | membership (was `has`) |
| 121 | 🚣 | `orelse` | a b → c | a if truthy else b |
| 122 | 🤨 | `keys` | h → list | dict keys / obj field names |
| 152 | 🌦 | `remove` | h k → | (was `del`) dict: tombstone |

### Arithmetic & logic

int ops die on float/pointer operands (use CAST or `uf_f`-aware ops). `pow`,
`sqrt`, `lte`, `gte` (indices 13–15, 22) live in the core table above; they
coerce their operands via the universal coercion rules.

| idx | | mn | stack | notes |
|----|---|----|----|-------|
| 104 | 😕 | `div` | a b → a/b | int truncates toward zero; float f64. b=0: dies |
| 105 | 🚚 | `rem` | a b → a%b | C remainder (sign follows dividend). b=0: dies |
| 106 | 🤤 | `eq` | a b → 0/1 | loose equality (==); coerces per universal coercion rules |
| 212 | 🥡 | `structural_equal` | a b → 0/1 | strict equality (===); types and values must match (was `seq`) |
| 213 | 🥢 | `structural_not_equal` | a b → 0/1 | strict inequality (!==) (was `sne`) |
| 107 | 🌙 | `lt` | a b → 0/1 | numeric or string lexicographic |
| 108 | 😖 | `gt` | a b → 0/1 | as lt |
| 109 | 🚛 | `not` | a → 0/1 | 1 if a==0 |
| 110 | 🤥 | `or` | a b → a\|b | ints only |
| 111 | 🌚 | `xor` | a b → a^b | ints only |
| 112 | 😗 | `shl` | a b → a<<b | ints only; b<0 or b≥64: dies |
| 113 | 🚜 | `bnot` | a → ~a | ints only |

### Structured control flow

Compiler-resolved; quotation addresses via `'<label>`. `break`/`continue`
outside a loop are compile errors.

| idx | | mn | stack | notes |
|----|---|----|----|-------|
| 114 | 🤦 | `if` | cond body_addr → | CALL body if cond nonzero |
| 115 | 🌛 | `if_else` | cond then_addr else_addr → | (was `ifelse`) transfers control — a body may end with it |
| 116 | 😘 | `while` | cond_addr body_addr → | exit on 0, else CALL body, repeat |
| 117 | 🚢 | `break` | → | to end of nearest enclosing while/for |
| 118 | 🤧 | `continue` | → | next iteration of nearest enclosing loop (was `cont`) |

### Sequences

| idx | | mn | stack | notes |
|----|---|----|----|-------|
| 123 | 🌝 | `range` | start stop → list | ints [start, stop) |
| 124 | 😚 | `sort` | seq → seq' | Timsort (stable); list and arr |
| 125 | 🚦 | `filter` | list pred_addr → list' | keep elems where pred truthy |
| 126 | 🤩 | `any` | list pred_addr → 0/1 | short-circuits; empty → 0 (was `some`) |
| 127 | 🌞 | `all` | list pred_addr → 0/1 | short-circuits; empty → 1 (was `every`) |
| 164 | 🌩 | `group_by` | list fn_addr → dict | fn (elem → key); dict maps key → list (was `group`) |
| 165 | 😤 | `aggregate` | dict fn_addr → dict' | map each group's value-list through fn (was `agg`) |
| 166 | 🚶 | `unique` | list → list' | dedup, first-occurrence order, O(n) via dict |
| 167 | 🤳 | `flatten` | list → list' | flatten one level (was `flat`) |
| 168 | 🌪 | `chunk` | seq size → list | split into size-element pieces (last may be short); size<1: dies |

### Vector ops (128–154, plus 169–171, 185–186)

Operate on arr/tensor of numeric element type; autovectorized C loops; results
freshly allocated; die on non-arr input. A **bitmap** (tag 14) is a dense
LSB-first u64-word array — the mask currency of the `scalar_*`/`array_*`
family.

| idx | | mn | stack | notes |
|----|---|----|----|-------|
| 128 | 😛 | `scalar_add` | arr scalar → arr' | (was `vadd`) |
| 129 | 🚧 | `scalar_sub` | arr scalar → arr' | (was `vsub`) |
| 130 | 🤪 | `scalar_mul` | arr scalar → arr' | (was `vmul`) |
| 131 | 🌟 | `scalar_div` | arr scalar → arr' | scalar 0: dies (was `vdiv`) |
| 132 | 😜 | `array_add` | arr arr → arr' | length mismatch: dies (was `veadd`) |
| 133 | 🚨 | `array_sub` | arr arr → arr' | (was `vesub`) |
| 134 | 🤫 | `array_mul` | arr arr → arr' | (was `vemul`) |
| 135 | 🌠 | `array_div` | arr arr → arr' | any 0 divisor: dies (was `vediv`) |
| 136 | 😝 | `array_max` | arr arr → arr' | elementwise max (was `vemax`) |
| 201 | 😭 | `array_min` | arr arr → arr' | elementwise min (was `vemin`) |
| 137 | 🚩 | `scalar_eq` | arr scalar → bitmap | (was `veq`) |
| 138 | 🤬 | `scalar_lt` | arr scalar → bitmap | (was `vlt`) |
| 139 | 🌡 | `scalar_gt` | arr scalar → bitmap | (was `vgt`) |
| 140 | 😞 | `scalar_gte` | arr scalar → bitmap | (was `vge`) |
| 141 | 🚪 | `scalar_lte` | arr scalar → bitmap | (was `vle`) |
| 142 | 🤭 | `bitmap_and` | bm bm → bm' | (was `vand`) |
| 143 | 🌤 | `bitmap_or` | bm bm → bm' | (was `vor`) |
| 144 | 😟 | `bitmap_not` | bm → bm' | (was `vnot`) |
| 145 | 🚫 | `bitmap_count` | bm → n | popcount (was `vcount`) |
| 146 | 🤮 | `array_gather` | arr bm → arr' | keep set-bit elements (was `vgather`) |
| 147 | 🌥 | `sum` | arr → scalar | empty → 0 (was `vsum`) |
| 148 | 😠 | `mean` | arr → f64 | empty: dies (was `vmean`) |
| 149 | 🚬 | `min` | arr → scalar | empty: dies (was `vmin`) |
| 150 | 🤯 | `max` | arr → scalar | empty: dies (was `vmax`) |
| 153 | 😡 | `array_map` | arr fn_addr → arr' | elementwise fn (elem → elem) (was `vmap`) |
| 154 | 🚲 | `array_reduce` | arr init fn_addr → acc | generic reduction fn (acc elem → acc) (was `vfold`) |
| 169 | 😥 | `array_argsort` | arr → idx_arr | indices that would stably sort (was `vargsort`) |
| 170 | 🚹 | `array_search_sorted` | sorted_arr val → idx | binary-search insertion point (was `vsearchsorted`) |
| 171 | 🤴 | `array_where` | arr arr bm → arr' | blend: bit set → first arr, else second (was `vwhere`) |
| 185 | 😩 | `array_get` | h idx → v | direct typed array read, no handle validation (was `vget`) |
| 186 | 🛀 | `array_set` | h idx v → | direct typed array write (was `vset`) |

`array_map`/`array_reduce` let the family stay compact: `vsqrt`..`vceil` are
`array_map` over an IMPORTed libm fn; windowed/grouped/cumulative reductions
are `array_reduce`.

### Time (scalar cells: time tag 15, dur tag 16, both i64 nanos)

| idx | | mn | stack | notes |
|----|---|----|----|-------|
| 155 | 🤰 | `now` | → t | CLOCK_REALTIME nanos |
| 156 | 🌧 | `parse_time` | str fmt → t | `"unix"` (float s) or strptime(3) (was `time`) |
| 157 | 😢 | `format_time` | t fmt → str | `"unix"` or strftime(3); honors process TZ (was `timef`) |

Calendar arithmetic, durations, truncation, time-series joins are library code
(`mods/`).

### Bloom filter (tag 17; double-hashed FNV-1a, 1% FP at n)

| idx | | mn | stack | notes |
|----|---|----|----|-------|
| 158 | 🚴 | `bloom` | n → h | n<1: dies |
| 159 | 🤱 | `bloom_add` | h v → | ints by value, strings by content (was `badd`) |
| 160 | 🌨 | `bloom_test` | h v → 0/1 | 1 = maybe, 0 = definitely not (was `btest`) |

### Script I/O

| idx | | mn | stack | notes |
|----|---|----|----|-------|
| 161 | 😣 | `read_file` | path → str | whole file; not found: dies (was `slurp`) |
| 162 | 🚵 | `write_file` | path str → | create/truncate; error: dies (was `spit`) |
| 163 | 🤲 | `argv` | → list | program argv as list of strings |

### Shell

| idx | | mn | stack | notes |
|----|---|----|----|-------|
| 85 | 🌑 | `shell` | cmd → list | returns `[out, err, status]`; -1 spawn failure, 128+signal (was `sh`) |
| 88 | 😑 | `shell_stream` | cmd → chan | detached thread feeds stdout line-by-line (cap 64) (was `shp`) |
| 89 | 🚖 | `execute` | list → status | no shell; list is argv (elem 0 = program) (was `exec`) |

### Strings & regex

All results freshly allocated. Indices are **byte** indices. Embedded
backtracking regex engine (no dependencies).

Regex syntax: literals; `\` escape; `.` any char except EOS; `*` `+` `?` greedy
(with backtracking); `[...]` char classes (ranges, negation `[^...]`, `]`
first is literal); `^` start anchor, `$` end anchor; `|` alternation; `(...)`
capture groups (max 9, group 0 = whole match). Malformed pattern = runtime die.

| idx | | mn | stack | notes |
|----|---|----|----|-------|
| 90 | 🤠 | `regex_match` | str pat → list | returns `[groups, found]`; first match; group strings 0..n (was `match`) |
| 91 | 🌓 | `regex_replace` | str pat repl → str' | replace ALL; `\1`..`\9` backrefs (was `replace`) |
| 92 | 😒 | `regex_split` | str pat → list | pieces between matches; empty matches skipped (was `rsplit`) |
| 93 | 🚗 | `glob_match` | str pat → 0/1 | fnmatch-style (was `glob`) |
| 94 | 🤡 | `split` | str sep → list | literal separator; empty sep: dies |
| 95 | 🌔 | `join` | list sep → str | |
| 96 | 😓 | `slice` | seq a b → seq' | tag-dispatched (str/arr/list); Python slice semantics |
| 97 | 🚘 | `find` | str sub → idx | first occurrence, −1 on miss |
| 98 | 🤢 | `replace_all` | str old new → str' | literal replace all; empty old: dies (was `repl`) |
| 99 | 🌕 | `trim` | str → str' | strips isspace both ends |
| 100 | 😔 | `uppercase` | str → str' | ASCII uppercase (was `up`) |
| 101 | 🚙 | `lowercase` | str → str' | ASCII lowercase (was `down`) |
| 102 | 🤣 | `starts_with` | str affix → 0/1 | (was `starts`) |
| 103 | 🌘 | `ends_with` | str affix → 0/1 | (was `ends`) |

### Large-data & graph ops

No file-handle object type; every op is self-contained.

| idx | | mn | stack | notes |
|----|---|----|----|-------|
| 172 | 🌫 | `mmap` | path → str | read-only zero-copy string; GC-unmapped on sweep. All string ops work |
| 173 | 😦 | `file_each_line` | path fn_addr → | call fn (line → flag) per line; stops early when fn returns 0 (was `feach`) |
| 174 | 🚼 | `file_fold_lines` | path init fn_addr → acc | streaming reduce; fn (acc line → acc) (was `ffold`) |
| 175 | 🤵 | `file_split_lines` | path sep init fn_addr → acc | streaming split; fields via `field_get`/`field_int`/`field_float`/`field_slice`/`field_byte` (was `fsplit`) |
| 176 | 🌬 | `field_get` | field_idx → str | zero-copy field view (current file_split_lines line) (was `fget`) |
| 177 | 😧 | `field_int` | field_idx → int | parse field directly, no alloc (was `fatoi`) |
| 178 | 🚾 | `field_float` | field_idx → float | parse field directly, no alloc (was `fatof`) |
| 179 | 🤶 | `field_slice` | field_idx off len → str | zero-alloc field substring (was `fsget`) |
| 180 | 🌮 | `field_byte` | field_idx off → int | single byte from field, no alloc (was `fbyte`) |
| 181 | 😨 | `file_match_lines` | path pat → chan | spawn producer streaming regex-matching lines (cap 64); closed at EOF (was `fmatch`) |
| 182 | 🚿 | `bfs` | start fn_addr → list | breadth-first visit-order; fn (node → neighbors) |
| 183 | 🤷 | `dfs` | start fn_addr → list | depth-first pre-order; same fn contract |
| 184 | 🌯 | `find_first` | start fn_addr pred_addr → v_or_0 | BFS with early exit: first match or 0 (was `wfind`) |
| 187 | 🤸 | `add_to` | dict key amount → | dict[key] += amount; missing starts at 0 (was `addto`) |
| 188 | 🌰 | `field_add_to` | dict field_idx amount → | dict[field] += amount; no Str alloc (was `faddto`) |
| 189 | 😪 | `field_inc` | dict field_idx → | dict[field] += 1; no Str alloc (was `finc`) |

### JSON

| idx | | mn | stack | notes |
|----|---|----|----|-------|
| 190 | 🛁 | `parse_json` | str → v | object → dict, array → list, number → int/float, true/false → 1/0, null → 0 (was `json`) |
| 191 | 🤹 | `to_json` | v → str | dict keys must be strings; atom/chan/iter/bitmap/bloom: dies (was `unjson`) |

### Iterators (tag 18)

Single-use, mutable cursors. Every collection is iterable.

| idx | | mn | stack | notes |
|----|---|----|----|-------|
| 192 | 🌱 | `iter` | h → it | list/arr/tensor (elems), dict (keys), str (bytes), chan (until close), bitmap (set-bit indices) |
| 193 | 😫 | `next` | it → list | returns `[value, more]`; exhausted → `[0, 0]`; non-iter: dies |
| 194 | 🛋 | `collect` | it → list | drain into fresh list |
| 195 | 🤽 | `iter_map` | it fn_addr → it' | lazy map; fn (v → v') (was `imap`) |
| 196 | 🌲 | `iter_filter` | it pred_addr → it' | lazy filter; pred (v → 0/1) (was `ifilter`) |
| 197 | 😬 | `file_emit` | path it → n | stream any iterable to file, one item per line; returns count (was `femit`) |

### Error containment & threads

| idx | | mn | stack | notes |
|----|---|----|----|-------|
| 198 | 🛌 | `try` | body_addr → list | returns `[result, ok]`; success → `[value, 1]`, die → `[0, 0]` |
| 199 | 🤾 | `retry` | n body_addr → list | same as `try`; try up to n+1 times |
| 200 | 🌳 | `spawn` | body_addr → chan | detached thread; body must end with `ret`, whose value is enqueued + chan closed at body end; `dequeue` = join |

`die` unwinds to the nearest `setjmp` checkpoint pushed by `try`/`retry` (they
nest); with no checkpoint, `die` is fatal. No backoff, jitter, or exception
typing — count and containment are the whole policy.

### Conversion

| idx | | mn | stack | notes |
|----|---|----|----|-------|
| 202 | 🛍 | `parse_int` | str → int | strtoll base 10 (was `atoi`) |
| 203 | 🥀 | `parse_float` | str → float | strtod (was `atof`) |
| 204 | 🌴 | `format_int` | int → str | (was `itoa`) |
| 205 | 😮 | `format_float` | float → str | (was `ftoa`) |

### Script convenience (207–210)

| idx | | mn | stack | notes |
|----|---|----|----|-------|
| 207 | 🥛 | `has_args` | → 0/1 | 1 if `argv` has more than one element; replaces `argv length 1 gt` (was `hasargs`) |
| 208 | 🥜 | `arg_index` | idx → int | `argv[idx]` parsed as integer via `strtoll`; out of bounds: dies (was `argi`) |
| 209 | 🥝 | `sort_keys` | dict → key_list | `keys` + `sort` fused; returns dict keys sorted ascending (was `sortkeys`) |
| 210 | 🥞 | `top_n` | dict n → list | top-n `[key value]` pairs by value descending, ties by key ascending; selection sort (was `topn`) |
| 211 | 🥘 | `range_reduce` | count init label → scalar | fold over range 0..count; label `(acc i → acc)` called per iteration (was `rangefold`) |

### Modules & directives

| idx | | mn | notes |
|----|---|----|-------|
| 78 | 😏 | `use` | `use"name"` — link `-l<name>`, load `mods/<name>.ufm` |
| 79 | 🚔 | `mod` | `mod"name"` — translation-unit name |
| 80 | 🤞 | `pub` | export next label to global namespace |
| 81 | 🌐 | `weave` | begin task scope |
| 82 | 😐 | `task` | begin task body (inside weave): `task name:` with input bindings |
| 84 | 🤟 | `run` | schedule the DAG, wait; optional terminal task name (was `wrun`) |

`endt` (index 83) is removed in v13 — task bodies end with an explicit `ret`.

## Reflection

`type_of h → tag`, `length h → n` (generalized), `keys h → list` (dict keys or
obj field names). OBJ objects carry a tag header (tag obj, struct id); field
access by name or offset goes through the container protocol. CAST is checked
(compares struct id, dies on mismatch). Dynamic dispatch (v9 SEND/METHOD) is
removed.

## Modules: USE and binding manifests

`use"name"`: links `-l<name>` and loads the binding manifest `<name>.ufm`,
searched in `./mods`, `~/.nkr/mods`, then `$NKRMODPATH` dirs. A manifest is a
Enmerkar file containing IMPORT/EXTERN/STRUCT lines; compiled as part of the
USEing TU. Ships: `m.ufm`, `c.ufm`, `pthread.ufm`, `curl.ufm`, `sdl2.ufm`,
`ssl.ufm`. `-lpthread -lm` always linked; additional `-l<name>` per USE.

### FFI details

- `import c"fn"(types)->ret` — declares an unprototyped C function; args cast
  at call site. Generated C aliases the symbol (`__asm__`), so libc names
  already prototyped by the prelude are safe to import. Varargs: declare fixed
  params then `...` (e.g. `import c"printf"(ptr,...)->int`); at the call site
  push [fixed params..., varargs..., vararg-count] — the count of variadic
  arguments as the topmost int cell, e.g. `"x=%d" 7 1 _call printf`. The
  runtime pops the count first, so it never scans the stack for the format
  string (unrelated pointer cells deeper on the stack could otherwise be
  misread as the format). A missing/malformed count cell fails at runtime with
  `variadic call: top cell must be the vararg count`. `->int` is C `int`
  (32-bit), sign-extended into the 64-bit cell.
- `extern "symbol"` — pushes the address of a global C symbol for use with
  `load`/`store`. Runtime exposes `nkr_argc` and `nkr_argv` this way (though
  `argv` op 163 is preferred).

## Multiple translation units (MTU)

`nkr main.en lib.en ...`: first input is the main TU (execution starts at its pc
0; a TU's top-level flow never falls into the next TU). Per-TU:

- Optional `MOD"name"` header; default is filename stem. Glyph v-names, ASCII
  labels, shared variables (declared in the TU's top-level prefix), and macros
  are file-local. Local variables are call-scoped.
- `PUB` before a label exports it to the global namespace; CALL/`'` resolve PUB
  names across TUs. Duplicate PUB = compile error. STRUCTs, IMPORT/EXTERN/USE
  are global (deduped).
- Encodings may be mixed (per-TU auto-detection).
- Implementation: per-TU lex+parse, one merged codegen, one C file, one cc run.

## Directory mode and init threads

Bare `nkr` (or `nkr somedir/`) discovers source files:

- **Root**: `main.en`/`main.ent` is the entry point (first TU, pc 0). Error if
  not found. Other `*.en`/`*.ent` in root are additional TUs.
- **Subdirectory with `init.en`/`init.ent`**: compiled as TUs; the init file is
  flagged as an init TU — its top-level code runs in a separate thread,
  automatically spawned before main starts. Recurses into nested init subdirs.
- **Subdirectory without init**: ignored. `mods/` is never scanned.

Init threads are detached pthreads; each gets its own `Ctx`. Shared variables
are visible across all threads (atomic snapshot reads, atomic RMW for
`x++`/`x+=`). Coordination via chans and PUB/CALL.

Explicit-file mode is unchanged — no discovery, no init threads.

## Garbage collection

Precise, non-moving, stop-the-world mark-sweep. Handle stability (a handle is
never invalidated by a collection) keeps FFI, chan buffers, and weave task
results trivially safe.

- **Heap objects**: every tagged object allocated with a GC header, linked into
  a global list. Bodies holding cells (list/dict/arr/chan/obj) are scanned for
  children during marking; str/bitmap/bloom are leaf bytes.
- **Roots**: each `Ctx`'s data and call stacks, all shared variables,
  the full locals array of every `Ctx` (v13.2 — the innermost frame is where
  running code keeps its tensors; scanning conservatively past the frame
  pointer only over-retains, never frees live data), weave task results,
  chan queue contents, and per-thread temporary-root stacks (v13.2 — operands
  and in-progress results held in C locals across an allocation are pushed
  there by runtime ops via `UF_PROTECT`/`UF_UNPROTECT`, published with
  release/acquire so a concurrent collection on another weave worker can never
  miss or pop another thread's entry; dead threads' stacks are skipped).
  Generated code materializes pending compiler temporaries onto the data stack
  before polymorphic helper calls that may allocate.
- **Untagged pointers** (`malloc`, `buffer`): never traced, never freed by GC.
- **Trigger**: bytes allocated since last collection exceeds threshold (default:
  max(1 MiB, 2× live bytes)), and explicit `gc` op (50). Adjustable via
  `NKR_GC_THRESHOLD` env var or `--gc-threshold` runtime flag.
- **Concurrency**: stop-the-world via global GC mutex; weave workers park at
  allocation safepoints. Collections never start mid-weave join.
- **Non-goals**: compaction, generations, incremental/concurrent marking.

## Concurrency — weave

`weave` is the single construct for task graphs, servers, and composable
multithreaded processes. v13 task bodies are label-shaped: a task is introduced
by `task name:` (with optional input bindings after the colon) and ends with an
explicit `ret`.

```
weave
  task a:
    ret 1
  task b: a!
    ret a@ 2 add
run
```

- **Task bodies** use label syntax. `task name:` introduces a task; the body
  must end with `ret` (falling off the end is a compile error, exactly like a
  label). Bare `ret` returns `null`. The value named in `ret` becomes the task
  result; with fanout, each worker's `ret` value becomes one element of the
  published result list. `endt` is removed.
- **Inputs** are parameter bindings after the colon: `task b: a!` makes task
  `a`'s result available as local parameter `a`; `_!` discards. Bindings are
  evaluated left-to-right, matching the label
  parameter convention.
- **Fanout**: a numeric literal 1..64 before `task` declares the worker count:
  `4 task worker: item!`. Only the **first** input drives fanout and must be
  iterable (list/arr/tensor/chan/iter); additional inputs are broadcast
  (copied unchanged into every worker). Distribution is dynamic — items flow
  through an internal bounded chan and workers pull, so cheap workers never sit
  idle behind slow ones. A chan input drains until close; other iterables until
  exhaustion. Published result is a **list of per-item results in completion
  order**. Empty input → empty list. Count > 1 requires ≥1 input.
- **Static DAG**: task inputs must name tasks in the same weave block; unknown
  input or cycle = compile error. Task bodies are self-contained (labels
  task-local; two tasks may reuse v-names). Shared variables cross
  task boundaries; local variables are per-task.
- **`run [terminal]`**: `run` with no name executes every task (v12
  compatibility) and leaves the last task's result on the stack. `run <name>`
  names a terminal task: the compiler walks the DAG backward from the terminal
  and executes only reachable tasks. Tasks not reachable from the terminal are
  **orphans** — they stay in scope, remain callable via `'name` at runtime, but
  their results are not auto-computed. The terminal's result is left on the
  stack after `run`.

  ```
  weave
    task data:
      ret 3
    task process: data!
      ret data@ 2 mul
    task summary: data! process!
      ret data@ process@ add
    task orphan:
      ret 999        ; not reachable from `summary` — never runs
  run summary        ; 9
  ```

- **`shutdown`** (op 24, no return value) inside any task sets the weave's
  drain flag: running tasks finish, the weave joins all running tasks, and
  `run` returns; tasks that never started stay pending (their results remain
  unset). After `shutdown`, execution continues until the task's `ret`. In
  server weaves it is typically called from a request handler when a shutdown
  path is hit.

  ```
  weave
    task serve:
      0 n!
      'more 'step while
      ret
    more:
      ret n@ 100 lt
    step:
      n@ 1 add n!
      n@ 100 gte 'shut if
      ret
    shut:
      shutdown
      ret
  run
  ```

- **Runtime**: worker pool of `min(total declared workers, ncpu)` pthreads plus
  the calling thread. Each task runs with fresh data and call stacks; inputs
  are copied in as the initial stack in declared order. `spawn` target labels
  are ordinary labels subject to the same mandatory-`ret` rule.
- **Timing**: `NKR_WEAVE_DEBUG` env var prints per-task wall time, declared
  workers, items processed, retries, tolerated failures to stderr.

`spawn` (200): run the target label on a detached thread with a fresh `Ctx`;
returns a cap-1 chan immediately. The label body must end with `ret` (falling
off the end is a compile error); its return value is enqueued and the chan
closed at body end — `dequeue` on it is a join. Bare `ret` returns `null`. An
uncontained `die` in a spawned thread kills the process.

## Text encoding

Lowercase ASCII mnemonics, whitespace-delimited. Same Tok AST as dense.

- Tokens split on whitespace; `;` comments; `"..."` strings may contain spaces.
  `-` alone is a lexer error ("use `sub`"), `-5` is a number (no lookback
  rule). `+`, `*`, `&` are lexer errors ("use `add` / `mul` / `and`").
- Bare decimal/hex/float literals self-evaluate; `_lit` stays for type ids and
  numbers.
- Names: label def `name:`, refs `'name`, variables `name!`/`name@` (any
  identifier). Opcode mnemonics are reserved words.
- v13 mnemonics are full English words or `snake_case` phrases; the complete
  mapping is in the opcode reference tables above.
- `--emit-text` / `--emit-dense` round-trip between encodings. `--to-text` /
  `--to-dense` convert (writes `<stem>.ent` / `<stem>.en`, `-o` overrides).

## PRINT and SCAN

- **PRINT** (54): `v →`. `print` consumes the top-of-stack value and emits a
  type-aware, recursive representation. Top-level strings are printed raw (no
  quotes); nested strings and non-string values are rendered with unambiguous
  formatting. For formatted output, build a string with `format` and then
  `print` it.
- **format** (37): printf-style directives. `%d/%i/%u/%x/%X/%o` take i64,
  `%f/%e/%g` f64, `%s` a string, `%c` a code point, `%p` a pointer, `%%` a
  literal percent. A `*` width consumes an int argument (C semantics):
  `w 7 "%*d" format`. When an imported C variadic (e.g. `printf`) receives a
  Enmerkar string as a vararg, the ABI passes its character-data pointer, so
  `%s` prints the contents.
- **SCAN** (55): `fmt → list`. Each conversion reads stdin via fscanf:
  `%d/%i/%u/%x/%o` → i64, `%f/%e/%g` → f64, `%s` → fresh string handle. The
  returned list holds the converted values followed by the count. Input error
  aborts. Destructure with `list N get` or a bind list.

## Universal coercion

v13 keeps v12's uniform coercion: strict per-op type checking is replaced by
coercion based on the context in which a value is used. The rules are the same
for every op; there are no per-op exceptions.

### Numeric context

`add`, `sub`, `mul`, `div`, `rem`, comparisons, and vector ops coerce operands
to numbers. Exception: a **raw pointer** (an untracked ptr — from `malloc`,
`buffer` data, FFI returns, or `strstr`-style interior pointers into string
data) combined with an int under `add`/`sub` performs pointer arithmetic and
yields a raw pointer (`p 8 add load` reads the next 8 bytes). Tracked handles
(strings, lists, dicts) are unaffected: they coerce by content as before:

| Input | Result |
|-------|--------|
| int `42` | `42` |
| float `3.14` | `3.14` |
| str `"42"` | `42` |
| str `"0x1A"` | `26` |
| str `""` / whitespace | `0` |
| str `"hello"` | `NaN` |
| null / `0` handle | `0` |
| bool | `1` or `0` |
| list `[x]` | unwrap single element, then coerce |
| list `[]` | `0` |
| list `[x y …]` / dict / obj | `NaN` |

`NaN` propagates through arithmetic. Ops that must produce an integer truncate
`NaN` at the point of use, which is a die.

### String context

`concat`, `join`, `split`, `format`, and string ops coerce operands to strings:

| Input | Result |
|-------|--------|
| int | decimal string |
| float | decimal string or `"NaN"` |
| null / `0` handle | `"null"` |
| bool | `"true"` / `"false"` |
| list | `"1,2,3"` style join |
| list `[]` | `""` |
| dict / obj | `"[object Object]"` |

### Truthiness

`if`, `if_else`, `while`, `filter`, `any`, `all`, and bitmap ops use JS-style
truthiness. Falsy values: `0`, `""`, `null`, `NaN`, and empty collections.
Everything else is truthy (`1`).

### Tail-position if: early returns (v13.2)

When an `if`'s fall-through is only a value push and the enclosing body's
`ret` (tail position), the branch target's `ret` **returns from the enclosing
label** instead of resuming after the `if`:

```
solve: r! cols!
  r@ cols@ r@ n@ lt 'rec if       ; tail position: only `1 ret` follows
  1 ret                             ; not taken: return 1
rec: r! cols! ... ret               ; taken: rec's ret returns from solve
```

This is the early-return idiom for `_call`ed labels. The rule applies only in
real bodies (top level and outlined functions); inside inlined loop bodies a
target's `ret` still means "continue the loop", and `if`/`if_else` elsewhere
keep resume-after-branch semantics (the target's `ret` value is discarded).
A limitation to know about: `_call`ed-label recursion that passes frames
through branch parameters can still mis-bind when a pass-through assignment
(`x!`) precedes the call (vstack pass-through leak). v14 fixed the other half
of the old caveat: a callee's local frame is now pushed past the caller's
live slots, so nested calls can no longer alias the caller's locals
(the v13 "callee-frame overlap").

### Equality

- `eq` uses loose equality (`==`): `"3" eq 3` → `1`, `null eq 0` → `1`.
- `structural_equal` uses strict equality (`===`): types and values must match.
- `structural_not_equal` is strict inequality.
- `lt`/`gt` coerce both sides to Number for comparison.

### Collection coercion

Vector ops expecting `arr` coerce lists, dict values, strings (char codes),
bitmaps (set-bit indices), and scalars (single-element array). Bitmap ops coerce
collection elements to bits via truthiness.

### When coercion dies

Only when the result is genuinely undefined, never on input type:

- Integer truncation of `NaN` or `Infinity` (`div`, `rem`).
- `mean` / `min` / `max` on empty collection.
- Length mismatch in elementwise ops (`array_add`, etc.).

## Concrete grammar

Whitespace/comments skipped between tokens. Chat delimiters U+13100..13108
stripped anywhere.

```
program      := token*
token        := number | string | literal | varset | varget | labeldef | jump | op | directive
number       := ['-'] lrun ['.' lrun] ['e' ['-'] lrun]
             ; text mode also accepts ASCII decimal/hex/float literals
string       := '"' ... '"'
literal      := '[' token* ']' [<type> ('array' | 'tensor')]
             | '{' token* '}'           ; dict literal; element count must be even
name         := [a-zA-Z][a-zA-Z0-9_]*    ; must NOT start with '_' (reserved for _-prefixed ops)
varset       := name ('!' | setv-glyph)       ; local: store and pass through;
             ; shared (name declared at top level): store and consume
varget       := name ('@' | getv-glyph)
labeldef     := name ':' [param]*       ; params (v14): name! | _!
             | name                      ; label def without colon
jump         := (_call | "'") name
op           := opcode-glyph | text-mnemonic
             ; text mode: immediate-operand ops are _-prefixed — see section below
             ; v12: dup, ovr, drop, swp, pick are retired
             ; v13: + - * & symbol operators removed; `-` only in number literals
directive    := import c"name"(params)->ret | export "name" | extern "sym"
             | macro name { token* } | struct name { field:type, … }
             | use "name" | mod "name" | pub <labeldef>
             | weave task* run [name]
task         := [<count>] task name ':' token*   ; body must end with ret
```

`ret` is a prefix keyword: `ret [expr]` where the operand expression runs until
the next statement boundary (label, directive, task, another `ret`, or a
literal closer). `ret a b c` builds a list from the operand's values.

A sequence of consecutive `varset` tokens after a multi-return op is a
**destructuring bind**: each `name!` binds the next slot of the returned tuple,
and `_!` discards a slot. Bind arity need not match tuple length; excess binds
receive `null` and unbound trailing slots are auto-discarded.

Quotation-taking ops (if/if_else/while/for, try/retry, filter/any/all,
array_map/array_reduce, iter_map/iter_filter, file_each_line/file_fold_lines,
bfs/dfs/find_first, spawn, group_by/aggregate, fanout bodies) take label
addresses on the hidden stack — written `'<label>` — resolved by the compiler.
`break`/`continue` valid only inside a lexically enclosing `while`/`for` body
in the same function (compile error otherwise).

## Immediate-operand opcodes (_-prefixed, text mode only)

Opcodes starting with `_` take a **compile-time immediate**: the next source
token is consumed as a label name, type name, or numeric operand at compile
time. The immediate operand never touches the runtime stack — the value it
denotes is baked into the generated code or resolved to an address by the
compiler.

All other opcodes operate purely on the runtime stack.

This makes the distinction visible at a glance: `_call foo` consumes the
token `foo` as a label reference, whereas `add` reads its inputs from the
hidden stack at run time.

User-defined identifiers (variables, labels) may **not** start with `_` —
the prefix is reserved for these opcodes.

Dense/glyph mode is unaffected: glyphs dispatch by codepoint, not by name,
so no prefix is needed (or possible) there.

| mnemonic | opcode | immediate operand | stack effect |
|----------|--------|-------------------|--------------|
| `_lit` | LIT | number / type glyph / type keyword | → v |
| `_call` | CALL | label name | call (see notes) |
| `_addr` | ADDR | label name (`'label`) | → code address |
| `_syscall` | SYS | syscall number | args… num → ret |
| `_str` | STR | string literal | → h |
| `_size_of` | SIZEOF | type name / Struct name | type → n |
| `_offset` | OFFSET | `Struct.field` | → n |
| `_obj` | OBJ | type name (struct id) | → h |
| `_cast` | CAST | type name (struct id) | h type → h |
| `_array` | ARR | type name | len → h |
| `_tensor` | TENSOR | type name | len → h |

## Codegen notes

Pipeline: tokens → parser (labels/macros/structs/imports/v13 locals) → C with
computed-goto threaded interpreter → `cc -O2 -w`.

CLI modes: `nkr prog.en` (compile + run, cached binary in `$TMPDIR/nkr-cache/`);
`nkr -c prog.en -o bin` (compile only); `nkr --emit-c prog.en` (dump C); `--emit-text`/
`--emit-dense` (encoding conversion); `--to-text`/`--to-dense` (convert). First
positional arg is a file if it exists, otherwise inline source. Everything after
`--` is forwarded as program argv.

**`--debug` / `-D`**: compiles in debug mode (`cc -O0 -g`). Disables local-variable
register caching so locals are always memory-resident and accurate. On any fatal
runtime error (`die()`), prints a crash dump to stderr with: call stack (label
names + PCs), local variables per frame (names + values), and global variables
(names + values). In normal mode, no metadata tables are emitted and `die()`
behaves as before. Debug binaries are cached separately.

Always-on (non-debug) error messages include the operation name and stack depth:
e.g. `stack underflow in op_print (sp=0)`, `stack overflow in op_push (sp=1048576)`.

Codegen optimizations (`comp/src/gen.rs`):
- **Basic-block stack virtualization**: within straight-line blocks, ds push/pop
  become C locals; spilled to real ds at jump targets and control-flow edges.
- **Type specialization**: when both arithmetic operands have known types,
  emits raw C arithmetic (no tag checks), enabling SSE/autovectorization.
- **FOR inlining**: a FOR preceded by ADDR of a compile-time-known label, whose
  body is structurally inlinable, becomes a direct C `for` loop over a renamed
  inline copy. Depth cap 8.
- **Deferred variable stores**: SETV writes a temp cache within a block; dirty
  temps flushed at block boundaries.

Linking: always `-lpthread -lm` plus `-l<name>` per USE.

## Totals

214 opcode slots (0..213); 20 retired indices (never reused); **192 live
opcodes**. The five v13 additions — `pow`, `sqrt`, `lte`, `gte`, `shutdown` —
occupy the previously retired slots 13–15, 22, and 24. Every live opcode has a
unique dense glyph codepoint.

## Non-goals

B-tree/trie/skiplist/R-tree/suffix-array index structures (library candidates in
`mods/`); dataframe ops; HTTP clients in core (`use"curl"`); Roaring bitmaps;
generational/incremental/compacting GC; async I/O; object-file linking;
pkg-config probing; hand-written SIMD (autovectorization first); remote weave
executors; MSP/mobile targets.

## C → Enmerkar transpiler (trans/)

`trans/` is a C-subset → Enmerkar transpiler written as a standalone Rust
crate (std-only; modules: lexer, parser, AST, emitter) emitting the text
encoding. Supported subset, libc IMPORT preamble, emission model
(quotation-label control flow, direct calls with fresh per-call frames,
`fr`/`rv` shared-register early-return guards), and the gated
test pathways (system-binary round-trips plus per-operation output gates)
are documented in `trans/README.md`.

---

# v14 addenda

## Shared variables replace globals

`^` is removed (hard lex error). Plain names only:

- A plain-name assignment in a TU's straight-line top level (before its first
  label) declares a **shared variable** — see "Shared variables (v14)" above.
- Compile-time scope checks: a name assigned (non-param) in more than one
  label body is an error (distinct frames, non-aliasing writes); a never-
  assigned name read, or another body's local used cross-body, is an error.
  Label parameter binds shadow a shared name within their body.
- Shared vars are C11 `_Atomic` seqlock cells (`<stdatomic.h>` only — no
  platform-specific intrinsics; compiles anywhere with a C11 toolchain).
  Reads are snapshots; `x++`/`x += k` become a single atomic RMW
  (no lost updates across threads); shared stores consume their value.
- MTU/directory mode: every TU's own top-level prefix may declare shared
  vars; an init TU publishes cross-thread state this way.

## Callee frames no longer overlap the caller

A `_call` now pushes the callee's local frame past the **caller's** live
slots (the caller body's slot count), not by the callee's own count. Small
callees can no longer alias the caller's locals — the v13 "callee-frame
overlap" limitation is gone. Spawned bodies get a fresh `Ctx` and are
unaffected.

# v13.1 addenda

## Strict label-call arity

Missing call arguments no longer bind `null`. The compiler tracks a static
stack depth per label body (an effects table over all ops; dynamic regions are
poisoned conservatively) and rejects `_call`s whose site leaves fewer values
than the callee declares. Dynamically-shaped sites fall back to a runtime
check that dies with a message naming the label, parameter, and declared arity.

## Sandboxing and capabilities

`nkr` compiles unrestricted. `nkrsb` — built by default alongside `nkr`
(`cargo build --release` produces both; `NKR_SANDBOX_CONFIG=<file.ufs>` at build
time bakes a config into **both** binaries) — always bakes a capability
config: the repo default `comp/sandbox.ufs` unless overridden.

**Capabilities** (gated ops): `fs.read` (read_file, mmap,
file_each_line/fold_lines/split_lines/match_lines), `fs.write` (write_file,
file_emit; mmap needs both), `proc` (shell, shell_stream, execute), `ffi.use`
(`USE"..."`, plus per-module allow/deny), `ffi.import` (`import c"..."`,
`extern`), `raw.syscall` (`_syscall`), `raw.mem` (malloc, free, buffer,
copy_memory, load, store), `host.argv` (argv, has_args, arg_index).
Print/scan, time ops, threads, GPU compute, and all pure compute (including
matrix arithmetic) are always allowed.

**Config format (`.ufs`)** — line-based, `;` comments, `[policy NAME]`
sections; keys `deny`/`allow` (capability patterns; `*` and `prefix.*`
wildcards; a more specific allow beats a deny), `workspace` (fs roots),
`allow-module`/`deny-module`, and `name` (names the top-level policy):

```
deny  proc ffi.import raw.* host.argv
allow fs.read fs.write
workspace .
allow-module m curl

[policy pure]
deny fs.* proc ffi.* raw.* host.argv
```

Default policies baked into `nkrsb`: **pure** (computation only), **data**
(default — sandboxed filesystem, no network, no subprocesses), **web** (adds
HTTPS modules curl/ssl, denies subprocesses/raw FFI), **build** (broader fs
`~ /tmp`, subprocesses, still no raw host access).

**CLI** (both binaries): `--policy NAME` (select among baked policies),
`--sandbox FILE` (tighten with a `.ufs`; repeatable; may only restrict —
denies union, workspace roots and module allowlists intersect; runtime files
cannot define policies), `--workspace DIR` (add/intersect a root),
`--caps` (print the effective capability report and exit).

**Workspace**: when `fs.*` is allowed and a workspace is configured, every
file open resolves via realpath (parent for not-yet-existing write targets)
and must fall under a root, else the program dies with
`sandbox: path '<p>' is outside the workspace roots [...] (policy '<name>')`.
Default roots: the main program's directory and `$TMPDIR/nkr`. Best-effort
(realpath prefix check; symlink escapes caught, TOCTOU not).

Denied ops are **compile errors** naming the op, capability, policy, and
remedy.

## Capability discovery: `cap` / `caps`

- `"<name>" cap → 0/1` — query any capability name above, plus pseudo-caps
  `fs.workspace` (a workspace restriction is active) and `compute` (GPU
  offload permitted). Never sandbox-gated itself.
- `caps → dict` — `{"policy" ..., "<cap>" 0/1 ..., "fs.workspace" 0/1,
  "workspace" [roots...], "modules" [...|"*"], "device" "auto"}`.
- CLI: `uf --caps` prints the effective report.

## Polymorphic matrices and arithmetic

Matrices are 2-D tensors: `[rows cols] type tensor` builds a zeroed matrix
(tag `matrix`, `type_of` → 20; row-major flat storage, `length` =
rows·cols, `get`/`set` index flat). `[rows cols v0 v1 …] type tensor` builds
one from flat row-major data (len = rows·cols+2). The 1-D tensor form is
unchanged.

The core arithmetic ops are polymorphic by operand type:

| op | scalar×scalar | array×array / matrix×matrix | array/matrix × scalar | matrix×vector / vector×matrix |
|----|---------------|------------------------------|-----------------------|-------------------------------|
| `mul` | numeric | elementwise (shape-checked) | broadcast | **matmul** / **matvec / vecmat** |
| `add` `sub` `div` | numeric | elementwise (shape-checked) | broadcast | n/a |

Shape/dim mismatches die with the dims in the message. `sum`/`mean`/`min`/
`max` reduce matrices flat; `sqrt` applies elementwise to arrays/tensors.
New opcode **`transpose`**: `mat → mat'` (swaps rows/cols; dies on non-matrix).
`scalar_add/sub/mul/div` and `array_add/sub/mul/div` are deprecated aliases of
the core ops (kept parseable). `mul` on two equal-length 1-D arrays is
elementwise, **not** a dot product (use `mul` then `sum`).

## GPU compute offloading (Vulkan)

No opt-in flag. When a Vulkan shader toolchain (`glslc` or
`glslangValidator`) is present, the device mode is not `cpu`, and the program
contains at least one GPU-eligible op, the compiler compiles its **static
shader library** (float64 elementwise/broadcast add/sub/mul/div, matmul,
matvec, reductions sum/min/max, sqrt, transpose) to SPIR-V, embeds the blobs
in the generated C (`#define NKR_GPU`), and links `-lvulkan`. Kernels are only
ever **launched**, never generated from user code.

**Devices**: default `auto` enumerates Vulkan devices and picks, hardware
first (discrete > integrated > software), the one with the most free VRAM
(`VK_EXT_memory_budget` when available). `--device cpu` compiles GPU code out
entirely (v13 behavior). `--device vk<N>` pins a device (descriptive error
listing what exists if unavailable). Zero devices → silent CPU.

**Eligibility & threshold**: eligible ops are add/sub/mul/div (all polymorphic
forms), sqrt, sum/mean/min/max, transpose. A launch happens only when the
element/work count clears `NKR_GPU_MIN` (env, default 65536); otherwise the CPU
implementation runs. Any Vulkan failure degrades permanently to CPU — the GPU
is a fast path, never a correctness dependency.

**First-run CPU vs GPU (no autotuning)**: default `auto` never times both
backends and never runs a calibration trial. It uses a **static** estimate of
work vs host-visible transfer vs Vulkan init:

- **matmul** (`mul` on two matrices): offload only when `rows·k·cols`
  FLOPs clear `NKR_GPU_MATMUL_MIN` (env, default 400·2²⁰ ≈ 4.2e8, between
  512³ and 768³). Smaller squares (README: N=512) stay on CPU on the first
  run of a fresh binary; N=1024/2048 still go to the GPU.
- **Fused regions / weave-task elementwise**: if Vulkan has not been
  initialized yet, auto additionally requires `NKR_GPU_ARITH_MIN` elements
  (default 8M). Black-scholes at N=2M therefore stays CPU on first run;
  `--device vk<N>` still launches.
- Pipelines are created **lazily** (only the kernels a launch actually
  uses). `--device vk<N>` pins GPU and skips the auto estimate.

**Weave-task compilation**: a weave task whose body (after its input binds,
before `ret`) is a straight-line chain of elementwise arithmetic
(add/sub/mul/div/sqrt) over its inputs — every intermediate explicitly bound
with `x!` — is compiled into ONE fused float64 kernel: inputs staged in once,
every op executed on-device, result staged out once. Tasks containing anything
else (tensor construction, control flow, calls, non-eligible ops, global
reads) decline fusion and run on CPU unchanged. Fused-task output is
bit-identical to the CPU body. Concurrent tasks serialize their GPU work on an
internal mutex. Reference: `comp/tests/t14_task_gpu.ent` (black-scholes chain:
~60 per-op launches collapse to 4 fused kernels; 7.3s → 0.42s at N=2M).

**Elementwise region fusion (v13.2)**: the same analysis generalized to plain
code. A maximal straight-line run of eligible instructions (local/global
reads, local binds, float/int literals, `add/sub/mul/div/sqrt`) anywhere in
the top-level flow is a **fusable region** when every value it produces is an
elementwise function of its input tensors: up to 4 **live-out** locals (read
after the region — intermediate locals never read again stay internal), with
inputs up to 7 total buffers. `array_reduce` over a bound literal-only list
whose body is pure elementwise arithmetic (globals allowed) is **unrolled at
compile time** into the region expression — the source keeps its fold shape,
the machine gets one pass. The compiler emits one guarded block at the region
head: first a fused multi-output GPU kernel launch (one stage-in, whole chain
on-device, one stage-out per output), then raw `double` C loops
(restrict pointers, vectorizable — programs build with `cc -O3`), and only if
both decline the original per-op code runs. Declines are static or runtime
(inputs not same-length float64 tensors). Fused division carries an inline
zero-divisor `die`, preserving the per-op semantics exactly; on the GPU the
documented inf/nan divergence applies. Results are bit-identical to the
per-op path: same ops, same order, no reassociation. Regions in inlined
loops, outlined label bodies, or weave tasks are not analyzed (tasks have
their own fusion). Debug: `NKR_DEBUG_REGION=1` prints fused regions;
`NKR_DEBUG_REGION2=1` traces declined walks. Reference:
`bench/src/blackscholes/blackscholes.ent` (the 19-coefficient polynomial as a
constant-list fold — 120 instructions unroll into one fused loop/kernel).

**Per-op staging pool (v13.2)**: launches stage through one persistent
mapped HOST_VISIBLE (HOST_COHERENT when available) buffer, suballocated per
launch via 256B-aligned offsets and grown on demand — no per-op
`vkAllocateMemory`/`vkMapMemory` churn. Transfers remain synchronous
(upload → dispatch → wait → download).

**Per-op arith threshold (v13.2)**: a *single* add/sub/mul/div moves ~3×n×8
bytes for one op of work — on host-visible staging that loses to the CPU
typed fast path until n is large, so per-op arith offload additionally
requires `NKR_GPU_ARITH_MIN` elements (env, default 8M). Matmul on `auto`
uses `NKR_GPU_MATMUL_MIN` FLOPs (see first-run estimate above); a pinned
`vk<N>` device still uses `NKR_GPU_MIN` so small squares can be forced
onto the GPU. Fused regions and reductions use `NKR_GPU_MIN`, with the
auto first-run `NKR_GPU_ARITH_MIN` extra floor when Vulkan is not yet up.

**Determinism**: elementwise/broadcast results are bit-identical to the CPU;
reductions and matmul may reassociate (benchmarks compare within tolerance;
`div` on the GPU yields inf/nan for zero divisors where the CPU dies —
documented divergence). Sandboxing: GPU compute is pure compute — allowed
under every policy.

**Backend abstraction**: `comp/src/compute.rs` — `ComputeBackend` trait with
the Vulkan implementation first (CUDA/HIP can sit behind the same interface).

## New opcodes (v13.1)

| # | glyph | mnemonic | stack effect / notes |
|---|-------|----------|----------------------|
| 86 | 🤡 | `cap` | `name → 0/1` — sandbox capability query |
| 87 | 🌔 | `caps` | `→ dict` — full capability report |
| 214 | 🔀 | `transpose` | `mat → mat'` — swap rows/cols |


# SPEC_HISTORY.md — Operation History

When each operation (opcode) was added or changed, newest first, one
sentence per operation. Dates and commit hashes are from `git log`; slot
numbers are the current `OP_NAMES` indices in `comp/src/lex.rs`. Retired
operations are listed with their fate. Behavior-only changes (same opcode,
new semantics or performance) are listed under the commit that made them.

## v15 — 2026-09-20 (implicit loads + dense tag glyphs, this changeset)

- `getv` (34, 😆) — changed: source syntax is now a **bare name** (implicit
  load) in both encodings; the `<name>@` varget suffix and its glyph-suffix
  spelling are removed (lex error with migration hint). Slot and semantics
  unchanged.
- Dense label definitions — changed: a bare v-run no longer defines a label
  (bare v-runs load variables); dense label definitions are `name` + 🏷
  (U+1F3F7, single-token). Text keeps `name:` as-is.
- `_size_of`/`_offset`/`_obj`/`_cast` struct-name immediates — changed: dense
  spelling is now a dedicated tag glyph + ASCII name (📏 name, 📍 name.field,
  📦 name, 🎭 name; all single-token, U+1F4CF/U+1F4CD/U+1F4E6/U+1F3AD),
  replacing the v14 `@sizeof:`-style sentinel idents. `@` no longer appears
  in any encoding; `@`-prefixed source tokens are lex errors (also closing
  the text-mode fallthrough that let hand-written `@flush`/`@liststart`
  reach the parser).
- Macros vs variables — changed: bare-name resolution is sentinel → macro →
  implicit load; storing into a macro's name is a compile error.
- Dense `name++`/`name+=` — fixed: the dense lexer now accepts the RMW
  suffixes the emitter writes (previously emitted dense could not re-lex).

## v14.1 — 2026-09-20 (strictness + casting, this commit)

- `strict` (215, 🔒) — added: postfix compile-time marker that makes the
  value reject all implicit coercion from then on, with static contagion
  through variables, ops, calls, and callbacks.
- `loose` (216, 🔓) — added: postfix compile-time marker removing strictness
  from a value; only an explicit `loose` clears it, contagion re-applies.
- `cast` (217, 🔁) — added: dynamic postfix cast `[v type] → v'`, the
  explicit form of universal coercion (parses strings for `int`/`float`,
  renders for str tag 9, checks struct ids ≥1000), always legal on strict
  values.
- `_cast` (27, 😅) — changed: the immediate now accepts all static-castable
  types — `int`/`float`/`ptr`/`byte` convert raw C-style (struct names keep
  the checked downcast) — and literal operands fold at parse time.
- Every implicitly-coercing op (`add`, `eq`, `format`, `if`, vector family,
  …) — changed: consuming a strict operand is now a compile error naming the
  op and remedy; runtime behavior is unchanged.

## 2026-09-19/20 — performance era (87f87d8..456bd48)

- `field_int`/`field_float`/`field_byte` — changed (456bd48): inlined as
  typed C reads instead of allocating intermediate cells.
- `file_split_lines` — changed (31f40c8): falls back to `getline` on pipes
  and stdin where mmap is impossible.
- `field_get`-style field splitting — changed (e5159ed): single-character
  separators split via `memchr`.
- `dict` (open-addressing table) — changed (5f33c1b): grows from cap 64 by
  4× to cut rehashing.
- `get`/`set` on int-keyed dicts — changed (5281af3): integer-key hash fast
  path used by BFS-style workloads.
- typed-array `get`/`set` — changed (235a8fa): specialized int-array
  element access in hot loops.

## v14 — 2026-09-19 (87f87d8)

- `setv`/`getv` (33/34, `x!`/`x@`) — changed: plain names assigned at a
  TU's top level now declare shared variables backed by atomic seqlock
  cells (snapshot reads, atomic `x++`/`x+=`), replacing v13 `^` globals.
- `x++` / `x+=` — added as sugar: expand to atomic read-modify-write on
  shared variables and to add/store on locals.

## 2026-09-19 — modern-era additions (6cc009d, "gnu transpiling working")

- `pow` (13, 🪔) — added: C/Python power, coercing operands, returns float.
- `sqrt` (14, 🪑) — added: coerced square root, elementwise on arrays.
- `lte` (15, 🪒) / `gte` (22, 🪐) — added: coerced comparisons, NaN → 0.
- `shutdown` (24, 🪫) — added: sets a weave's drain flag for graceful
  task-graph shutdown.
- `cap` (86, 🌑) / `caps` (87, 😑) — added: sandbox capability query and
  full capability report (never sandbox-gated themselves).
- `transpose` (214, 🔀) — added: swaps rows/cols of a 2-D tensor.

## v13 — 2026-08-08 (9902c39; spec cba9516)

- No opcode-table changes: v13 rebuilt calling (label parameters,
  `ret expr`, stack draining at every call boundary) around the same ops.
- `endt` (83) — retired: task bodies now end with an explicit `ret`, so the
  end-task marker lost its mnemonic.

## v12 — 2026-08-08 (915f9a7)

- `structural_equal` (212, 🥡) — added: strict equality (===), types and
  values must match.
- `structural_not_equal` (213, 🥢) — added: strict inequality (!==).
- `dup`/`ovr`/`drop`/`swp`/`pick` (slots 1–5, now `~1`–`~5`) — retired:
  named variables replace stack shuffling; any use is a compile error.

## 2026-08-07 — `_`-prefix and script convenience (a6e7bf0)

- All immediate-operand mnemonics (`_lit`, `_call`, `_addr`, `_syscall`,
  `_str`, `_size_of`, `_offset`, `_obj`, `_cast`, `_array`, `_tensor`) —
  changed: text mode now `_`-prefixes compile-time-immediate ops so they are
  visible at a glance; identifiers may no longer start with `_`.
- `has_args` (207, 🥛) — added: 1 when argv has more than one element.
- `arg_index` (208, 🥜) — added: argv[idx] parsed as an integer.
- `sort_keys` (209, 🥝) — added: dict keys + sort fused.
- `top_n` (210, 🥞) — added: top-n `[key value]` pairs by value descending.
- `range_reduce` (211, 🥘) — added: fold over range 0..count via a label.
- `field_inc` (189, 😪) — added (replacing `FCOUNT`): dict[field] += 1 with
  no string allocation.

## v11 — 2026-08-07 (7c27eed)

- No opcode-table changes: v11 introduced per-call local frames and
  register-cached `x!`/`x@` locals beneath the existing op set.

## 2026-08-07 — direct array access (b5afeb2)

- `array_get` (185, 😩) — added: direct typed-array read without handle
  validation.
- `array_set` (186, 🛀) — added: direct typed-array write.
- `field_slice` (179, 🤶) — added: zero-alloc field substring.
- `field_byte` (180, 🌮) — added: single byte from a field, no allocation.

## 2026-08-06 — file fields, conversions, entry (854b63a)

- `file_split_lines` (175, 🤵) — added: streaming split of a file or pipe
  into numeric fields.
- `field_get` (176, 🌬) — added: zero-copy field view of the current line.
- `field_int` (177, 😧) — added: parse a field directly to int.
- `field_float` (178, 🚾) — added: parse a field directly to float.
- `field_add_to` (188, 🌰) — added: dict[field] += amount without
  allocating the key.
- `add_to` (187, 🤸) — added: dict[key] += amount, missing keys start at 0.
- `parse_int` (202, 🛍) — added: strtoll base-10 string→int.
- `parse_float` (203, 🥀) — added: strtod string→float.
- `format_int` (204, 🌴) — added: int→decimal string.
- `format_float` (205, 😮) — added: float→decimal string.
- `remove` (152, 🌦) — added: dict key removal via tombstone.
- `entry` (206, 🛎) — added: marks the program entry point (implicit jump
  from pc 0).
- `jmp`/`jz`/`je` — removed (v10.1): structured control flow replaces raw
  jumps; any use is a compile error.

## v10 — 2026-08-06 (a201978, modular compiler)

Arithmetic & logic:
- `div` (104, 😕) — added: division, int truncates toward zero, b=0 dies.
- `rem` (105, 🚚) — added: C remainder, sign follows the dividend.
- `eq` (106, 🤤) — added: loose equality coercing per universal rules.
- `lt` (107, 🌙) / `gt` (108, 😖) — added: numeric or lexicographic compare.
- `not` (109, 🚛) — added: 1 when the value is falsy.
- `or` (110, 🤥) / `xor` (111, 🌚) — added: integer-only bit ops that die
  on floats/pointers.
- `bnot` (113, 🚜) — added: integer-only bitwise complement.

Structured control flow (compiler-resolved):
- `if` (114, 🤦) — added: call a body label when the condition is truthy.
- `if_else` (115, 🌛) — added: transfer control to one of two body labels.
- `while` (116, 😘) — added: loop calling cond/body labels until cond is 0.
- `break` (117, 🚢) / `continue` (118, 🤧) — added: loop exit/next
  iteration, compile errors outside loops.

Container protocol & sequences:
- `get_or_zero` (119, 🌜) — added: container get that returns 0 on absence.
- `contains` (120, 😙) — added: membership test across containers.
- `orelse` (121, 🚣) — added: first value if truthy else the second.
- `keys` (122, 🤨) — added: dict keys or obj field names.
- `range` (123, 🌝) — added: list of ints in [start, stop).
- `sort` (124, 😚) — added: stable Timsort of lists and arrays.
- `filter` (125, 🚦) — added: keep elements whose predicate is truthy.
- `any` (126, 🤩) / `all` (127, 🌞) — added: short-circuit predicate
  existential/universal over a list.
- `append` (63, 🌌) — renamed from v0 `APPEND` to `PUSH`-style list append
  returning the possibly-realloced handle.
- `group_by` (164, 🌩) — added: group a list into a dict of lists by key
  function.
- `aggregate` (165, 😤) — added: map each group's value-list through a
  function.
- `unique` (166, 🚶) — added: O(n) first-occurrence dedup via dict.
- `flatten` (167, 🤳) — added: flatten one level of nesting.
- `chunk` (168, 🌪) — added: split a sequence into fixed-size pieces.

Vector ops (typed arrays/tensors):
- `scalar_add`/`scalar_sub`/`scalar_mul`/`scalar_div` (128–131) — added:
  broadcast a scalar across an array.
- `array_add`/`array_sub`/`array_mul`/`array_div` (132–135) — added:
  elementwise array arithmetic, shape-checked.
- `array_max` (136, 😝) / `array_min` (201, 😭) — added: elementwise
  min/max blend.
- `scalar_eq`/`scalar_lt`/`scalar_gt`/`scalar_gte`/`scalar_lte` (137–141) —
  added: array-vs-scalar comparisons producing bitmaps.
- `bitmap_and`/`bitmap_or`/`bitmap_not`/`bitmap_count` (142–145) — added:
  bitwise set operations over dense u64 bitmaps.
- `array_gather` (146, 🤮) — added: keep elements whose bitmap bit is set.
- `sum` (147, 🌥) / `mean` (148, 😠) / `min` (149, 🚬) / `max` (150, 🤯) —
  added: array reductions (empty → 0 for sum, dies for the others).
- `array_map` (153, 😡) — added: elementwise label application.
- `array_reduce` (154, 🚲) — added: generic label-fold with an initializer.
- `array_argsort` (169, 😥) — added: stable indices that would sort.
- `array_search_sorted` (170, 🚹) — added: binary-search insertion point.
- `array_where` (171, 🤴) — added: bitmap-selected blend of two arrays.

Time, bloom, script I/O:
- `now` (155, 🤰) — added: CLOCK_REALTIME nanoseconds.
- `parse_time` (156, 🌧) / `format_time` (157, 😢) — added: strptime/
  strftime (or "unix") time conversion.
- `bloom` (158, 🚴) / `bloom_add` (159, 🤱) / `bloom_test` (160, 🌨) —
  added: double-hashed FNV-1a bloom filter.
- `read_file` (161, 😣) — added: whole-file read, dies when missing.
- `write_file` (162, 🚵) — added: create/truncate write, dies on error.
- `argv` (163, 🤲) — added: program argv as a list of strings.

Large-data & graph ops:
- `mmap` (172, 🌫) — added: read-only zero-copy string, GC-unmapped on
  sweep.
- `file_each_line` (173, 😦) — added: call a label per line, stop early on
  a falsy return.
- `file_fold_lines` (174, 🚼) — added: streaming reduce over file lines.
- `file_match_lines` (181, 😨) — added: spawn a producer streaming
  regex-matching lines into a chan.
- `bfs` (182, 🚿) / `dfs` (183, 🤷) — added: graph traversal in
  visit order via a neighbors label.
- `find_first` (184, 🌯) — added: BFS with early exit returning the first
  match.

JSON, iterators, containment, threads:
- `parse_json` (190, 🛁) / `to_json` (191, 🤹) — added: JSON text ⇄ values.
- `iter` (192, 🌱) — added: single-use mutable cursor over any collection.
- `next` (193, 😫) — added: `[value, more]` step of an iterator.
- `collect` (194, 🛋) — added: drain an iterator into a fresh list.
- `iter_map` (195, 🤽) / `iter_filter` (196, 🌲) — added: lazy iterator
  transform/selection.
- `file_emit` (197, 😬) — added: stream any iterable to a file one item per
  line.
- `try` (198, 🛌) / `retry` (199, 🤾) — added: error containment returning
  `[result, ok]`, with a retry count.
- `spawn` (200, 🌳) — added: run a label on a detached thread, returning a
  cap-1 chan whose dequeue joins.

Strings & regex:
- `regex_match` (90, 🤠) — renamed from v0 `RX`: embedded-backtracking
  first match returning group strings.
- `regex_replace` (91, 🌓) — renamed from v0 `RXSUB`: replace all with
  `\1`..`\9` backrefs.
- `regex_split` (92, 😒) — renamed from v0 `RXSPLIT`: split between matches,
  empty matches skipped.
- `glob_match` (93, 🚗) — added: fnmatch-style pattern test.

## v0 — 2026-08-06 initial commit (9c0915f, single-file compiler)

Core stack, memory, and I/O:
- `_lit` (0, 🌀) — added: compile-time literal (number or type id).
- `add`/`sub`/`mul` (6–8) — added: coercing arithmetic.
- `and` (9, 😂) — added: coercing bitwise and.
- `shr` (10, 🚃) — added: arithmetic shift right by one.
- `inc` (11, 🤑) / `dec` (12, 🌃) — added: coercing ±1.
- `for` (16, 😃) — added: counted loop pushing the index per iteration.
- `_call` (17, 🚄) — added: call a label.
- `ret` (18, 🤒) — added: return from a call.
- `_obj` (19, 🌄) — added: allocate a struct instance.
- `get` (20, 😄) / `set` (21, 🚅) — added: polymorphic container access.
- `array` (23, 🤓) — added: typed array allocation.
- `copy` (26, 🌅) — added: deep copy.
- `_cast` (27, 😅) — added: checked struct downcast (extended to scalars in
  v14.1).
- `macro` (28, 🚆) — added: token macro definition.
- `tensor` (29, 🤔) — added: 64-aligned tensor allocation.
- `setv` (33, 🌆) / `getv` (34, 😆) — added: variable store/fetch (globals
  then; shared atomics in v14).
- `_str` (35, 🚇) — added: string literal push.
- `concat` (36, 🤕) — added: tag-dispatched concatenation.
- `format` (37, 🌇) — added: printf-style string builder.
- `buffer` (38, 😇) — added: raw untracked buffer.
- `copy_memory` (40, 🚉) — added: raw memory copy.
- `_addr` (41, 🤖) — added: push a label's code address (`'label`).
- `load` (42, 🌈) / `store` (43, 😈) — added: raw memory read/write.
- `_size_of` (44, 🚊) — added: compile-time type size.
- `_offset` (45, 🤗) — added: compile-time Struct.field offset.
- `struct` (46, 🌉) — added: struct layout directive.
- `malloc` (47, 😉) / `free` (48, 🚌) — added: raw untracked allocation.
- `_syscall` (49, 🤘) — added: syscall by number.
- `gc` (50, 🌊) — added: force a full mark-sweep collection.
- `import` (51, 😊) — added: unprototyped C function binding.
- `export` (52, 🚍) — added: export a label to a C symbol.
- `extern` (53, 🤙) — added: global C symbol address.
- `print` (54, 🌋) — added: type-aware recursive print.
- `scan` (55, 😋) — added: fscanf-style stdin parse.

Containers & channels:
- `dict` (56, 🚐) — added: open-addressing FNV-1a hash map.
- `list` (62, 🤚) — added: growable cell vector.
- `pop` (64, 😌) — added: pop the last list element.
- `channel` (65, 🚑) — added: bounded blocking MPSC ring.
- `enqueue` (66, 🤛) / `dequeue` (67, 🌍) — added: channel send/receive.
- `close` (68, 😍) — added: close a channel.
- `atomic` (69, 🚒) — added: atomic i64 cell.
- `atomic_get` (70, 🤜) / `atomic_set` (71, 🌎) — added: atomic load/store.
- `atomic_add` (72, 😎) — added: atomic fetch-add.
- `cas` (73, 🚓) — added: compare-and-swap.
- `type_of` (74, 🤝) — added: runtime tag.
- `length` (75, 🌏) — added: generalized length.

Modules & directives:
- `use` (78, 😏) — added: link a module and load its binding manifest.
- `mod` (79, 🚔) — added: translation-unit name.
- `pub` (80, 🤞) — added: export a label across translation units.
- `weave` (81, 🌐) / `task` (82, 😐) / `run` (84, 🤟) — added: static task
  DAG construct with fanout workers.

Shell:
- `shell` (85, 🌑) — added: /bin/sh -c returning `[out, err, status]`.
- `shell_stream` (88, 😑) — added: stream a command's stdout lines into a
  chan.
- `execute` (89, 🚖) — added: exec without a shell from an argv list.

Strings:
- `split` (94, 🤡) — added: literal-separator split (destructive).
- `join` (95, 🌔) — added: list to string with a separator.
- `slice` (96, 😓) — added: Python-semantics slice.
- `find` (97, 🚘) — added: first substring index, −1 on miss.
- `replace_all` (98, 🤢) — added: literal replace all.
- `trim` (99, 🌕) — added: strip whitespace both ends.
- `uppercase` (100, 😔) / `lowercase` (101, 🚙) — added: ASCII case fold.
- `starts_with` (102, 🤣) / `ends_with` (103, 🌘) — added: affix tests.

Retired at v10 (removed in a201978):
- `SEND`, `IDX`, `SETI`, `VEC`, `PIN`, `UNPIN`, `BUFPTR`, `DGET`, `DPUT`,
  `DDEL`, `DCOUNT`, `DKEYS`, `FIELDS`, `METHOD`, `SHX`, `RX`, `RXSUB`,
  `RXSPLIT`, and the v0 `APPEND` — replaced by the uniform container
  protocol, the v10 string/regex set, and structured concurrency.

Retired at v10.1 (removed in 854b63a):
- `jmp`, `jz`, `je` — structured control flow (`if`/`if_else`/`while`/`for`)
  replaces raw jumps.

Retired at v12 (915f9a7, slots now `~1`–`~5`):
- `dup`, `ovr`, `drop`, `swp`, `pick` — named variables replace stack
  shuffling.

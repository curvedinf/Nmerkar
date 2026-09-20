# examples/gnu — Nmerkar code transpiled from multi-file GNU tools

Each `.n` file here is machine-generated Nmerkar text-encoding source,
produced by the `trans` C→Nmerkar transpiler (`trans/`) from the
multi-file C adaptations in `trans/tests/gnu/<tool>/`. They are checked in
so the generated code is browsable and indexable.

| File | Source | Gates |
|---|---|---|
| `cat.n` | `trans/tests/gnu/cat/*.c` | 16 flag combos vs GNU cat (-n -b -E -T -A -v -s and combinations) |
| `head.n` | `trans/tests/gnu/head/*.c` | 11 gates vs GNU head (-n/-c with b/K/M/G suffixes, old-style -N, -q/-v headers) |
| `nl.n` | `trans/tests/gnu/nl/*.c` | 10 gates vs GNU nl (-b a/t/n, -n ln/rn/rz, -w/-v/-i/-s) |
| `tee.n` | `trans/tests/gnu/tee/*.c` | stdout+multi-file fan-out, -a append |
| `cksum.n` | `trans/tests/gnu/cksum/*.c` | POSIX CRC-32 matches GNU cksum on text/binary/large files |
| `base64.n` | `trans/tests/gnu/base64/*.c` | encode/decode with -w wrapping, round-trips |

Regenerate: `bash trans/tests/gnu/run.sh` (re-transpiles, re-gates, and
re-exports every `.n` here). Read a file top-down: import preamble,
`entry:` (the C `main`), function labels, then control-flow/k-call
labels. The emission model is documented in `trans/README.md`.

# Emitted-C nondeterminism (known, unfixed)

Finding (2026-10-12, parse-refactor verification): `nk --emit-c` output is not
byte-stable run-to-run even with the same compiler binary. Two sources, both
HashMap iteration order:

- `uf_init_locals` assignment order in the emitted main prologue,
- emitted function-block order (labels).

Observed varying on: examples/gnu/{nl,base64}, comp/tests/{t09,t17} — 2-line
diffs between consecutive runs of the same binary. Behavior is unaffected;
only the emitted C text ordering varies. The 78-gate parity suite is
insensitive to it (it compares program rc/stdout/stderr, not emitted C).

Impact: any future byte-identity check on `--emit-c` output (or a
reproducible-build claim) must first canonicalize these orderings (sort
init_locals assignments, sort function blocks) or fix the iteration order at
the source (ordered map / sorted keys in `comp/src/gen.rs`).

Revisit if: caching keyed on emitted-C hashes, reproducible-build
requirements, or C-level diff tooling is added.

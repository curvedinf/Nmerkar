# Pre-existing drift (verified 2026-10-14, present at HEAD before the string-arithmetic fix)

Not caused by the string-arithmetic change (verified by stashing the runtime
diffs and rerunning); left unresolved for the owner:

1. `bash trans/tests/gnu/run.sh`: `cat` fails 1/16 gates, `base64` fails
   6/6 (native segfault in the `-d` roundtrip). The run also re-exports
   `examples/gnu/*.n`, which are stale w.r.t. the current transpiler (v15.1
   `consume` emission, no `pt!`/`frv!` traffic) — the committed exports were
   restored rather than absorbed into an unrelated commit.
2. SPEC "Numeric context" table says `str ""`/whitespace → `0`, but both
   targets coerce an empty string to NaN (`"" 5 add` → NaN; parity hides it
   since both agree). Adjacent to the string-arithmetic fix but a separate
   decision: change the runtimes or amend the table.

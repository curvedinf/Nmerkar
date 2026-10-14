# Scratch index

- [Dev Skills installation](dev-skills-install.md): source revision, destination
  mapping, adaptations, conflicts; self-improvement enabled.
- [Emitted-C nondeterminism](codegen-nondeterminism.md): `--emit-c` output
  varies run-to-run (HashMap-ordered init_locals/function blocks); known,
  behavior-neutral, fix before any emitted-C hashing or reproducible builds.
- [Pre-existing drift](preexisting-drift.md): gnu gate failures (cat/base64)
  + stale `examples/gnu` exports, and `""`→NaN vs SPEC's `""`→0 coercion row.
- [Security disclosure policy](security-policy.md): standing permissions for
  the pre-commit security gate (currently none approved — every finding needs
  an explicit disposition).
- [timers.json](timers.json): shared time-triggered-skill timer state
  (maintain-scratch, 7-day interval).

Durable project working notes belong here and are included in Git. Organize
records by goal or topic and link them from this index as they are created.
Disposable files belong in the ignored `../tmp/` directory.

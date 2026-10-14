# Dev Skills installation record

- **Source:** https://github.com/curvedinf/dev-skills, revision `main`,
  commit `d1754529343d4c7bc8fb6de40565e80757f0dbaa` (updated 2026-10-14;
  prior install: `eec75da909fc954cd970cc3921859f9884a66a84`).
- **Scope:** project-wide (user option 1), destination
  `agents/skills/<passive|activated|triggered>/<name>/` at the Nmerkar root,
  branch `probe`.
- **License:** MIT (Copyright (c) 2026 Chase); copied to
  `agents/skills/LICENSE`.

## Destination mapping

- 20 skill folders (11 activated + 6 passive + 3 triggered) copied verbatim
  from source `skills/` to `agents/skills/` (SKILL.md, `agents/openai.yaml`
  adapters, `references/` where present). `agents/skills/` is byte-identical
  to upstream `skills/` except: upstream `skills/.gitkeep` not copied, local
  extra `LICENSE`.
- Source `AGENTS.md` guidance merged into the Nmerkar `AGENTS.md` "Dev Skills"
  section: category definitions, request-term routing table (links rewritten
  `skills/...` → `agents/skills/...`), authoring conventions, scratch policy,
  self-improvement/metaskill wiring, reference libraries. The source
  "Installation" and "Skill selection" sections were not imported verbatim —
  installation lives in this record, and selection is the routing table.
- Excluded: upstream `.git`, upstream `agents/scratch/` (source's own scratch
  history, timers, and security policy), `docs/skill-research.md`, upstream
  `README.md`.
- `agents/scratch/` Git-versioned with `index.md`; `/agents/tmp/` in the root
  `.gitignore`.

## Update 2026-10-12: f2f50ba → eec75da

- Added: activated `design-time-triggered-skill` (with
  `references/timer-contract.md`); new `triggered/` category holding
  `maintain-scratch`, `security-hygiene-check`, and `refine-skill` (moved
  from `passive/`, which no longer contains it).
- Modified: design-skill, install-dev-skills, polish-docs,
  documentation-style, git-discipline, scratch-space, refine-skill.
- AGENTS.md "Dev Skills" section only: pinned commit f2f50ba → eec75da;
  triggered category added to the intro sentence and path pattern; three
  routing-table rows added (security-hygiene-check, design-time-triggered-
  skill, maintain-scratch) at upstream's positions; refine-skill links
  repointed `passive/` → `triggered/` in the table, Authoring conventions,
  and Self-improvement. No other AGENTS.md sections touched;
  "Self-improvement: enabled." unchanged.
- Local divergence vs f2f50ba before this update was exactly one: the
  install-dev-skills step-1 fallback sentence from the first install (see
  Corrections applied). Upstream eec75da replaces that step with equivalent,
  stronger language ("Ask through an interactive question tool or a normal
  reply. **Stop here until the user answers.**"), so the local file was
  overwritten with upstream verbatim — the correction is resolved upstream,
  not preserved as a divergence. No other local files diverged.
- Deliberately not wired (task scope limited AGENTS.md to the pinned commit
  and routing table): upstream's triggered-skill host wiring — the
  fresh-session `maintain-scratch` check with `agents/scratch/timers.json`,
  and the pre-commit `security-hygiene-check` gate with
  `agents/scratch/security-policy.md`. Both require paragraphs in AGENTS.md
  sections outside "Dev Skills" (Scratch space / a new gate section). The
  routing table exposes both skills by request terms; adopting the wiring is
  a separate decision for the repo owner. `timers.json` was not created.

## Update 2026-10-14: eec75da → d175452

- Scope: project-wide (user: "reinstall dev-skills to this project" —
  explicit option 1). Upstream `main` resolved to
  `d1754529343d4c7bc8fb6de40565e80757f0dbaa` (2026-10-14).
- Upstream theme: trigger conditions move into triggered skills'
  `description` frontmatter (fire without reading the body); complex
  conditions use "encompassing initial conditions + ' and extra
  conditions.'" with the full list inside the skill.
- Updated from upstream (complete folders): design-skill,
  design-time-triggered-skill, maintain-scratch, refine-skill,
  security-hygiene-check (the last three gained full trigger-condition
  descriptions; refine-skill's Event section became an explicit condition
  list; the two authoring skills gained the description-state rule).
- Preserved local customization: coordinate-work keeps the local
  baseline-freeze sentence (see Conflicts, 2026-10-12 entry) — upstream
  d175452's copy is unchanged from eec75da, so no merge was needed;
  divergence remains exactly that one sentence. All other 14 skills are
  byte-identical to upstream.
- AGENTS.md "Dev Skills" section merged: pinned commit updated; the
  triggered-description rule added to the category definitions; authoring
  conventions gained the consolidation bullet, "obvious advice," and the
  behavioral-evaluation clause; Scratch space gained the fresh-session
  startup paragraph and the no-changelog retention rule; Self-improvement
  reworded to the coalesced per-activity pass (blocked/interrupted uses
  included; rereading is not a new event; `disabled.` disables edits, not
  the read requirement); Reference libraries gained the Hermes
  authoring-procedure link and the no-star-counts sentence. Routing table
  unchanged (same 20 rows). "Self-improvement: enabled." preserved (user's
  explicit first-install opt-in; upstream's declaration not inherited).
- Wiring completed this install (the 2026-10-12 install deferred it as a
  repo-owner decision; the current installer skill mandates it, and the
  user's reinstall request executes that skill): `agents/scratch/timers.json`
  initialized per the shared timer contract (maintain-scratch, 7-day
  default, null completion — due at the next fresh-session context load);
  `agents/scratch/security-policy.md` created as a strict baseline (no
  standing disclosures approved; every finding requires explicit
  disposition). No upstream disclosure permissions imported.
- Verification: 20 skills, frontmatter intact (name/description/category
  match directories), all AGENTS.md routing links resolve, updated skills'
  cross-links resolve (`../../activated/design-time-triggered-skill/
  references/timer-contract.md`, `../../passive/scratch-space/SKILL.md`).
  Host does not auto-register `agents/skills/`; guidance loads via AGENTS.md
  in a fresh session — this session's context still shows the pre-update
  section.
- Upstream clone fetched to the ignored `agents/tmp/` and removed after
  copy.

## Adaptations

- Cross-skill relative links (`../../triggered/...`, `../../passive/...`,
  `../...`, `references/...`) resolve unchanged: the `<category>/<name>/`
  layout is preserved. All 20 frontmatter blocks checked (name, description,
  metadata.category matching the directory); activated adapters carry
  `policy.allow_implicit_invocation: false`, passive and triggered `true`.
- Installed skills are discovered via the explicit links in `AGENTS.md`; the
  host does not auto-register project `agents/skills/` as invokable skills.
  `AGENTS.md` changes load into a new session's context; the current session
  keeps the pre-update text until reload.

## Conflicts

- testing-doctrine's cadence/nightly/release assumptions vs. Nmerkar's
  "Testing" section (manual gates, no automated test runner): resolved by
  precedence — the repository's own sections win, as stated in the "Dev
  Skills" section intro.
- git-discipline's publication defaults vs. the standing rule that
  Git-mutating commands require explicit user authorization: user/harness
  policy wins; no edit made. eec75da adds a pre-commit security-hygiene-check
  step inside git-discipline; it is subject to the same precedence.
- Upstream declares `Self-improvement: enabled.`; not inherited. Nmerkar's
  declaration came from the user's explicit opt-in during the first install
  and is unchanged by updates.
- Local refinement (2026-10-12, task-end self-improvement): coordinate-work
  step 3 gained one sentence — identity-verification baselines must come from
  a frozen source snapshot under concurrent edits. Evidence: a byte-identity
  check was contaminated by a parallel contributor's edit to a shared
  artifact during SPEC/compiler work. Reconcile on the next upstream sync.

## Corrections applied

- First install added a fallback to install-dev-skills step 1 ("If the host
  blocks the question tool, ask in plain text and end the turn while
  waiting.") after a user correction under auto-permission mode. Superseded
  upstream in eec75da ("Required opening: ask, then wait"); the local copy
  now matches upstream verbatim. Reinstall merges should still compare local
  files against the pinned revision before overwriting, per install-dev-skills
  step 2.

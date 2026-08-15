# PLAN — code_health: the deterministic code-health gate

Phases in delivery order; each ships the app working at its gate. Phases
never depend on a later phase.

## Phase 1 — CodeScene-lite core

- **Inputs:** repo path; radon; code-review-graph SQLite; git log.
- **Outputs:** a prioritized action list: complexity, large functions,
  hub files, hotspots (volatile functions named, with per-function churn),
  graph-risk (callers resolved).
- **Operations:** radon complexity pass, graph queries, git churn
  (`git log --name-only` + `git log -L` per volatile function).
- **Quality gate:** exit 1 when actions exist; `GATE: FAIL/PASS/
  INFORMATIONAL` verdict; priority = percentile of churn × complexity ×
  fan-in; per-file grouping; JSON with full metadata.

## Phase 2 — Requirements-framed guidance and honesty

- **Inputs:** Phase 1 actions, coverage data, repo standards.
- **Outputs:** anti-gaming guidance (data table vs methods, composition
  roots, service-layer extraction), real coverage verdicts with staleness
  handling, contracts to pin, extended test-file hints.
- **Quality gate:** blind evaluator (developer-on-houses) accepts the
  report as a workable fix list; coverage field never contradicts the
  prose; stale snapshots say "verify".

## Phase 3 — Baselines and diff awareness

- **Inputs:** Phase 2 report, git branch, baseline file.
- **Outputs:** `--baseline`/`--update-baseline` locking; in-diff marking;
  "no baseline — cannot tell what is new" honesty; lifecycle facts for
  low-churn scripts.
- **Quality gate:** a repo can lock today's debt and go green on NEW
  actions only; the gate's own repo demonstrates it.

## Phase 4 — Deterministic standards enforcement

- **Inputs:** repo source, coding/testing standards (scaffolded).
- **Outputs:** `record-shape` (records are objects; maps named by meaning;
  boundaries ingest), `latent-class` (closures, field partitions,
  strewing), `vague-name` (role suffixes hiding load), `standard` (inline
  imports, private imports, globals/module-mutable state, bare/empty/
  log-only excepts, `type: ignore` with a why, tuple aliases, env-skipif),
  `over-abstraction` (ABC with one concrete), `class-module` (one class
  per module, named after it), `docs` (links resolve, docs reachable from
  AGENTS.md).
- **Quality gate:** the tool passes its own gate (self-run GATE: PASS,
  max CC < 15, record check clean); lint-style exemptions
  (`# code-health: ignore <signal> <why>`) tested thoroughly; suppression
  without a why is itself a finding.

## Phase 5 — Deployment readiness

- **Inputs:** Phase 1-4 tool, this repo's docs.
- **Outputs:** house doc set (AGENTS.md, PRD, TECHSPEC, PLAN), CI
  workflow delegating to `make test` + `make self-check`, single-source
  tool copies (consuming repos fetch check_review_posted from a pinned
  SHA, not a local duplicate).
- **Quality gate:** `make test` and `make self-check` green in CI on
  push/PR; the docs kind passes on this repo's own docs; no duplicated
  tool copies anywhere.

## Phase 6 — Structural splitting detection and filesystem-test hygiene (delivered 2026-08-14)

- **Inputs:** Phase 4 tool, graph communities, file-level call graph,
  test files.
- **Outputs:** `folder-mix` (a folder whose direct files split across
  graph communities is a grab bag — extract a sub-folder per community;
  validated on chat-workflow: chat_workflow/ splits into chat-workflow-log
  and chat-workflow-error), `layer-mix` (a file whose functions partition
  by dominant callee subsystem — houses' server.py and api_router.py fire),
  and `fakefs` (tests touching the real filesystem without pyfakefs are
  findings, with the standard's real-FS exceptions and file-scoped
  explained exemptions).
- **Quality gate:** self-run stays green with only genuinely-reasoned
  exemptions (sqlite3 C-level I/O for the tool's own graph fixtures;
  test_check_records migrated to pyfakefs instead of exempting); 135
  tests green; CI runs make test + make self-check.
## Phase 7 — Warn tier: noisy-but-useful signals (delivered 2026-08-14)

- **Decision:** a "warn" severity — reported, never fails — unlocks the
  checks whose false-positive rate would block a hard gate.
- **Outputs:** `magic-number` (raw int/float operands, indices, and call
  args outside (0, 1, 2, -1); all-literal containers pass), `duplicate`
  (functions ≥ 90% structurally similar by Dice on skeleton bigrams —
  names/constants/args collapse, so copy-paste with renames matches;
  length-bucketed pairs; one finding per function), `unused` (module-level
  functions never referenced by name, import alias, or string literal —
  CLI dispatch and `main` pass), and `broad-except` (non-empty
  `except Exception`/`BaseException` — empty/bare stays fail-tier).
- **Severity plumbing:** `LatentFinding.severity` defaults fail; gate
  counts fails only; warns render `[warn]`-tagged with a "never-fail"
  verdict note; `--update-baseline` excludes warns (nothing noisy needs
  acknowledging); a warn merged into a fail target stays fail, and merged
  distinct messages are preserved as notes.
- **Self-check:** the tool's own run is GATE: PASS (17 warnings reported);
  max CC 14 (the two new repo-wide builders decomposed under the tool's own
  CC gate); 151 tests green.
- **Validation:** houses gate unchanged — 541 fail actions, +235 warnings
  never-fail; the tool's own near-duplicate scan finds the genuine
  copy-paste clusters in houses.

## Phase 7a — Eval round 2: precision fixes (delivered 2026-08-14)

A blind developer-eval of the houses report scored usability 6/10,
accuracy 6.5/10 and named four systematic false-positive classes. All
fixed with detector-semantics changes:
- Decorated module-level functions are referenced (routes, middleware —
  kills ~20 "dead code" warnings on live FastAPI endpoints).
- References split prod vs test: a function used only by tests is
  flagged conditionally (test seam, document it — or dead code) instead
  of silently treated as live or dead; 7 such findings on houses.
- Near-duplicate skips single-statement bodies (accessors, stubs,
  delegation wrappers) — genuine clusters like `_infeasible_commute`
  still fire.
- Swallow requires no control-flow exit: explicit returns (even None /
  empty literals) and continue are surfaced contracts; only bare, empty,
  log-only handlers fail. The eval's 4 of 6 mislabels are gone.
- Global-state covers typed AnnAssign literals and module collections
  mutated inside functions (`_oauth_states` now caught at auth.py:35).
- Hub-file counts exclude CALLS to true builtins (print/len/isinstance
  are not coupling).
- Record-shape held: the eval's "these dicts are fine" cases verified —
  the session payload (`dict[str, Any]` read by string key), the
  per-call threshold dict, and API bodies are all genuine records under
  the rule's own stated exceptions.
- Validation: 162 tests green; self-check ok (new code decomposed under
  its own CC gate); houses fails 541→537 (mislabels + builtin edges),
  warns 235→165.

## Phase 7b — Eval round 3: precision fixes + roll-up (delivered 2026-08-14)

Eval round 3: usability 6.5, accuracy 8.0 (up from 6.0/6.5); record-shape
messages credited as well-reasoned. Its five new verified findings, all
fixed, plus the deferred category roll-up:
- Swallow surfaces via accumulator: a handler that stores into or mutates
  a name the enclosing function returns (`issues.append(...)` in
  validate_payload) rides the error out in the result — the
  drive_isochrone.py:605 false FAIL is gone.
- Dict spread merges (`{**session, "is_superuser": live}`) are updates,
  not record construction — record_literal_lines skips spread keys
  (None-key on 3.14, DictUnpack on 3.5-3.13).
- Negative literals are constants: `_all_constant`/`_is_constant_value`
  unwrap UnaryOp, so DEFAULT_BBOX-style all-literal tables pass the
  lookup-table carve-out.
- `__init__` excluded from the near-duplicate scan (~30 DAG-boilerplate
  matches gone).
- No-op statements: expression statements that discard their value are
  dead-statement findings — catches the eval's server.py:220/269 miss
  (a ternary as a bare line, surely meant as an assignment).
- Text reports open with a per-kind roll-up (`by kind — fails:
  record-shape=260, ...; warnings: ...`) — the usability ask.
- Validation: 171 tests green; self-check ok (_all_constant decomposed
  under its own CC gate); houses fails 537→535, warns 165→158.

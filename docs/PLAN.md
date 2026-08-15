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

## Phase 6 — Structural splitting detection (in development)

- **Inputs:** Phase 4 tool, graph communities, file-level call graph.
- **Outputs:** `folder-mix` (a folder whose direct files split across
  graph communities is a grab bag — extract a sub-folder per community)
  and `layer-mix` (a file whose functions partition by dominant callee
  subsystem — the call graph is the seam; extract a module per layer).
- **Quality gate:** houses shows real folder/layer splits; self-run stays
  green; false-positive cases (organized packages with subdirectories,
  files with unresolved callees) excluded by the gates.
# PRD — code_health: the deterministic code-health gate

A deterministic, gate-shaped code-health scanner (CodeScene-lite) that turns
the maintainability requirements into a prioritized, actionable list of fix
actions — cheap to run, machine-readable, testable as a failing gate, and
honest about what it can and cannot know.

## Overarching goal

All code produced under these conventions stays readable, maintainable, and
anti-fragile — and the health of that code is measured deterministically,
cheaply, and honestly, with each finding carrying a concrete fix direction
rather than a number.

1. **Readability, maintainability, and anti-fragility are the requirements;
   metrics are proxies.** Low complexity, small functions, low coupling are
   symptoms of the goals — separation of concerns, domain language,
   effective encapsulation, code that is obviously correct — never the goals
   themselves. The report speaks in the requirements' language.
2. **Facts first, guidance second.** Every action states the evidence it
   actually has (locations, counts, churn, field access, callees) and offers
   a conditional fix; nothing is asserted the tool cannot know, and no
   interpretation (deletion, one-off-ness, class-vs-coincidence) is stated
   as fact.
3. **Cheap, scannable, self-honest.** The gate runs fast enough for
   per-PR use, the report is one line per action, the tool passes its own
   checks, and the report discloses its own limitations (stale data,
   blind spots, unresolved evidence).

## JTBD

1. When I am a developer whose PR failed CI, I want to know exactly which
   actions to fix and in what order, so I can clear the gate without
   re-deriving the seams myself.
2. When I am a maintainer, I want the repo's debt visible and ranked by
   change-cost, so refactoring effort lands where it pays.
3. When I add a coding-standard rule, I want it enforced deterministically
   where the rule has a checkable form, so compliance does not depend on an
   AI reviewer's judgment.
4. When I start fixing a finding, I want the message to tell me what to do
   in the requirements' terms — the seam, the domain noun, the boundary —
   not just what the number is.
5. When today's debt cannot be fixed now, I want to lock it in a baseline
   so the gate fails only on NEW actions.
6. When an automated check could tell me something, I want it to do so
   rather than leaving it to a reviewer's eye.

## Personas

- **Developer whose PR failed**: opens the report, expects a gate verdict,
  a prioritized per-file list, and per-action guidance they can act on
  without opening the flagged file first. Decision-useful: severity is
  binary (fix or baseline), priority ranks change-cost, coverage verdicts
  are trustworthy.
- **Maintainer**: reads the same report as backlog triage. Decision-useful:
  the action list is a punch list, baseline locks debt, diff awareness shows
  what a branch introduced.
- **Fixing agent**: an AI agent that will act on the messages. Decision-
  useful: messages name the seam (which subsystems a function mixes, which
  fields a group touches, which functions are the volatile part), give the
  contract to pin with tests, and resist metric-gaming.
- **The tool itself**: a member of the repos it scans. Decision-useful:
  the tool passes its own gate, and its own code is the exemplar of the
  patterns it prescribes.

## Constraints

- **Cost.** Token cost and wall time are budgeted: the report is one line
  per action with a second-line note; runtime stays seconds on a ~50k-LOC
  repo; findings are deduped per target.
- **Precision.** Facts only. A finding must be something the user should
  plausibly fix; where the tool cannot know (coverage staleness, grouping
  intent, one-off scripts), it says so and offers conditional guidance —
  never a confident assertion on weak evidence.
- **No AI in the loop.** The gate is deterministic (AST, graph SQLite,
  git history, radon). AI review is a separate layer (PR-Agent / review
  loop), not part of this tool.
- **Baselines culture.** Acknowledged debt is lockable (like pyrefly/
  basedpyright baselines) so the gate can go green incrementally.
- **Longevity.** The tool passes its own checks (self-run, record gate,
  regression tests); its messages carry the house dialect so agents and
  humans converge on the same fixes.
- **Hosting.** build-tools repo; fetched by consuming workflows at run time
  (pin to a tag, never main). No runtime services.

## Requirements learned this session (2026-08-14)

- **R1 — The report is a punch list, not a metric dump.** Output a list of
  actions to address, each with kind, file, line, function, message, and a
  fail/acknowledge severity; the exit code is the gate (a test can fail on
  any action).
- **R2 — Metrics are proxies; messages speak requirements.** Guidance is
  written in the terms of readability, maintainability, anti-fragility:
  separation of concerns, domain language, effective encapsulation,
  "obviously correct, not correct by flailing". Fixing the number without
  clarifying the code is not the point (and the guidance says so).
- **R3 — Fix guidance is concrete and anti-gaming.** Not "reduce
  complexity": one decision per named method, data table + loop for
  repeated blocks, composition roots keep the assembly thin, service-layer
  extraction behind DI for endpoints. Guidance must resist mechanical
  splitting that clears the metric while worsening the code.
- **R4 — The volatile part is named, per case.** Hotspot findings name the
  exact functions with their own churn (`git log -L`), not a vague "the
  volatile part".
- **R5 — The seams are detected, not guessed.** Where the graph's CALLS
  edges show a function or file pulling from multiple subsystems, the
  message names them with example callees; the extract-class boundary is
  the subsystem boundary, and unresolved seams are marked explicitly with
  the actual callee list, never silent boilerplate.
- **R6 — Coverage verdicts are trustworthy.** Test status comes from the
  repo's own data (coverage.xml, else `.coverage`), a file absent from the
  snapshot is UNKNOWN not uncovered, a stale snapshot flips the verdict to
  "verify" (never "write the failing tests first" on stale data), and
  untested functions get the exact contract to pin (`name(params) -> ret`)
  plus the mirrored test file to extend.
- **R7 — Honesty about uncertainty is a feature.** Weak seam signals are
  labeled weak; caller counts distinguish resolved vs total; coverage
  provenance and freshness are disclosed; "if this is the situation, do
  that" conditional guidance is used on known errors instead of asserting
  intent.
- **R8 — Priority ranks change-cost.** Order by churn × complexity ×
  fan-in, normalized to P01–P99 percentiles, with the formula, norms, and
  caps documented in the report so the ranking is auditable.
- **R9 — Baseline locks today's debt.** `--baseline`/`--update-baseline`
  acknowledges current actions so the gate fails only on new ones; the
  report states exactly how many are acknowledged.
- **R10 — Diff awareness, honestly worded.** Actions in files the current
  branch touches are marked; the header says "actions in files your diff
  touches" and admits when no baseline means "cannot tell what is new".
- **R11 — Lifecycle facts, not advice.** Low-churn scripts/tools carry
  churn and last-touch dates as facts; whether to delete, leave, or
  refactor is the agent's call — the tool stays in its lane.
- **R12 — Gate verdict and provenance are explicit.** `GATE: FAIL/PASS/
  INFORMATIONAL`, thresholds, coverage source + mtime, tool location, and
  re-run command are in the report.
- **R13 — Records are objects; maps are named by meaning; boundaries
  ingest.** Bare dict/tuple collections in signatures and record-position
  literals are findings (check_records); a genuine map is named by what it
  MEANS (`CoverageLines`), never `SomethingDict` (a `*Dict` alias renames
  the smell); data crossing a boundary is ingested into a domain class at
  that boundary. Only constant lookup tables stay anonymous, at module
  scope.
- **R14 — Fat code carrying unextracted classes is detected.** Nested
  closures that capture state (a class in disguise) and field-disjoint
  method groups (the partition is the seam; connectors named) are findings,
  gated on size/complexity so they are plausible fix items, with conditional
  guidance that leaves coincidental grouping alone. A role-suffix class
  name (Controller, Handler, Store, Repository, Manager, Orchestrator,
  Utils, Info) is a finding only when it hides load (>= 120 lines or >= 6
  methods): a thin framework-role class that delegates is communicatively
  named, and the message says so while pointing at the domain noun that
  should carry the weight.
- **R15 — The tool passes itself.** Self-run is clean, the record gate
  passes on the tool's own code, and regression tests pin both; the tool's
  own code is the exemplar of its prescriptions.
- **R16 — Testable with fakes.** The tool's behavior is unit-tested with
  injected fakes (no real libraries, no real repos), so every rule change
  is verified deterministically.
- **R17 — A catch must fail fast.** Bare excepts, empty bodies, and
  log-only catches (no raise, no surfaced return) are findings — logging
  alone is not fail-fast. The only sanctioned swallow is an explicitly
  safe-to-ignore error, and it must be marked and explained.
- **R18 — Lint-style suppressions carry a why.** A finding is exempted by
  `# code-health: ignore <signal> <why>` on its line or the line above;
  a suppression without an explanation is itself a finding (the tool is
  only skipped when the reader knows why the tool is wrong). Only real
  comments count — marker text inside a string never suppresses.
- **R19 — More standard rules are enforced in code, not by review.** The
  checkable-form rules from coding-standards.md run deterministically:
  imports at module top (never in function bodies), no private-symbol
  imports, no `global` or module-level mutable state, `# type: ignore`
  needs a why (tokenize-read real comments), no vague-suffix class names
  hiding load, function strewing over a same-module record is a missed
  class, no ABC with a single concrete implementation (repo-wide, resolved
  through imports), each class in its own module named after the class,
  no type alias for a fixed tuple (a `Key = tuple[str, str]` erases which
  element is which — make a class with named fields, GeoPoint not
  LatLngPair), and no `@pytest.mark.skipif` on environment presence
  (tests fake dependencies; only E2E may skip).
- **R20 — Documentation standards are enforced too.** Every relative
  markdown link in the repo's docs resolves; backtick paths that look like
  real paths resolve; every doc in `docs/` is reachable from AGENTS.md
  through any number of links — several hops are the norm. AGENTS.md
  carries only content relevant to all agents and links group indexes; it
  never flat-lists the doc tree. Bare names like `coding-standards.md`
  are references, not paths, and are left alone.
- **R21 — Structural mixing is detected at folder and layer level.** A
  folder whose direct files split across graph communities mixes concerns —
  extract a sub-folder per community. A file whose functions partition by
  dominant callee subsystem mixes architecture layers — the call graph is
  the seam; extract a module per layer. Both are graph-based, gated on the
  evidence (community membership, resolved callees), and skipped without a
  graph.
- **R22 — Tests fake the filesystem.** Real-FS access in a test
  (tmp_path/open/Path) without pyfakefs is a finding; the `fs` fixture and
  fake_filesystem_unittest pass. Real FS is sanctioned only when the code
  under test needs real semantics — subprocess interop, symlinks, C-level
  I/O like sqlite3 — and `# code-health: ignore-file <signal> <why>`
  exempts a whole file with an explanation (a why-less ignore-file is
  itself a finding).
- **R23 — Cycles, dead code, and shadowed builtins are findings.** Import
  cycles between local modules (from the graph's IMPORTS_FROM edges,
  strongly-connected components) are always fixed by restructuring — the
  fix direction is to hoist the shared interface into its own module and
  have both sides depend on it, never to bodge with lazy imports.
  Statements after an unconditional return/raise/continue/break are dead
  code — deleted, not kept. Parameters and locals named after builtins
  (list, dict, id, input, …) are renamed: a shadowed builtin makes the
  code read wrong.
- **R24 — A warn tier exists for noisy-but-useful signals.** Magic
  numbers (raw int/float operands, indices, and call arguments outside a
  tiny allowlist — lookup tables pass), copy-paste near-duplicates
  (functions ≥ 90% structurally similar), unused module-level functions,
  and broad `except Exception`/`BaseException` handlers are reported but
  never fail the gate. Their severity is "warn": the signal is real but
  the false-positive rate is too high to block on (same-shaped endpoints
  match as duplicates; `mod.fn()` attribute calls and public API used by
  other repos look unused). Warns are excluded from `--update-baseline`
  — nothing noisy needs acknowledging to go green. Merging a warn into a
  fail target keeps the fail severity, and merged messages are preserved
  as notes rather than dropped.

## Non-goals

- Not a per-PR review bot — PR-Agent and the review loop cover change
  review; this tool measures repo health.
- Not a linter or type-checker — ruff, pyrefly, basedpyright already own
  that layer.
- No knowledge distribution / bus factor, no CodeHealth 1–10 scoring, no
  trend-over-time charts (explicitly deferred; the gate does not need them).
- Not a deletion recommender — lifecycle facts only.
- Not wired into CI by this repo — the gate is a snapshot tool with a
  baseline; wiring it into workflows is a deployment decision.

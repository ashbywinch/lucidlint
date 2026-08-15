# build-tools

Shared CI utilities used by the house repos' workflows. Each tool is
self-contained (stdlib only), reads its inputs from environment variables,
and is fetched by the consuming workflow at run time — e.g. the pr-agent
"AI Code Review" workflow fetches `check_review_posted.py` from the pinned
tag and runs it as its review-attribution gate.

## Tools

- `check_review_posted.py` — fail the PR when the AI review bot did not post
  a "PR Reviewer Guide" comment covering the head commit. Env: `SHA`,
  `GITHUB_REPOSITORY`, `PR_NUMBER`, `GITHUB_TOKEN`. Attribution: the comment
  body references the head SHA (incremental reviews), or the comment was
  created after the head commit landed (first-review case — regular pr-agent
  reviews never contain the SHA).

- `code_health.py` — deterministic code-health gate. Emits a list of actions to address
  (high cyclomatic complexity, oversized functions, dependency hubs, git
  hotspots, high graph-risk nodes) and exits 1 when any exist, so it works
  as a failing test gate. Every finding family computes in the Rust scan
  core (scanner/, built with `make scanner-check`); the graph families read
  a versioned export contract generated through the code-review-graph tool's
  own public API (never its SQLite schema or location), and `git log`
  supplies change frequency. Metrics are proxies — the requirement
  is code that is obviously correct and cheap to change (readability,
  maintainability, anti-fragility), so each action's message gives a fix
  guideline in those terms: separation of concerns, domain language,
  effective encapsulation. Where the graph's CALLS edges show a function or
  file pulling from >= 2 subsystems, the action names those subsystems (with
  example callees) — the seams to extract classes/modules along — and hotspot
  actions name the exact volatile functions with their own churn (`git log -L`).
  Coverage verdicts come from the repo's own data (coverage.xml, else
  .coverage line_bits, else the graph risk index) and untested functions get
  the contract to pin (`name(params) -> ret`). A `record-shape` kind flags
  bare dict/tuple
  collections as records — the fix is a small domain class; a genuine map is
  named by its meaning (CoverageLines, never SomethingDict), and data
  crossing a boundary is ingested into a domain class at that boundary. A
  `latent-class` kind detects fat functions/classes carrying unextracted
  classes inside them: nested closures that capture state (a class in
  disguise) and field-disjoint method groups (the partition is the seam;
  connectors are named). A `standard` kind enforces the checkable-form
  rules from coding-standards.md deterministically: top-level imports,
  no private-symbol imports, no `global`/module-level mutable state,
  catches that fail fast (logging alone is not fail-fast), `# type: ignore`
  with a why, vague-suffix class names that hide load, strewing over a
  same-module record, no ABC with a single concrete implementation, each
  class in its own module, no fixed-tuple type aliases (they erase which
  element is which), and no env-keyed `skipif` in tests. A `docs` kind
  checks that markdown links resolve and every doc is reachable from
  AGENTS.md (multi-hop is the norm — AGENTS.md links groups, never flat
  lists). `folder-mix` and `layer-mix` detect a folder or file whose parts
  split across graph communities / callee subsystems — the seams for
  splitting. Import cycles (the fix: hoist the shared interface), unreachable
  statements after unconditional returns, and builtin-shadowing params and
  locals are standard-family findings. Tests touching the real filesystem
  without pyfakefs are
  findings (fakefs), except subprocess/symlink/sqlite3 C-level I/O. A
  **warn tier** reports noisy-but-useful signals that never fail the gate
  (tagged `[warn]`, counted as "N warnings never-fail", excluded from the
  baseline): magic numbers (raw int/float operands, indices, and call
  args outside (0, 1, 2, -1) — lookup tables pass), copy-paste
  near-duplicates (functions ≥ 90% structurally similar, two+ body
  statements — one-line accessors are not copy-paste), unused
  module-level functions (never referenced, imported, or dispatched by
  string; decorated functions are registered; a function referenced
  only from tests is a conditional test-seam finding), and broad
  `except Exception`/`BaseException` handlers. A swallow finding
  requires a handler with no control-flow exit at all — an explicit
  return is the documented contract, and a handler that mutates a name
  the enclosing function returns (accumulator) surfaces the error.
  No-op statements (a ternary or arithmetic as a bare line — value
  discarded) are dead-statement findings. Text reports open with a
  per-kind roll-up of fail and warning counts.
  Lint-style exemptions: `# code-health: ignore <signal>
  <why>` on the line (or above) — a suppression without a why is itself a
  finding. Actions are grouped by file and
  ranked by priority (percentile of metric x churn x fan-in); a baseline file
  (`--baseline`, `--update-baseline`) locks acknowledged debt so the gate can
  go green incrementally — and it is a BOTH-DIRECTION lock, like the pyrefly
  gate: a stale entry (a finding the code no longer produces) fails the run
  with "run --update-baseline", so debt paid without shrinking the baseline
  is drift, never silent. `--base <ref>` marks actions in your branch's
  diff. Flags: `--repo`, `--max-complexity`
  (15), `--max-function-lines` (120), `--max-file-edges` (150),
  `--max-risk` (0.8), `--hotspot-top-frac` (0.1), `--hotspot-min-cc` (15),
  `--json`, `--warn` (informational, exit 0).

## Use in a workflow

```yaml
- name: Fail loud if no review covers the head commit
  env:
    SHA: ${{ github.event.pull_request.head.sha }}
    PR_NUMBER: ${{ github.event.pull_request.number }}
    GITHUB_TOKEN: ${{ secrets.GITHUB_TOKEN }}
  run: |
    curl -fsSL -o check_review_posted.py \
      https://raw.githubusercontent.com/ashbywinch/build-tools/v1/check_review_posted.py
    python3 check_review_posted.py
```

Pin the URL to a tag, never `main`, so a later change cannot silently alter
what CI runs.

## Tests

`make test` runs the pytest suite (`tests/test_code_health.py` plus the
LSP session tests) and the Rust unit suite (`cargo test`, via
scanner-check). The orchestrator tests drive the real binary through a
passthrough subprocess route with faked git, so they run without a real
repo; the finding logic itself is owned by the Rust suite. They cover the
priority/merge/baseline logic, the gate's exit codes, and rendering.
`code_health.py`
also runs on itself: `make code-health REPO=.` reports its own hotspots.

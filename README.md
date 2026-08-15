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

- `code_health.py` — CodeScene-lite. Emits a list of actions to address
  (high cyclomatic complexity, oversized functions, dependency hubs, git
  hotspots, high graph-risk nodes) and exits 1 when any exist, so it works
  as a failing test gate. Reads the code-review-graph SQLite at
  `<repo>/.code-review-graph/graph.db` (build with `code-review-graph build
  --repo <repo>`), radon for complexity (run via `uv run --with radon`),
  and `git log` for change frequency. Metrics are proxies — the requirement
  is code that is obviously correct and cheap to change (readability,
  maintainability, anti-fragility), so each action's message gives a fix
  guideline in those terms: separation of concerns, domain language,
  effective encapsulation. Where the graph's CALLS edges show a function or
  file pulling from >= 2 subsystems, the action names those subsystems (with
  example callees) — the seams to extract classes/modules along — and hotspot
  actions name the exact volatile functions with their own churn (`git log -L`).
  Coverage verdicts come from the repo's own data (coverage.xml, else
  .coverage line_bits, else the graph risk index) and untested functions get
  the contract to pin (`name(params) -> ret`). Actions are grouped by file and
  ranked by priority (percentile of metric x churn x fan-in); a baseline file
  (`--baseline`, `--update-baseline`) locks acknowledged debt so the gate can
  go green incrementally, and `--base <ref>` marks actions in your branch's
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

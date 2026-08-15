# build-tools

Shared CI utilities for the house repos, plus the deterministic
code-health gate (`code_health.py`) and its test suite. Consuming workflows
fetch a tool at run time, pinned to a tag — never `main`.

## Quick start

- `make test` — run the pytest suite and syntax-check every tool
- `make code-health REPO=<path>` — run the gate on a repo (default `..`)
- `make self-check` — the gate must pass on this repo itself (PR gate)

## Where things live

| Task | Route to |
|---|---|
| Why this repo exists, requirements | `docs/PRD.md` |
| How the tool is built | `docs/TECHSPEC.md` |
| Phased delivery and gates | `docs/PLAN.md` |
| What each tool does | `README.md` (below) |

## Tools

- `check_review_posted.py` — PR review-attribution gate (env: `SHA`,
  `GITHUB_REPOSITORY`, `PR_NUMBER`, `GITHUB_TOKEN`)
- `check_records.py` — record-vs-bare-dict gate (stdlib AST), fixture-tested
- `code_health.py` — the deterministic code-health gate: complexity, size,
  coupling, hotspots, risk, record shape, latent classes, vague names,
  coding-standard rules, docs integrity — actions with facts and fix
  guidance, exit 1 when any exist, baselines for acknowledged debt

## Rules

- Never commit to main; branch + PR.
- Every behavior change ships with a test — fakes only, no monkeypatch.
- The tool passes itself: `make self-check` is green before any PR.

# build-tools

Shared CI utilities for the house repos, plus the deterministic
code-health gate (`code_health.py`) and its test suite. Consuming workflows
fetch a tool at run time, pinned to a tag — never `main`.

## Quick start

- `make setup` — venv + deps + pre-commit/pre-push hooks
- `make test` — lint + typecheck gate, then the pytest suite
- `make check` — lint + typecheck only (what the pre-push hook and CI run)
- `make self-check` — the gate must pass on this repo itself (PR gate)
- `make code-health REPO=<path>` — run the gate on a repo (default `..`)
- `make coverage` — pytest with coverage report (XML for CI)

Toolchain: uv, ruff (E,F,I,UP,B,SIM,N, no ignores), pyrefly with a
both-direction baseline lock (`scripts/pyrefly-lock.py`; refresh with
`make typecheck-update-baseline`). Raw git hooks in `scripts/`, installed
by `make install-hooks` — re-run `make setup` after pulling updates.
`tests/fixtures/` is intentionally non-compliant test input: ruff and
pyrefly both exclude it (check_records skips it for the same reason).

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

## Repo self-checks

- Docs integrity (links resolve, every doc reachable from this file) is
  enforced by the tool itself: `make self-check` runs `code_health.py
  --repo .`, whose `docs` kind fails on broken links and unreachable
  docs. No separate docs-links test is needed — the tool IS the checker.
- The tools import only stdlib at module level (radon is an optional
  guarded import) — that invariant is what lets consuming repos run them
  with a bare `uv run --with radon`; a test guards it
  (`tests/test_tool_imports.py`).

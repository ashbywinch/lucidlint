# build-tools

Shared CI utilities for the house repos, plus the deterministic
lucidlint gate (`lucidlint.py`) and its test suite. Consuming workflows
fetch a tool at run time, pinned to a tag — never `main`.

## Quick start

- `make setup` — venv + deps + pre-commit/pre-push hooks
- `make test` — lint + typecheck gate, then the pytest suite
- `make check` — lint + typecheck only (what the pre-push hook and CI run)
- `make self-check` — the gate must pass on this repo itself (PR gate)
- `make lucidlint REPO=<path>` — run the gate on a repo (default `..`)
- `make coverage` — pytest with coverage report (XML for CI)

Toolchain: uv, ruff (E,F,I,UP,B,SIM,N, no ignores), pyrefly with a
both-direction baseline lock (`scripts/pyrefly-lock.py`; refresh with
`make typecheck-update-baseline`). Raw git hooks in `scripts/`, installed
by `make install-hooks` — re-run `make setup` after pulling updates.
`tests/fixtures/` is intentionally non-compliant test input: ruff and
pyrefly both exclude it (the fixtures are intentionally broken input).

## Where things live

| Task | Route to |
|---|---|
| Why this repo exists, requirements | `docs/PRD.md` |
| How the tool is built | `docs/TECHSPEC.md` |
| Phased delivery and gates | `docs/PLAN.md` |
| Coding standards (canonical + language conventions) | `docs/coding-standards.md` |
| Testing standards | `docs/testing-standards.md` |
| UX standards | `docs/ux-standards.md` |
| What good documentation is | `docs/writing-documentation.md` |
| Required doc set and folder structure | `docs/documentation-structure.md` |
| What each tool does | `README.md` (below) |

## Tools

- `check_review_posted.py` — PR review-attribution gate (env: `SHA`,
  `GITHUB_REPOSITORY`, `PR_NUMBER`, `GITHUB_TOKEN`)
- the Rust scan core (`scanner/`) — every finding family; the Python
  orchestrator converts + renders
- `lucidlint.py` — the deterministic lucidlint gate: complexity, size,
  coupling, hotspots, risk, record shape, latent classes, vague names,
  coding-standard rules, docs integrity — actions with facts and fix
  guidance, exit 1 when any exist, baselines for acknowledged debt

## Rules

- Never commit to main; branch + PR.
- Every behavior change ships with a test — fakes only, no monkeypatch.
- The tool passes itself: `make self-check` is green before any PR.

## Repo self-checks

- Docs integrity (links resolve, every doc reachable from this file) is
  enforced by the tool itself: `make self-check` runs `lucidlint.py
  --repo .`, whose `docs` kind fails on broken links and unreachable
  docs. No separate docs-links test is needed — the tool IS the checker.
- The tools import only stdlib at module level; the scan engine is the
  Rust binary (built by `make scanner-check`, required by the gate).

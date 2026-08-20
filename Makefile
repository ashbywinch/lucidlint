# Makefile for lucidlint — single dev entry point
#
# CHANGE: none expected — this repo is the tools themselves; add new tool
# targets below `clean` (keep them behind `deps`, never `install-hooks`).
#
# DO NOT CHANGE:
# - SHELL + .SHELLFLAGS (the shell pin): make's default /bin/sh is dash on
#   Debian/Ubuntu — including the CI runners — and dash has no `pipefail`.
# - `deps` never depends on `install-hooks`: a hook that calls `make check`
#   would re-copy/refuse the very hook file — the pre-push deadlock.
# - `check` = lint-check + typecheck, the single gate CI and pre-push both run.
# - `clean` never touches user data (only .venv + generated reports).

SHELL := /bin/bash
.SHELLFLAGS := -eu -o pipefail -c

.PHONY: help setup deps uv-sync install-hooks check lint lint-check lint-github typecheck typecheck-update-baseline format test scanner-check coverage self-check lucidlint wheel wheel-check clean

# Tool paths. uv is the package manager (installs itself if missing).
PYTHON := .venv/bin/python
UV := $(shell command -v uv 2>/dev/null || echo $(HOME)/.local/bin/uv)
RUFF := .venv/bin/ruff
PYTEST := .venv/bin/pytest

# Colors
GREEN := \033[0;32m
YELLOW := \033[1;33m
RED := \033[0;31m
NC := \033[0m

help:
	@echo "lucidlint — available commands:"
	@echo "  ${GREEN}make setup${NC}        Create venv, install deps + pre-commit hooks"
	@echo "  ${GREEN}make check${NC}        Lint + typecheck — the gate CI and the pre-push hook run"
	@echo "  ${GREEN}make test${NC}         Run tests (lint + typecheck gate)"
	@echo "  ${GREEN}make self-check${NC}   The tool's own gate: lucidlint on this repo + record check"
	@echo "  ${GREEN}make coverage${NC}     Run tests with coverage report"
	@echo "  ${GREEN}make lucidlint${NC}  Run lucidlint.py on REPO (default: ..)"
	@echo "  ${GREEN}make format${NC}       Auto-fix lint + formatting issues"
	@echo "  ${GREEN}make clean${NC}        Remove .venv and generated files"

setup: deps install-hooks
	@[ -f .env ] || cp .env.example .env
	@echo "${GREEN}✓ Setup complete${NC}"

# What the CHECK targets actually need — no hook machinery. A check must
# never depend on install-hooks (see the deadlock note at the top).
deps: uv-sync

uv-sync:
	@$(UV) --version >/dev/null 2>&1 || curl -LsSf https://astral.sh/uv/install.sh | sh
	@$(UV) sync --all-extras

install-hooks:
	@mkdir -p .git/hooks
	@for hook in pre-commit pre-push; do \
		if [ -f .git/hooks/$$hook ] && ! cmp -s scripts/$$hook .git/hooks/$$hook; then \
			echo "${YELLOW}Overwriting an existing $$hook hook (scripts/ is the single source of truth — re-run after pulling updates, as the hook headers say)${NC}"; \
		fi; \
		cp scripts/$$hook .git/hooks/$$hook; \
		chmod +x .git/hooks/$$hook; \
	done
	@echo "${GREEN}✓ Hooks installed (pre-commit: lint + gitleaks; pre-push: make check)${NC}"

# The exact gate a push must pass — run identically by CI and the pre-push
# hook (single source of truth; no test run — too slow for a hook, and CI's
# `make test` already includes these).
check: deps lint-check typecheck

# The Rust scan core must be freshly built before pytest — the orchestrator
# tests drive the real binary, and a stale binary would test nothing.
test-fixture:
	@$(PYTHON) scripts/build-test-repo.py

test: deps lint-check typecheck scanner-check scanner-test
	@$(PYTEST) tests/ -q --tb=short

# The Rust suite is part of the gate — a change to the core must pass its
# own tests, not just build.
scanner-test:
	@cd scanner && cargo test --release 2>&1 | tail -30
	@echo "${GREEN}✓ scanner tests passed${NC}"

scanner-check:
	@cd scanner && cargo build --release 2>&1 | tail -30
	@echo "${GREEN}✓ scanner built${NC}"

rules:
	@$(UV) run python scripts/gen-rules.py

# The pip distribution must be self-contained: `uv build` compiles the Rust
# core INTO the wheel (setup.py's build_py), so a clean-venv install scans
# and fixes with no bundle, no PATH, no make. dist/ must be emptied first:
# `uv build` never deletes previous artifacts, and wheel-check installs
# `dist/*.whl` — two wheels of different versions in dist/ collide with a
# "conflicting URLs" resolution error (0.1.0 + 0.2.0 both matched the glob).
wheel: scanner-check
	@rm -rf dist build *.egg-info
	@$(UV) build
# The deployment check: install the freshly built wheel into a CLEAN venv and
# exercise the installed package end to end on a mini project (what a real
# pip user gets). scripts/deploy-check.py runs scan -> fix -> re-scan PASS
# against the INSTALLED binary; each step depends on a different part of the
# wheel (the embedded Rust core, the shipped modules, the libcst dependency),
# so a packaging error fails loudly instead of silently passing.
wheel-check: wheel
	@tmp=$$(mktemp -d); \
	$(UV) venv $$tmp/venv >/dev/null; \
	$(UV) pip install --python $$tmp/venv dist/*.whl >/dev/null; \
	$$tmp/venv/bin/lucidlint --version | grep -q "^lucidlint"; \
	$(PYTHON) scripts/deploy-check.py --lucidlint $$tmp/venv/bin/lucidlint --project $$tmp/project; \
	rm -rf $$tmp; \
	echo "${GREEN}✓ pip wheel: clean-venv install scans + fixes a mini project${NC}"


coverage: deps
	@$(UV) run coverage run -m pytest tests/ -q --tb=short
	@$(UV) run coverage report -m
	@$(UV) run coverage xml
	@$(UV) run coverage html
	@cd scanner && cargo llvm-cov --lcov --output-path ../lcov.info 2>&1 | tail -30
	# the CI summary action reads Cobertura XML — lcov is for local tooling only
	@cd scanner && cargo llvm-cov --cobertura --output-path ../coverage-rust.xml 2>&1 | tail -5
	@echo "${GREEN}Coverage reports: htmlcov/index.html + lcov.info + coverage-rust.xml${NC}"

lint: deps lint-check

lint-check: deps  # Shared with the pre-commit hook — single source of truth for the lint scope
	@$(RUFF) check *.py tests/ scripts/
	@cd scanner && cargo fmt -- --check
	@cd scanner && cargo clippy --all-targets -- -D warnings

lint-github: deps   # CI only: findings surface as PR annotations
	@$(RUFF) check *.py tests/ scripts/ --output-format=github
	@cd scanner && cargo fmt -- --check
	@cd scanner && cargo clippy --all-targets --message-format json -- -D warnings

# pyrefly + BOTH-direction baseline lock (new errors AND stale entries fail);
# the Rust borrow checker (cargo check) is deterministic under the
# rust-toolchain.toml pin, so it needs no baseline.
typecheck: deps
	@$(PYTHON) scripts/pyrefly-lock.py check --pyrefly-config pyrefly.toml
	@cd scanner && cargo check --all-targets 2>&1 | tail -30

# After a deliberate diagnostic change, commit the refresh
typecheck-update-baseline: deps
	@$(PYTHON) scripts/pyrefly-lock.py update-baseline --pyrefly-config pyrefly.toml

format: setup
	@$(RUFF) check --fix *.py tests/ scripts/
	@$(RUFF) format *.py tests/ scripts/

# The repo's defining gate: the tool must pass on itself — every finding
# family (record-shape included) computes in the Rust core.
self-check:
	@$(PYTHON) lucidlint.py --repo .
	@echo "ok — the tool passes its own gate"

# Run the health tool on another repo (default: the parent directory).
lucidlint: deps scanner-check
	@$(PYTHON) lucidlint.py --repo $(REPO)

clean:
	@rm -rf .venv htmlcov/
	@rm -f .coverage coverage.xml
	@find . -type d -name "__pycache__" -exec rm -rf {} + 2>/dev/null || true
	@find . -type f -name "*.pyc" -delete

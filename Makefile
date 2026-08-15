# Makefile for build-tools — single dev entry point
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

.PHONY: help setup deps uv-sync install-hooks check lint lint-check lint-github typecheck typecheck-update-baseline format test scanner-check coverage self-check code-health clean

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
	@echo "build-tools — available commands:"
	@echo "  ${GREEN}make setup${NC}        Create venv, install deps + pre-commit hooks"
	@echo "  ${GREEN}make check${NC}        Lint + typecheck — the gate CI and the pre-push hook run"
	@echo "  ${GREEN}make test${NC}         Run tests (lint + typecheck gate)"
	@echo "  ${GREEN}make self-check${NC}   The tool's own gate: code_health on this repo + record check"
	@echo "  ${GREEN}make coverage${NC}     Run tests with coverage report"
	@echo "  ${GREEN}make code-health${NC}  Run code_health.py on REPO (default: ..)"
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
	@$(UV) sync

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

# The Rust scan core must be freshly built before pytest — the parity
# gate (tests/test_scanner_parity.py) diffs it against the Python
# implementation, and a stale binary would test nothing.
test: deps lint-check typecheck scanner-check
	@$(PYTEST) tests/ -q --tb=short

scanner-check:
	@cd scanner && cargo build --release 2>&1 | tail -1
	@echo "${GREEN}✓ scanner built (parity gate active)${NC}"

coverage: deps
	@$(UV) run coverage run -m pytest tests/ -q --tb=short
	@$(UV) run coverage report -m
	@$(UV) run coverage xml
	@$(UV) run coverage html
	@echo "${GREEN}Coverage report: htmlcov/index.html${NC}"

lint: deps lint-check

lint-check: deps  # Shared with the pre-commit hook — single source of truth for the lint scope
	@$(RUFF) check *.py tests/ scripts/

lint-github: deps   # CI only: findings surface as PR annotations
	@$(RUFF) check *.py tests/ scripts/ --output-format=github

# pyrefly + BOTH-direction baseline lock (new errors AND stale entries fail)
typecheck: deps
	@$(PYTHON) scripts/pyrefly-lock.py check --pyrefly-config pyrefly.toml

# After a deliberate diagnostic change, commit the refresh
typecheck-update-baseline: deps
	@$(PYTHON) scripts/pyrefly-lock.py update-baseline --pyrefly-config pyrefly.toml

format: setup
	@$(RUFF) check --fix *.py tests/ scripts/
	@$(RUFF) format *.py tests/ scripts/

# The repo's defining gate: the tool must pass on itself, and the record
# check must stay clean on the tools' own signatures.
self-check:
	@$(PYTHON) code_health.py --repo .
	@$(PYTHON) check_records.py code_health.py check_records.py
	@echo "ok — the tool passes its own gate"

# Run the health tool on another repo (default: the parent directory).
code-health: deps scanner-check
	@$(PYTHON) code_health.py --repo $(REPO)

clean:
	@rm -rf .venv htmlcov/
	@rm -f .coverage coverage.xml
	@find . -type d -name "__pycache__" -exec rm -rf {} + 2>/dev/null || true
	@find . -type f -name "*.pyc" -delete

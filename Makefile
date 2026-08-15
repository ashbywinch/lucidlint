# build-tools — shared CI utilities (single dev entry point per the house standard).
.PHONY: help test code-health self-check

REPO ?= ..

help:
	@echo "build-tools — available commands:"
	@echo "  ${GREEN}make test${NC}          Syntax-check every tool"
	@echo "  ${GREEN}make code-health${NC}   Run code_health.py on REPO (default: ..)"
	@echo "  ${GREEN}make self-check${NC}   The gate must pass on this repo itself"

test:
	uv run --with pytest --with pyfakefs python3 -m pytest tests -q
	@for f in *.py; do python3 -m py_compile $$f || exit 1; done
	@echo "ok — tests pass"

code-health:
	uv run --with radon python3 code_health.py --repo $(REPO)

self-check:
	uv run --with radon python3 code_health.py --repo .
	python3 check_records.py code_health.py check_records.py
	@echo "ok — the tool passes its own gate"

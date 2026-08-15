# build-tools — shared CI utilities (single dev entry point per the house standard).
.PHONY: help test code-health

REPO ?= ..

help:
	@echo "build-tools — available commands:"
	@echo "  ${GREEN}make test${NC}          Syntax-check every tool"
	@echo "  ${GREEN}make code-health${NC}   Run code_health.py on REPO (default: ..)"

test:
	uv run --with pytest python3 -m pytest tests -q
	@for f in *.py; do python3 -m py_compile $$f || exit 1; done
	@echo "ok — tests pass"

code-health:
	uv run --with radon python3 code_health.py --repo $(REPO)

# build-tools — shared CI utilities (single dev entry point per the house standard).
.PHONY: help test

help:
	@echo "build-tools — available commands:"
	@echo "  ${GREEN}make test${NC}          Syntax-check every tool"

test:
	@for f in *.py; do python3 -m py_compile $$f || exit 1; done
	@echo "ok — all tools compile"

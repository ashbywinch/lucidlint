# lucidscan — the code-health gate (release-bundle shim).
#
# This Makefile ships INSIDE the release bundle. A consumer repo copies or
# unpacks the bundle (e.g. to .tools/lucidscan/) and runs:
#
#   make -C .tools/lucidscan code-health REPO=../..
#
# The orchestrator finds its sibling binary (bin/lucidscan) by itself — no
# PATH or env needed. The baseline flow locks acknowledged debt:
#
#   make -C .tools/lucidscan code-health-baseline REPO=../..
#
# REPO is the repo to scan; PYTHON overrides the interpreter.

REPO ?= .
PYTHON ?= python3
BASELINE ?= code-health.json
HERE := $(dir $(abspath $(lastword $(MAKEFILE_LIST))))

.PHONY: code-health code-health-baseline version

code-health:
	$(PYTHON) $(HERE)code_health.py --repo $(REPO) $$([ -z "$(BASELINE)" ] || echo "--baseline $(BASELINE)")

code-health-baseline:
	$(PYTHON) $(HERE)code_health.py --repo $(REPO) --update-baseline --baseline $(BASELINE)

version:
	@cat $(HERE)version.txt

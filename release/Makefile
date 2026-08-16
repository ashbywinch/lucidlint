# lucidscan — the lucidlint gate (release-bundle shim).
#
# This Makefile ships INSIDE the release bundle. A consumer repo copies or
# unpacks the bundle (e.g. to .tools/lucidscan/) and runs:
#
#   make -C .tools/lucidscan lucidlint REPO=../..
#
# The orchestrator finds its sibling binary (bin/lucidscan) by itself — no
# PATH or env needed. The baseline flow locks acknowledged debt:
#
#   make -C .tools/lucidscan lucidlint-baseline REPO=../..
#
# REPO is the repo to scan; PYTHON overrides the interpreter.

REPO ?= .
PYTHON ?= python3
BASELINE ?= lucidlint.json
HERE := $(dir $(abspath $(lastword $(MAKEFILE_LIST))))

.PHONY: lucidlint lucidlint-baseline version

lucidlint:
	$(PYTHON) $(HERE)lucidlint.py --repo $(REPO) $$([ -z "$(BASELINE)" ] || echo "--baseline $(BASELINE)")

lucidlint-baseline:
	$(PYTHON) $(HERE)lucidlint.py --repo $(REPO) --update-baseline --baseline $(BASELINE)

version:
	@cat $(HERE)version.txt

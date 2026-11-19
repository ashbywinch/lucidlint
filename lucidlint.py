#!/usr/bin/env python3
# are the gate's configuration tables — a config class is the eventual home, not this round
# lucidlint: ignore-file global-state module-level constants (excluded dirs, action kinds, thresholds, fix aliases)
# walks — decisions are path branches, not branching logic
# lucidlint: ignore-file complexity the orchestrator's git functions are single-pass protocol

"""lucidlint.py — the deterministic lucidlint gate: a thin orchestrator
over the Rust scan core.

The finding engine is the Rust binary (scanner/): every family — per-file,
latent-class partition, the test-only rules, duplicate/unused, record-shape,
complexity, the graph families (via the versioned export contract),
hotspot, over-abstraction, docs — computes there. This orchestrator gathers
the inputs (git file list/history, the graph contract, churn, coverage),
converts the contract findings to Actions verbatim, and renders the gate:
baselines, dedupe/merge, priority, diff marking, exit codes.

    python3 lucidlint.py --repo /path/to/repo --json
    echo $?   # 1 when there is work to do

The scan thresholds live in the binary (schema 4): CC>=15, fn>=120 lines,
file>=150 edges, risk>=0.8, hotspot top 10% by churn with CC>=15.
Philosophy: the metrics are *proxies* for code that is obviously correct
and cheap to change; each message says what to do in those terms.
"""

from __future__ import annotations

import os
import subprocess
import sys

import rule_metadata
from orchestrator.action import Action
from orchestrator.baseline import Baseline
from orchestrator.cli import Cli
from orchestrator.console import CONSOLE
from orchestrator.fix.fix_command import FixCommand
from orchestrator.gate_runner import GateRunner
from orchestrator.lucidlint_config import LucidlintConfig
from orchestrator.report.render_ctx import RenderCtx
from orchestrator.rust_finding import RustFinding
from orchestrator.rust_findings import RustFindings
from orchestrator.rust_fix_request import RustFixRequest
from orchestrator.rust_scan import INPUTS, RustScan
from orchestrator.scan_flags import ScanFlags
from orchestrator.services import SERVICES, Services
from orchestrator.services.graph_contract import GraphContract
from orchestrator.sources.coverage_context import CoverageContext, coverage_context
from orchestrator.sources.coverage_input import CoverageInput
from orchestrator.sources.coverage_result import CoverageResult
from orchestrator.sources.file_history import FileHistory
from orchestrator.sources.git_head import GitHead
from orchestrator.sources.source_file import SourceFile
from orchestrator.sources.source_walk import SourceWalk, gitignored_docs
from orchestrator.vendoring import vendor_path
from orchestrator.version import VERSION as _VERSION
from orchestrator.version import version_from_bundle

__all__ = [
    "Action",
    "Baseline",
    "CONSOLE",
    "CONTRACT_VERSION",
    "CoverageContext",
    "CoverageInput",
    "CoverageResult",
    "FileHistory",
    "GitHead",
    "RULE_GROUPS",
    "RenderCtx",
    "RustFinding",
    "RustFindings",
    "RustFixRequest",
    "RustScan",
    "SERVICES",
    "ScanFlags",
    "SourceFile",
    "SourceWalk",
    "_ALL_RULE_KINDS",
    "_CoverageInput",
    "_FIX_ALIASES",
    "_FixCommand",
    "_GateRunner",
    "_GraphStore",
    "_LucidlintConfig",
    "_Registry",
    "_RenderCtx",
    "_RustScan",
    "_SCANNER_FAILURES",
    "_ScanFlags",
    "_Services",
    "_VERSION",
    "_actions_from_rust",
    "_apply_baseline",
    "_baseline_entry_is_current",
    "_baseline_file_state",
    "_coverage_context",
    "_excluded_part",
    "_facts_line",
    "_fix_directive_kind",
    "_fix_identifier_problem",
    "_fix_of",
    "_fix_refusal",
    "_gate_exit",
    "_gitignored_docs",
    "_iso_day",
    "_kind_counts",
    "_load_baseline",
    "_md_rels",
    "_merge_key",
    "_name_is_invalid",
    "_name_required_kinds",
    "_numbits_to_lines",
    "_position_key",
    "_print_trimmed_diff",
    "_pygit2",
    "fix_engine",
    "_raw_score",
    "_render_actions",
    "_render_file_group",
    "_rust_finding_rel",
    "_scan_input_dir",
    "_scanner_build_is_stale",
    "_scanner_candidates",
    "_seam_groups",
    "_stamp_of",
    "_temp_input",
    "_tracked_churn_path",
    "_trim_preview_diff",
    "_vendor_path",
    "_version_from_bundle",
    "action_key",
    "is_test_path",
    "log",
    "main",
    "parse_args",
    "release_temp_input",
    "subprocess",
]

# the entry module's own directory: the release bundle's root (deps/,
# version.txt) sits beside it
_here = os.path.dirname(os.path.abspath(__file__))
# Run as a script the module is `__main__`; the host seams resolve the entry
# module by its installed name, so make that name point here in both modes.
sys.modules.setdefault("lucidlint", sys.modules[__name__])

# the optional libraries and the process module are bound HERE because this is
# the surface a consumer or the test suite substitutes (`ch.subprocess = fake`,
# `ch._pygit2 = None`); the host seams read the current binding at call time
try:
    from code_review_graph.graph import GraphStore as _GraphStore
    from code_review_graph.registry import Registry as _Registry
except ImportError:  # lucidlint: ignore swallow code-review-graph is optional — degrades to non-graph families
    _GraphStore = None
    _Registry = None

try:
    import pygit2 as _pygit2
except ImportError:  # lucidlint: ignore swallow pygit2 is optional — degrades to gitless mode
    _pygit2 = None

# The release bundle self-contains the fix engine's libcst in a sibling `deps/`
# dir (review-log B8): when a `deps/` sits next to this file (the bundle, not a
# wheel install), prefer it so the fix path works with no pip step. A
# wheel/source install has no `deps/` and uses site-packages libcst.
_vendor = vendor_path(_here)
if _vendor is not None and _vendor not in sys.path:
    sys.path.insert(0, _vendor)
try:
    import fix_engine
    # lucidlint: ignore swallow optional extra — fix degrades to a clear error
except ImportError:
    fix_engine = None

# the entry module is the facade: these names stay here because callers (and
# the tests) reach them on `lucidlint`
_vendor_path = vendor_path
_version_from_bundle = version_from_bundle
log = CONSOLE.warn
_print_trimmed_diff = CONSOLE.print_diff
action_key = Action.key
_position_key = Action.position_key
_merge_key = Action.merge_key
_coverage_context = coverage_context
_CoverageInput = CoverageInput
_numbits_to_lines = CoverageResult.numbits_to_lines
_raw_score = FileHistory.raw_score
_iso_day = FileHistory.iso_day
_tracked_churn_path = FileHistory.tracked_churn_path
_baseline_entry_is_current = Baseline.entry_is_current
_baseline_file_state = Baseline.file_state
_load_baseline = Baseline
_apply_baseline = Baseline.apply
_excluded_part = SourceWalk.excluded_part
is_test_path = SourceWalk.is_test_path
_md_rels = SourceWalk.md_rels
_gitignored_docs = gitignored_docs
_RenderCtx = RenderCtx
_facts_line = RenderCtx._facts_line
_render_file_group = RenderCtx._render_file_group
_fix_of = RenderCtx._fix_of
_stamp_of = RenderCtx._stamp_of
_seam_groups = RenderCtx._seam_groups
_kind_counts = RenderCtx._kind_counts
_render_actions = RenderCtx._render_actions
_RustScan = RustScan
_ScanFlags = ScanFlags
_rust_finding_rel = RustScan._rust_finding_rel
_actions_from_rust = RustScan._actions_from_rust
_scanner_candidates = RustScan._scanner_candidates
_scanner_build_is_stale = RustScan._scanner_build_is_stale
_scan_input_dir = INPUTS.directory
_temp_input = INPUTS.write
release_temp_input = INPUTS.release
_GateRunner = GateRunner
_FixCommand = FixCommand
_FIX_ALIASES = FixCommand.ALIASES
_fix_directive_kind = FixCommand.directive_kind
_fix_refusal = FixCommand.refusal
_fix_identifier_problem = FixCommand.identifier_problem
_name_required_kinds = FixCommand.name_required_kinds
_name_is_invalid = FixCommand.name_is_invalid
_trim_preview_diff = FixCommand.trim_preview_diff
_LucidlintConfig = LucidlintConfig
_gate_exit = GateRunner.exit_code
_Services = Services
RULE_GROUPS = rule_metadata.CATALOG.groups()
_ALL_RULE_KINDS = frozenset(rule_metadata.CATALOG.kinds())
CONTRACT_VERSION = GraphContract.CONTRACT_VERSION
_SCANNER_FAILURES = RustScan.SCANNER_FAILURES
parse_args = Cli.parse_args


def main() -> int:
    """The process entry (`lucidlint:main`): the CLI does the work."""
    return Cli(Cli.parse_args()).run()


if __name__ == "__main__":
    sys.exit(main())

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

import argparse
import difflib
import json
import os
import re
import subprocess
import sys
import tempfile
from collections import Counter
from pathlib import Path
from subprocess import SubprocessError
from types import ModuleType
from typing import NamedTuple

import rule_metadata
from orchestrator.action import Action
from orchestrator.baseline import Baseline
from orchestrator.config import LucidlintConfig
from orchestrator.console import CONSOLE
from orchestrator.coverage_context import CoverageContext, coverage_context
from orchestrator.coverage_input import CoverageInput
from orchestrator.coverage_result import CoverageResult
from orchestrator.file_history import FileHistory
from orchestrator.gate_runner import GateRunner
from orchestrator.git_head import GitHead
from orchestrator.graph_contract import GraphContract
from orchestrator.render_ctx import RenderCtx
from orchestrator.rust_finding import RustFinding
from orchestrator.rust_findings import RustFindings
from orchestrator.rust_fix_request import RustFixRequest
from orchestrator.rust_scan import INPUTS, RustScan
from orchestrator.scan_flags import ScanFlags
from orchestrator.services import SERVICES, Services
from orchestrator.source_file import SourceFile
from orchestrator.source_walk import SourceWalk, gitignored_docs
from orchestrator.vendoring import vendor_path
from orchestrator.version import VERSION as _VERSION
from orchestrator.version import version_from_bundle

__all__ = [
    "Action",
    "Baseline",
    "CONSOLE",
    "CoverageContext",
    "CoverageInput",
    "CoverageResult",
    "FileHistory",
    "GitHead",
    "RenderCtx",
    "RustFinding",
    "RustFindings",
    "RustFixRequest",
    "RustScan",
    "ScanFlags",
    "SourceFile",
    "SourceWalk",
    "_RenderCtx",
    "_RustScan",
    "_ScanFlags",
    "_actions_from_rust",
    "_apply_baseline",
    "_baseline_entry_is_current",
    "_baseline_file_state",
    "_coverage_context",
    "_excluded_part",
    "_facts_line",
    "_fix_of",
    "_gitignored_docs",
    "_iso_day",
    "_kind_counts",
    "_load_baseline",
    "_md_rels",
    "_merge_key",
    "_numbits_to_lines",
    "_position_key",
    "_print_trimmed_diff",
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
    "_vendor_path",
    "_version_from_bundle",
    "action_key",
    "is_test_path",
    "log",
    "main",
    "release_temp_input",
]

# the entry module's own directory: the release bundle's root (deps/,
# version.txt) sits beside it
_here = os.path.dirname(os.path.abspath(__file__))
# Run as a script the module is `__main__`; the host seams resolve the entry
# module by its installed name, so make that name point here in both modes.
sys.modules.setdefault("lucidlint", sys.modules[__name__])

# the entry module is the facade: these names stay here because callers (and
# the tests) reach them on `lucidlint`
_version_from_bundle = version_from_bundle
_vendor_path = vendor_path
log = CONSOLE.warn
_print_trimmed_diff = CONSOLE.print_diff
# the source list and docs walk live in orchestrator.sources; the names stay
# here because the entry module is the facade
action_key = Action.key
_coverage_context = coverage_context
_CoverageInput = CoverageInput
_numbits_to_lines = CoverageResult.numbits_to_lines
_raw_score = FileHistory.raw_score
_iso_day = FileHistory.iso_day
_RenderCtx = RenderCtx
_RustScan = RustScan
_GateRunner = GateRunner
_LucidlintConfig = LucidlintConfig
_gate_exit = GateRunner.exit_code
_Services = Services
RULE_GROUPS = rule_metadata.CATALOG.groups()
_ALL_RULE_KINDS = frozenset(rule_metadata.CATALOG.kinds())
CONTRACT_VERSION = GraphContract.CONTRACT_VERSION
_SCANNER_FAILURES = RustScan.SCANNER_FAILURES
_ScanFlags = ScanFlags
_rust_finding_rel = RustScan._rust_finding_rel
_actions_from_rust = RustScan._actions_from_rust
_scanner_candidates = RustScan._scanner_candidates
_scanner_build_is_stale = RustScan._scanner_build_is_stale
_scan_input_dir = INPUTS.directory
_temp_input = INPUTS.write
release_temp_input = INPUTS.release
_facts_line = RenderCtx._facts_line
_render_file_group = RenderCtx._render_file_group
_fix_of = RenderCtx._fix_of
_stamp_of = RenderCtx._stamp_of
_seam_groups = RenderCtx._seam_groups
_kind_counts = RenderCtx._kind_counts
_render_actions = RenderCtx._render_actions
_baseline_entry_is_current = Baseline.entry_is_current
_baseline_file_state = Baseline.file_state
_load_baseline = Baseline
_apply_baseline = Baseline.apply
_tracked_churn_path = FileHistory.tracked_churn_path
_position_key = Action.position_key
_merge_key = Action.merge_key
_excluded_part = SourceWalk.excluded_part
is_test_path = SourceWalk.is_test_path
_md_rels = SourceWalk.md_rels
_gitignored_docs = gitignored_docs

# the fix engine is optional — the fix command degrades to a clear error
# when the `fix` extra is not installed
#
# The release bundle self-contains the fix engine's libcst in a sibling
# `deps/` dir (review-log B8): when a `deps/` sits next to this file (the
# bundle, not a wheel install), prefer it so the fix path works with no pip
# step. A wheel/source install has no `deps/` and uses site-packages libcst.


# The JSON inputs one scan hands the binary (graph contract, churn, coverage)
# are big — a graph dump is tens of MB — and the binary needs a PATH, not a
# pipe. One stdlib TemporaryDirectory per process owns them: every input is a
# file inside it, the directory removes itself (and anything left in it) at
# exit, and a per-run input is deleted as soon as its scan has read it. There is
# no registry to keep and no lock to take — the filesystem is the only shared
# state, and each input's name is unique.
_scan_inputs: tempfile.TemporaryDirectory[str] | None = None








_vendor = _vendor_path(_here)
if _vendor is not None and _vendor not in sys.path:
    sys.path.insert(0, _vendor)
try:
    import fix_engine
    # lucidlint: ignore swallow optional extra — fix degrades to a clear error
except ImportError:
    fix_engine = None

# Role/pattern suffixes from coding-standards.md: communicative for a thin
# framework-role class (MVC controller, event handler) that delegates; a smell


ACTION_KINDS = (
    "complexity",
    "large-function",
    "hub-file",
    "hotspot",
    "high-risk",
    "record-shape",
    "latent-class",
    "vague-name",
    "standard",
    "docs",
    "folder-mix",
    "layer-mix",
)






    # kind when present, else the rule's default link (Rule.fix); empty =
    # fix-less finding, never constructed into a Fix
















    # directive's kind when present, else the rule's default link




class _File(NamedTuple):
    """The file a fix targets — its repo and repo-relative path. The fixers
    are methods on it: every fixer needs the same (repo, rel) pair (the
    strewing pattern: functions sharing a domain class are its methods)."""

    repo: Path
    rel: str

    def fix_rust(self, kind: str, line: int, name: str | None) -> int:
        """A Rust fix runs in the scan core (syn), not the Python/libcst
        engine: invoke the binary's --fix mode, which edits in place."""
        repo, rel = self.repo, self.rel
        binary = SERVICES.rust_scan.binary(repo)
        if binary is None:
            print("fix: the Rust scan core is required — build it with `make scanner-check`")
            return 1
        target = repo / rel
        before = target.read_text(encoding="utf-8") if target.exists() else None
        if kind in _name_required_kinds() and not (name or "").strip():
            # the scanner would get "" and either write `fn ()` or fail
            # with "nothing to change" — both lie about a missing
            # prerequisite. Refuse with the naming guidance instead.
            print(_fix_refusal(kind, None, None, rel, line))
            return 0
        spec = RustFixRequest(kind=kind, file=str(target), line=line, name=name or "").to_json()
        try:
            proc = subprocess.run(
                [str(binary), "--fix", spec], capture_output=True, text=True, timeout=120, cwd=str(repo)
            )
        except subprocess.SubprocessError:
            print(f"fix: the Rust fix core failed for {rel}:{line}")
            return 1
        if proc.returncode != 0:
            # the scanner exits non-zero on an unknown/malformed request — an
            # unexpected error must surface (R29), not masquerade as a success
            if proc.stderr:
                sys.stderr.write(proc.stderr)
            return 1
        if before is not None:
            _print_trimmed_diff(before, target.read_text(encoding="utf-8"), rel)
        return 0

    def finding_lines(self, kind: str) -> list[int]:
        """The lines of every finding of `kind` in this file — the R27 line
        resolution: the tool scans, the agent never counts lines.

        `kind` is the FIX kind (the directive's `fix: <kind>` tail — what the
        agent's command names): a dispatch-shaped complexity finding carries
        the kind `complexity` but its message directive says
        `fix: dispatch-registry`, so the match is against the message's
        directive, not the raw kind."""
        wanted = _FIX_ALIASES.get(kind, kind)
        found = []
        for f in self.scan_single_file():
            directive = _fix_directive_kind(f.message)
            if directive is None:
                # a finding with no directive has no fix — it never matches
                continue
            if _FIX_ALIASES.get(directive, directive) == wanted:
                found.append(f.line)
        # two operands on one line (magic-number twins) are one ANCHOR —
        # re-attachment must not see "live findings at 2, 2"
        return sorted(set(found))

    def scan_single_file(self):
        """Scan this file, yielding its findings."""
        repo, rel = self.repo, self.rel
        let_include_tests = False  # the fix resolves one file's findings
        SERVICES.rust_scan.prepare(repo, rel, let_include_tests, Counter())
        files = SourceWalk(repo).py_files(rel)
        rust = SERVICES.rust_scan.load(repo, files)
        if rust is None:
            return
        yield from rust.by_rel.get(rel, [])
















# variant kind -> its display family (latent-class, loop-pipeline). A
# finding whose display kind IS its suppression family needs no recipe —
# `ignore latent-class <why>` covers every variant (RULES.md); offering
# "suppress with: <raw kind>" beside a report that says "create the class"
# invites hiding the defect (issue #21). The `standard` catch-all is NOT a
# suppression identity, so its findings keep the raw-signal note.





# the failure modes of a scanner invocation — a subprocess that dies, times
_SCANNER_FAILURES = (OSError, SubprocessError, json.JSONDecodeError, ValueError)


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

CONTRACT_VERSION = 1




def parse_args() -> argparse.Namespace:
    p = argparse.ArgumentParser(
        prog="lucidlint",
        description=(
            "deterministic code health for Python and Rust: a compiled scan core "
            "under a thin orchestrator. Run the gate with no subcommand; apply "
            "deterministic fixes with the `fix` subcommand (finding messages "
            "carry the exact command)."
        ),
    )
    p.add_argument(
        "--version",
        action="version",
        version=f"lucidlint {_VERSION}",
        help="print the version and exit",
    )
    p.add_argument("--repo", type=Path, default=Path.cwd(), help="repository root (default: cwd)")
    p.add_argument(
        "--file",
        type=str,
        default=None,
        help="scan ONE repo-relative file (the LSP mode): per-file findings only, "
        "no git history / graph / coverage / repo-wide scans",
    )
    p.add_argument("--include-tests", action="store_true", help="also analyze test files/nodes")
    p.add_argument(
        "--baseline",
        type=Path,
        default=None,
        help="baseline JSON of acknowledged actions; listed actions are reported but do not fail the gate",
    )
    p.add_argument(
        "--update-baseline",
        action="store_true",
        help="write all current action keys to --baseline and exit 0 (lock the list, like pyrefly baselines)",
    )
    p.add_argument("--json", action="store_true", help="emit actions as JSON object (meta + actions) on stdout")
    p.add_argument(
        "--refresh-coverage",
        action="store_true",
        help="run the repo's coverage suite (make coverage) before scanning so coverage verdicts are fresh (slow)",
    )
    p.add_argument("--warn", action="store_true", help="exit 0 even when actions exist (informational run)")

    sub = p.add_subparsers(dest="command", metavar="COMMAND")
    fix = sub.add_parser(
        "fix",
        help="apply a deterministic fix for one finding (the finding's message carries the exact command)",
    )
    fix.add_argument(
        "--kind",
        type=str,
        default=None,
        help="the fix family (from the finding message: magic-number, extract-method, stale-suppression, ...)",
    )
    fix.add_argument("--file", type=str, default=None, help="repo-relative file for the finding")
    fix.add_argument("--line", type=int, default=0, help="line of the finding (omitted when the file has one)")
    fix.add_argument(
        "--name",
        type=str,
        default=None,
        help="the semantic name the tool cannot invent: a constant for magic-number, the extracted method/class name",
    )
    fix.add_argument(
        "--callee",
        type=str,
        default=None,
        help="the callee whose --params these are, when the finding's message names it "
        "(binds the fix on lines with nested calls)",
    )
    fix.add_argument(
        "--params",
        type=str,
        default=None,
        help="comma-separated callee parameter names for positional-literals (external callees)",
    )
    fix.add_argument(
        "--confirm",
        action="store_true",
        help="explicitly apply a previewed structural fix (the name IS the commitment for extract-method)",
    )
    return p.parse_args()






























# --------------------------------------------------------------------------- #33/#34 render aids






# The report's closing ask (issue feedback): the gate checks this repo's
# code, never the soundness of its own advice. A suggestion that would make
# the reader's code worse, or a message the reader cannot act on, is a defect
# only the reader can see — the rules and fixtures cannot supply a case the
# rule does not yet exclude. Per-file runs stay silent (the LSP reprints the
# report on the next edit) and --json stdout stays a machine document.








# the gate reports DISPLAY kinds (final_kind output: strewing shows as
# latent-class); the fix surfaces (Python + Rust) accept either and normalize
# here. Kept in the orchestrator so the .rs fix path (and the R27 line
# resolution) work even when fix_engine is None (libcst missing) — the Rust
# fix surface does not need libcst.
_FIX_ALIASES = {
    "latent-class": "extract-class",
    "complexity": "extract-method",
    "large-function": "extract-method",
}








def _trim_preview_diff(diff, name, new_source):
    """Shape the extract-method preview: the call-site hunk (header + a few
    context lines + the inserted call + the next context line — the removed
    lines duplicate the extracted body) plus the extracted method's first
    lines. The agent names and comprehends from the method being created."""
    hunks, cur = [], []
    for line in diff:
        if line.startswith("@@"):
            if cur:
                hunks.append(cur)
            cur = [line]
        else:
            cur.append(line)
    if cur:
        hunks.append(cur)
    call_hunk = next(
        (h for h in hunks if any(ln.startswith("+") and f"{name}(" in ln for ln in h)),
        hunks[0] if hunks else [],
    )
    if call_hunk:
        call_idx = next(i for i, ln in enumerate(call_hunk) if ln.startswith("+") and f"{name}(" in ln)
        head = [ln for ln in call_hunk[1:call_idx] if ln.startswith(" ")][:3]
        tail = next((ln for ln in call_hunk[call_idx + 1 :] if ln.startswith(" ")), None)
        shown = [call_hunk[0]] + head + [call_hunk[call_idx]]
        if tail:
            shown.append(tail)
        omitted = len(call_hunk) - len(shown)
        if omitted > 0:
            shown.append(f"... ({omitted} diff lines omitted)")
        call_hunk = shown
    src_lines = new_source.splitlines()
    def_idx = next(
        (i for i, ln in enumerate(src_lines) if ln.startswith("def ") and name in ln),
        None,
    )
    body: list[str] = []
    if def_idx is not None:
        for ln in src_lines[def_idx + 1 :]:
            if ln and not ln[0].isspace():
                break
            body.append(ln)
    return call_hunk, def_idx, body


def _name_required_kinds() -> set[str]:
    """The engine owns the name-required policy. With libcst absent nothing
    can APPLY anyway, so an empty set only degrades refusal wording, never
    gating (the engine-present paths always see the real set)."""
    return set(fix_engine._NAME_REQUIRED_KINDS) if fix_engine else set()


def _name_is_invalid(name: str | None) -> bool:
    """A --name that is not a Python identifier cannot be built into a fixer's
    node; a missing name is fine (previews run nameless)."""
    return name is not None and not name.isidentifier()


def _fix_identifier_problem(kind: str, name: str | None, params: list[str] | None, where: str) -> str | None:
    """The invalid-argument guard: a non-identifier --name or --params entry
    would construct an invalid libcst node inside a fixer. Refused before
    dispatch with a message; a MISSING name is NOT refused here — previews
    run nameless by contract."""
    if kind in _name_required_kinds() and _name_is_invalid(name):
        return (
            f"fix: --name must be a valid Python identifier, got '{name}' "
            f"at {where} - replace the message's <placeholder> with the real name"
        )
    if params is not None:
        bad = [p for p in params if not p.isidentifier()]
        if bad:
            return f"fix: --params entries must be valid identifiers, got {bad} at {where}"
    return None


def _fix_refusal(kind: str, name: str | None, params: list[str] | None, file: str, line: int) -> str:
    """Why a fix produced nothing. A name-required kind with a missing or
    non-identifier name is NOT 'nothing to change' — it is an unsatisfied
    prerequisite, and the silence would read as the finding being
    unfixable (the LSP placeholder flow depends on this message)."""
    if kind in _name_required_kinds() and name is None:
        # an unsatisfied prerequisite, not 'nothing to change': the silence
        # would read as unfixable (the LSP placeholder flow depends on this)
        return (
            f"fix: {kind} needs a semantic name the tool cannot invent "
            f"(--name <Name>) at {file}:{line} - name it with a domain noun; naming is the judgement call"
        )
    return _fix_identifier_problem(kind, name, params, f"{file}:{line}") or (
        f"fix: nothing to change for {kind} at {file}:{line}"
    )


class _FixCommand:
    """The `lucidlint fix` command: R27 line resolution, the engine checks,
    the preview surface, and the apply path — one command's behavior, not a
    slice of main()."""

    def __init__(self, args, repo: Path):
        self.args: argparse.Namespace = args
        self.repo: Path = repo
        self.rel: str = args.file or ""
        self.fix_kind: str = ""

    def _params(self) -> list[str] | None:
        return self.args.params.split(",") if self.args.params else None

    def run(self) -> int:
        # the agent-driven fix surface: `lucidlint fix --kind X --file F --line N`
        if not self.rel.endswith((".py", ".rs")):
            # the fix engines (libcst for Python, syn for Rust) cannot touch a
            # non-Python/Rust target; refuse clearly instead of a parse crash
            print(
                f"fix: the fix engine only supports Python and Rust files — '{self.rel}' "
                f"is neither; refactor it by hand"
            )
            return 1
        self.fix_kind = _FIX_ALIASES.get(self.args.kind, self.args.kind)
        # validate BEFORE dispatch: an INVALID identifier (<CONST>) reaching a
        # fixer constructs an invalid libcst Name and dies with a traceback,
        # not a message (the LSP flow hands the verbatim tokens to a shell).
        # A MISSING name still previews — the preview IS the line-number-free
        # contract; only the apply needs the commitment.
        bad = _fix_identifier_problem(
            self.fix_kind, self.args.name, self._params(), f"{self.args.file}:{self.args.line}"
        )
        if bad:
            print(bad)
            return 1
        if self.args.line == 0:
            exit_code = self._resolve_line()
            if exit_code is not None:
                return exit_code
        if self.rel.endswith(".rs"):
            # a Rust fix runs in the scan core (syn) — libcst is Python-only
            if self.fix_kind == "extract-method" and self.args.name is None and not self.args.confirm:
                # H4: extract-method's semantic name is REQUIRED. A name-less
                # request forwarded to the scanner would serialize "" and write
                # `fn ()` — invalid Rust — so refuse silently (R28; the
                # directive prose already told the agent to pass --name). There
                # is no Rust preview surface yet.
                print(_fix_refusal(self.fix_kind, self.args.name, self._params(), self.args.file, self.args.line))
                return 0
            return _File(self.repo, self.rel).fix_rust(self.fix_kind, self.args.line, self.args.name)
        # Python: the libcst fix engine is a mandatory dependency
        fe = fix_engine
        if fe is None:
            # the release bundle vendors libcst in deps/ when it can (the
            # static musl bundle cannot); a wheel install always declares it
            print(
                "fix: the Python fix engine requires libcst and none is installed here — "
                "install lucidlint from the pip wheel (libcst is a declared dependency), "
                "or run `uv sync` in a dev checkout"
            )
            return 1
        # schema-3 anchor: same-line twins need the finding's column — the
        # innermost match wins, mirroring the peel binding order. The
        # fix-kind to finding-kind mapping differs: extract-record-class is
        # the fix for record-shape findings; magic-number is its own kind.
        col = 0
        signal = "record-shape" if self.fix_kind == "extract-record-class" else self.fix_kind
        if signal:
            anchors = [
                f.col
                for f in _File(self.repo, self.rel).scan_single_file()
                if f.signal == signal and f.line == self.args.line and f.col
            ]
            col = max(anchors) if anchors else 0
        if self.fix_kind not in fe.MECHANICAL_KINDS and self.fix_kind not in fe.STRUCTURAL_KINDS:
            # R31 (docs/PRD.md): messages address the actual mistake in
            # plain language — never a bare "no auto-fix exists"
            if self.fix_kind in _ALL_RULE_KINDS:
                print(
                    f"fix: '{self.args.kind}' cannot be fixed by this command — "
                    f"the report line for it says what to change"
                )
            else:
                near = difflib.get_close_matches(self.fix_kind, fe.FIXABLE_KINDS, n=1)
                hint = (
                    f" Did you mean '{near[0]}'?"
                    if near
                    else " Run the scan — its report lines name the checks this command can fix."
                )
                print(f"fix: '{self.args.kind}' is not a fixable kind.{hint}")
            return 1
        req = fe._FixRequest(
            kind=self.args.kind,
            repo=self.repo,
            rel=self.rel,
            line=self.args.line,
            opts=fe.FixOptions(params=self._params(), name=self.args.name, callee=self.args.callee),
            col=col,
        )
        if self._previews_only(fe):
            return self._preview(req)
        return self._apply(req)

    def _previews_only(self, engine: ModuleType) -> bool:
        """A fix kind that only previews, with no name or --confirm, stops at
        the preview — the caller never meant to write."""
        return self.fix_kind in engine.PREVIEW_KINDS and not self.args.name and not self.args.confirm

    def _resolve_line(self) -> int | None:
        """R27: agents never compute line numbers — the tool owns its own
        coordinates; when the file has exactly one finding of the kind, no
        --line is needed. Returns an exit code when the request cannot
        proceed, None when the line is resolved."""
        # R27: agents never compute line numbers — the tool owns its own
        # coordinates; when the file has exactly one finding of the kind,
        # no --line is needed (both the .py and .rs fix surfaces)
        lines = _File(self.repo, self.args.file).finding_lines(self.args.kind)
        if len(lines) == 1:
            self.args.line = lines[0]
        elif not lines:
            return 0  # nothing of this kind exists — R28 silence
        else:
            print(
                f"fix: {len(lines)} {self.args.kind} findings in {self.args.file} "
                f"(lines {', '.join(map(str, lines))}) — pass --line to pick one"
            )
            return 0
        return None

    def _preview(self, req) -> int:
        # the name-free preview surface: show the proposed refactoring
        # as a diff (the seam with a placeholder name — no --fix-name
        # needed to see it). The agent reviews the seam, then re-runs
        # with --fix-name <name>; the name is the commitment, so the
        # named run applies — no --confirm dance
        new_source, description = req.propose_finding()
        if new_source is None:
            # no seam exists — the finding is unfixable HERE; the missing-name
            # refusal would send the agent on a naming chase that cannot
            # succeed (the name-required gate lives in the apply path, where
            # a seam EXISTS) (review bot)
            return 0  # no seam — R28 silence
        diff = list(
            difflib.unified_diff(
                (self.repo / self.args.file).read_text().splitlines(),
                new_source.splitlines(),
                fromfile=self.args.file,
                tofile=self.args.file + " (proposed)",
                lineterm="",
            )
        )
        if self.fix_kind == "extract-method":
            # the C shape (subtask-tested): the call-site hunk plus the
            # EXTRACTED METHOD's first lines — agents name and comprehend
            # from the method being created, not the diff head; a bare
            # diff-head truncation left them wanting the cut part and
            # unsure whether _extracted was the final name
            call_hunk, def_idx, body = _trim_preview_diff(diff, self.args.name or "_extracted", new_source)
            print("\n".join(diff[:2]))  # --- / +++ file headers
            print("\n".join(call_hunk))
            print(f"# seam: {description}")
            if def_idx is not None:
                print("\nExtracted (the method being created, first lines):\n")
                print(new_source.splitlines()[def_idx])
                print("\n".join(body[:10]))
                if len(body) > 10:
                    print(f"... ({len(body) - 10} more lines omitted)")
        else:
            if len(diff) > 40:
                diff = diff[:40] + [f"... ({len(diff) - 40} more lines omitted)"]
            print("\n".join(diff))
        print(
            f"# the name `{self.args.name or '_extracted'}` is a placeholder — pick a real domain noun; "
            f"apply it: lucidlint fix --kind {self.args.kind} --file {self.args.file} "
            f"--line {self.args.line} --name <name>"
        )
        return 0

    def _apply(self, req, moved: bool = False) -> int:
        target = self.repo / self.args.file
        before = target.read_text(encoding="utf-8") if target.exists() else None
        description = req.fix_finding()
        if description is None:
            if req.decline:
                # autofix pending agent-supplied input, at a good anchor
                print(f"fix: {self.fix_kind} at {self.args.file}:{req.line} needs input — {req.decline}")
                return 0
            if self.fix_kind in _name_required_kinds() and self.args.name is None:
                print(
                    f"fix: {self.fix_kind} at {self.args.file}:{req.line} needs a semantic name "
                    f"the tool cannot invent — pass --name <Name> (name it with a domain noun)"
                )
                return 0
            return self._reattach_or_silence(req, moved)
        print(f"fix: applied {self.fix_kind} at {self.args.file}:{req.line} — {description}")
        if before is not None:
            # extract-module moves defs into a NEW module: the diff here is
            # the origin file; the created module's content is the preview
            # the agent already reviewed
            _print_trimmed_diff(before, target.read_text(encoding="utf-8"), self.args.file)
        if req.extra_writes or req.deletes:
            self._verify_multi_file(req)
        return 0

    def _reattach_or_silence(self, req, moved: bool) -> int:
        """The anchor no longer matches a writing fix. R27: the tool owns its
        coordinates — re-attach to the finding's current line when that is
        unambiguous (announcing the move), ask when it is not, and stay
        silent when nothing of this kind remains."""
        if moved:
            return 0  # already re-attached once and it still will not write
        lines = [ln for ln in _File(self.repo, self.args.file).finding_lines(self.fix_kind) if ln != self.args.line]
        if len(lines) == 1:
            print(f"fix: anchor moved — {self.fix_kind} now at {self.args.file}:{lines[0]}")
            req.line = lines[0]
            req.source = None
            req.decline = None
            # the reattached line's anchor column — same-line twins need the
            # finding's col just like the direct path (schema-3)
            anchors = [
                f.col
                for f in _File(self.repo, self.args.file).scan_single_file()
                if f.signal == (self.fix_kind if self.fix_kind != "extract-record-class" else "record-shape")
                and f.line == req.line
                and f.col
            ]
            req.col = max(anchors) if anchors else 0
            return self._apply(req, moved=True)
        if len(lines) > 1:
            print(
                f"fix: {self.fix_kind} anchor moved — live findings at "
                f"{', '.join(map(str, lines))} — pass --line to pick one"
            )
        return 0  # nothing of this kind remains — R28 silence

    def _verify_multi_file(self, req) -> None:
        """R8: a multi-file fix is verified by the REPO-WIDE scan — the
        finding kind must be gone (scan_single_file cannot see a chain or a
        split's cross-file layout). A scan failure is NOT flipped into a
        fix failure — the apply already succeeded."""
        try:
            rust = SERVICES.rust_scan.scan(self.repo, self.args, Counter(), None)
        except Exception:
            return
        remaining = [
            f
            for fs in rust.by_rel.values()
            for f in fs
            if f.signal in {"forwarding-chain", "class-module", "delegating-husk"}
        ]
        if remaining:
            rels = sorted({f.file for f in remaining})[:3]
            print(
                f"fix: {self.fix_kind} applied at {self.args.file}:{req.line} — re-scan still finds "
                f"{len(remaining)} related finding(s) in {', '.join(rels)}{'…' if len(rels) > 3 else ''}: "
                "iterate the fix"
            )


def main() -> int:
    args = parse_args()
    repo = args.repo.resolve()

    if args.command == "fix":
        return _FixCommand(args, repo).run()

    return GateRunner(repo, args).run()


def _fix_directive_kind(message: str) -> str | None:
    """The fix kind a finding's directive announces — what the agent's
    `--kind` names. The directive tail is the full command
    (`— fix: lucidlint fix --kind <kind> --file F --line N ...`); the
    pre-command form (`— fix: <kind> <prose>`) appears on message paths
    that have not been rewritten. None when the finding has no directive
    (R28: no fix exists — it never matches a fix request)."""
    idx = message.rfind("— fix: ")
    if idx == -1:
        return None
    tail = message[idx + len("— fix: ") :]
    m = re.search(r"--kind\s+([a-z-]+)", tail)
    if m:
        return m.group(1)
    first = tail.split()[0].strip()
    if first and first != "lucidlint":
        return first
    return None










if __name__ == "__main__":
    sys.exit(main())

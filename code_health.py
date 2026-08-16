#!/usr/bin/env python3
"""code_health.py — the deterministic code-health gate: a thin orchestrator
over the Rust scan core.

The finding engine is the Rust binary (scanner/): every family — per-file,
latent-class partition, the test-only rules, duplicate/unused, record-shape,
complexity, the graph families (via the versioned export contract),
hotspot, over-abstraction, docs — computes there. This orchestrator gathers
the inputs (git file list/history, the graph contract, churn, coverage),
converts the contract findings to Actions verbatim, and renders the gate:
baselines, dedupe/merge, priority, diff marking, exit codes.

    python3 code_health.py --repo /path/to/repo --json
    echo $?   # 1 when there is work to do

The scan thresholds live in the binary (schema 2): CC>=15, fn>=120 lines,
file>=150 edges, risk>=0.8, hotspot top 10% by churn with CC>=15.
Philosophy: the metrics are *proxies* for code that is obviously correct
and cheap to change; each message says what to do in those terms.
"""

from __future__ import annotations

import argparse
import datetime
import json
import os
import re
import shutil
import sqlite3
import subprocess
import sys
import tempfile
import time
import xml.etree.ElementTree as ET
from collections import Counter
from dataclasses import asdict, dataclass, field
from pathlib import Path
from subprocess import SubprocessError
from typing import NamedTuple

# Role/pattern suffixes from coding-standards.md: communicative for a thin
# framework-role class (MVC controller, event handler) that delegates; a smell


EXCLUDED_DIRS = {
    ".git",
    ".venv",
    "node_modules",
    "__pycache__",
    "dist",
    "build",
    ".mypy_cache",
    ".pytest_cache",
    ".ruff_cache",
}


def _excluded_part(part: str) -> bool:
    """An env/tool directory hides vendored code — .venv-htr, .conda-tools,
    node_modules, .mypy_cache, etc. Exact names in EXCLUDED_DIRS plus the
    .venv* / venv* env-dir families (a repo's real modules never live there)."""
    if part in EXCLUDED_DIRS:
        return True
    return part.startswith(".venv") or part.startswith("venv") or part.startswith(".conda")

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

@dataclass
class Action:
    """One finding. Kind families: complexity/large-function merge per target;
    hotspot/hub-file/high-risk/record-shape are separate problems."""

    kind: str
    severity: str
    file: str
    line: int
    function: str
    message: str
    metric: float
    churn: int
    last_modified: str
    tested: str
    note: str = ""
    raw: float = 0.0
    priority: int = 0
    in_diff: bool = False
    kinds: list[str] = field(default_factory=list)
    callers: list[str] = field(default_factory=list)


@dataclass
class FileHistory:
    """Per-file change counts and last-touched dates from git history."""

    churn: Counter[str]
    last_modified: dict[str, str]


@dataclass
class CoverageResult:
    """Covered line sets and the source they came from."""

    lines: dict[str, set[int]] | None
    source: str


@dataclass
class CoverageContext:
    """Coverage provenance + staleness verdict for the run."""

    label: str
    graph_preferred: bool
    stale_note: str


@dataclass
class GitHead:
    """Branch and short commit for report provenance."""

    branch: str
    commit: str


def rel_path(repo: Path, p: str) -> str:
    """Graph stores absolute paths; radon/git use repo-relative. Normalize."""
    p = p.replace("\\", "/")
    root = str(repo.resolve()).replace("\\", "/") + "/"
    if p.startswith(root):
        p = p[len(root) :]
    return p


def is_test_path(rel: str) -> bool:
    parts = rel.split("/")
    return (
        "tests" in parts
        or "__tests__" in parts
        or any(part.startswith("test_") or part.endswith("_test.py") for part in parts[-1:])
    )


def log(msg: str) -> None:
    print(msg, file=sys.stderr)


# --------------------------------------------------------------------------- complexity (radon)
def file_history(repo: Path) -> FileHistory:
    """One pass over git history: per-file change counts and last-modified date.

    Cheap even on long histories; drives the 'is this still live?' judgement
    and the priority ranking.
    """
    churn: Counter[str] = Counter()
    last: dict[str, str] = {}
    try:
        proc = subprocess.run(
            ["git", "-C", str(repo), "log", "--name-only", "--pretty=format:%ad", "--date=short"],
            capture_output=True,
            text=True,
            timeout=120,
        )
    except (OSError, SubprocessError):  # code-health: ignore except git unavailable/corrupt degrades to empty
        log(f"git log unavailable in {repo} — history-based signals are skipped")
        return FileHistory(churn, last)
    if proc.returncode != 0:
        return FileHistory(churn, last)
    date = ""
    for line in proc.stdout.splitlines():
        line = line.strip()
        if not line:
            continue
        if re.match(r"^\d{4}-\d{2}-\d{2}$", line):
            date = line
            continue
        if line.endswith((".py", ".rs", ".md")) and not line.startswith((".venv/", "node_modules/")):
            churn[line] += 1
            last[line] = date
    return FileHistory(churn, last)


def load_coverage(repo: Path) -> CoverageResult:
    """Per-file covered line sets, preferring the repo's own coverage data.

    Sources, in order: coverage.xml (Cobertura, what CI gates on), then
    .coverage (coverage.py SQLite, line_bits format). The graph's TESTED_BY
    edges miss tests that import inside the test body — real coverage data
    does not. lines is None when neither source exists.
    """
    if (repo / "coverage.xml").exists():
        return _coverage_from_xml(repo)
    if (repo / ".coverage").exists():
        return _coverage_from_sqlite(repo)
    return CoverageResult(None, "no coverage data (no coverage.xml, no .coverage)")


def _coverage_from_xml(repo: Path) -> CoverageResult:
    """Cobertura coverage.xml: class line elements with hits > 0."""
    try:
        root = ET.parse(repo / "coverage.xml").getroot()
    except ET.ParseError:
        return CoverageResult(None, "coverage.xml unparseable")
    covered: dict[str, set[int]] = {}
    for cls in root.iter("class"):
        filename = (cls.get("filename") or "").replace("\\", "/")
        if not filename.endswith(".py"):
            continue
        lines = covered.setdefault(rel_path(repo, filename), set())
        for ln in cls.iter("line"):
            if int(ln.get("hits", "0") or 0) > 0:
                try:
                    lines.add(int(ln.get("number")))
                except (TypeError, ValueError):  # code-health: ignore except malformed <line> elements are skipped
                    log(f"ignoring malformed <line> element in {repo / 'coverage.xml'}")
    return CoverageResult(covered or None, "coverage.xml")


def _coverage_from_sqlite(repo: Path) -> CoverageResult:
    """coverage.py .coverage SQLite: line_bits rows per file."""
    try:
        db = sqlite3.connect(repo / ".coverage")
        files = dict(db.execute("SELECT id, path FROM file"))
        covered: dict[str, set[int]] = {}
        for fid, numbits in db.execute("SELECT file_id, numbits FROM line_bits"):
            path = files.get(fid)
            if not path:
                continue
            rel = rel_path(repo, path)
            if not rel.endswith(".py"):
                continue
            covered.setdefault(rel, set()).update(_numbits_to_lines(numbits))
        db.close()
        return CoverageResult(covered or None, ".coverage")
    except sqlite3.Error:
        return CoverageResult(None, ".coverage unreadable")


def _numbits_to_lines(numbits: bytes) -> set[int]:
    """Decode coverage.py line_bits: bit (n-1)%8 of byte (n-1)//8 => line n."""
    lines: set[int] = set()
    for byte_idx, byte in enumerate(numbits):
        if byte:
            for bit in range(8):
                if byte & (1 << bit):
                    lines.add(byte_idx * 8 + bit + 1)
    return lines


def _raw_score(kind: str, metric: float, churn: int, callers: int | None = None) -> float:
    """Continuous risk score: normalized metric x churn factor (x fan-in for high-risk).

    Normalized to a 1-99 percentile ranking in main, so the list spreads
    instead of saturating at 99.
    """
    norm = {
        "latent-class": 0.7,
        "standard": 0.6,
        "docs": 0.5,
        "folder-mix": 0.5,
        "layer-mix": 0.5,
        "vague-name": 0.7,
        "record-shape": 0.7,
        "complexity": min(metric / 40, 1.0),
        "large-function": min(metric / 200, 1.0),
        "hub-file": min(metric / 400, 1.0),
        "hotspot": min(metric / 40, 1.0),
        "high-risk": min(metric, 1.0),
    }.get(kind, 0.5)
    score = norm * (1 + min(churn / 30, 1.5))
    if kind == "high-risk" and callers:
        score *= 1 + min(callers / 5, 1.0)
    return score


@dataclass(frozen=True)
class SourceFile:
    """One scanned .py file: its path and repo-relative name."""

    py: Path
    rel: str


def _py_files(repo: Path, only_rel: str | None = None) -> list[SourceFile]:
    """The repo's own .py and .rs files — git's answer, not an invented list.

    `git ls-files --cached --others --exclude-standard` = tracked files plus
    untracked-not-ignored: exactly what the repo's .gitignore defines as its
    code (venvs, caches, generated output never qualify, whatever they are
    named). One call per run, NUL-split, no quoting issues. With only_rel
    set, returns just that file (the --file / LSP mode).
    """
    if only_rel is not None:
        py = repo / only_rel
        return [SourceFile(py, only_rel)] if py.is_file() and py.suffix in (".py", ".rs", ".md") else []
    try:
        proc = subprocess.run(
            ["git", "-C", str(repo), "ls-files", "--cached", "--others", "--exclude-standard", "-z", "--",
             "*.py", "*.rs", "*.md"],
            capture_output=True, text=True, check=True, timeout=60,
        )
        rels = [r for r in proc.stdout.split("\0") if r]
        return [SourceFile(repo / rel, rel) for rel in sorted(rels)]
    except (OSError, SubprocessError, ValueError):
        # git unavailable (not a repo, submodule-less edge): fall back to the
        # rglob minus the known env/tool dirs — the pre-git behavior
        log("git ls-files failed — falling back to rglob for the file list")
        return [
            SourceFile(py, py.relative_to(repo).as_posix())
            for py in sorted(repo.rglob("*.py")) + sorted(repo.rglob("*.rs")) + sorted(repo.rglob("*.md"))
            if not any(_excluded_part(part) for part in py.parts)
        ]

class RustFinding(NamedTuple):
    """One contract finding — the final action model: kind (action kind,
    owned by the Rust core), signal (suppression identity), severity,
    location, message, and the raw metric the priority percentile runs on
    (cc for complexity; 1.0 for families with fixed norms)."""

    kind: str
    signal: str
    severity: str
    file: str
    line: int
    function: str
    message: str
    metric: float = 1.0


class RustFindings(NamedTuple):
    """The Rust scan output for one file set, keyed by repo-relative path —
    a named object, not a bare map (the record-shape rule's escape hatch)."""

    by_rel: dict[str, list[RustFinding]]
    cc_by_rel: dict[str, list[tuple[str, int, int]]]

    def for_rel(self, rel: str) -> list[RustFinding]:
        return self.by_rel.get(rel, [])


class _RustScan:
    """The Rust scan core — the required finding engine.

    One binary invocation per repo per run (or per file in --file / LSP mode);
    the versioned JSON findings ARE the action model (kind/severity owned by
    the core); this driver converts them to Actions verbatim and fills the
    orchestrator's fields (churn, priority, provenance)."""

    def __init__(self) -> None:
        self._cache: dict[tuple[Path, tuple[str, ...]], RustFindings | None] = {}
        self._binary_cache: dict[Path, Path | None] = {}

    # code-health: ignore global-state the scanner cache is a per-run memo of subprocess
    # output — a pure function of the repo + file set, not mutable state with behavior

    def binary(self, repo: Path) -> Path | None:
        """The scan binary: env override, then the repo's own build, then the
        tool checkout's build, then the distribution bundle's sibling binary
        (a `lucidlint` release installs as <prefix>/bin/lucidlint next to
        code_health.py — the bundle is self-contained, no env needed).
        None when not built — the Python path takes over."""
        if repo in self._binary_cache:
            return self._binary_cache[repo]
        exe = ".exe" if os.name == "nt" else ""
        candidates: list[Path] = []
        env = os.environ.get("CODE_HEALTH_SCANNER")
        if env:
            candidates.append(Path(env))
        candidates.append(repo / "scanner" / "target" / "release" / "code-health-scan")
        candidates.append(Path(__file__).resolve().parent / "scanner" / "target" / "release" / "code-health-scan")
        bundle_dir = Path(__file__).resolve().parent / "bin"
        candidates.append(bundle_dir / f"lucidlint{exe}")
        found = next((p for p in candidates if p.is_file()), None)
        self._binary_cache[repo] = found
        return found

    def load(self, repo: Path, files: list[SourceFile]) -> RustFindings | None:
        return self.load_with(repo, files)

    def load_with(
        self,
        repo: Path,
        files: list[SourceFile],
        graph: Path | None = None,
        churn_json: Path | None = None,
        include_tests: bool = False,
        docs_root: str | None = None,
    ) -> RustFindings | None:
        if graph is None and churn_json is None and not include_tests and docs_root is None:
            graph, churn_json, include_tests, docs_root = self._flags()
        """Findings per rel for one file set; None = Rust unavailable (Python path)."""
        rels = tuple(sf.rel for sf in files)
        key = (repo, rels)
        if key in self._cache:
            return self._cache[key]
        binary = self.binary(repo)
        if binary is None:
            raise RuntimeError(
                "the Rust scan core is required — build it with `make scanner-check`"
            )
        result: dict[str, list[RustFinding]] | None = None
        cc_result: dict[str, list[tuple[str, int, int]]] = {}
        if not files:
            result = {}  # a repo with no .py/.rs files scans clean — GATE: PASS, not an error
        else:
            result = {}
            try:
                cmd = [str(binary)]
                if graph is not None:
                    cmd += ["--graph", str(graph)]
                if churn_json is not None:
                    cmd += ["--churn", str(churn_json)]
                if include_tests:
                    cmd.append("--include-tests")
                if docs_root is not None:
                    cmd += ["--docs", docs_root]
                cmd += [str(sf.py) for sf in files]
                proc = subprocess.run(cmd, capture_output=True, text=True, timeout=300)
                if proc.returncode == 0:
                    data = json.loads(proc.stdout)
                    if data.get("schema_version") != 2:
                        raise RuntimeError(
                            f"scanner contract schema {data.get('schema_version')} — expected 2; "
                            "rebuild the binary (make scanner-check)"
                        )
                    rels_set = set(rels)
                    for f in data.get("findings", []) + data.get("complexity", []):
                        rel = _rust_finding_rel(f.get("file", ""), repo, rels_set)
                        if rel is None:
                            continue
                        result.setdefault(rel, []).append(
                            RustFinding(
                                kind=f.get("kind", "standard"),
                                signal=f.get("signal", f.get("kind", "")),
                                severity=f.get("severity", "fail"),
                                file=rel,
                                line=int(f.get("line", 0)),
                                function=f.get("function", ""),
                                message=f.get("message", ""),
                                metric=float(f.get("metric", 1.0)),
                            )
                        )
                    for e in data.get("cc", []):
                        rel = _rust_finding_rel(e.get("file", ""), repo, rels_set)
                        if rel is None:
                            continue
                        cc_result.setdefault(rel, []).append(
                            (e.get("function", ""), int(e.get("line", 0)), int(e.get("cc", 0)))
                        )
                else:
                    result = None
            except _SCANNER_FAILURES:  # code-health: ignore except degraded runs report nothing — visible
                result = None
        wrapped = (
            RustFindings(result, cc_result)
            if result is not None
            else None
        )
        self._cache[key] = wrapped
        return wrapped

    def prepare(
        self, repo: Path, only_rel: str | None, include_tests: bool, file_churn: Counter[str]
    ) -> None:
        """Graph contract + churn JSON + docs root for the next scan; repo-wide only.
        Each run scans fresh — the per-run cache must not leak across main()
        calls (the tests drive several runs in one process)."""
        self._cache.clear()
        self._pending_graph = None
        self._pending_churn = None
        self._pending_tests = include_tests
        self._pending_docs = None
        if only_rel is None:
            self._pending_graph = GRAPH_CONTRACT.contract(repo)
            if file_churn:
                tmp = Path(tempfile.mkstemp(prefix="code-health-churn-", suffix=".json")[1])
                tmp.write_text(json.dumps(dict(file_churn)), encoding="utf-8")
                self._pending_churn = tmp
            self._pending_docs = str(repo)

    def _flags(self):
        return self._pending_graph, self._pending_churn, self._pending_tests, self._pending_docs

    def active(self, repo: Path) -> bool:
        """True when the Rust core is available (it is required)."""
        return self.binary(repo) is not None


def _rust_finding_rel(file_val: str, repo: Path, rels: set[str]) -> str | None:
    """The binary reports per-file findings with the path as passed and the
    repo-wide ones (duplicate/unused) with the repo-relative path — normalize."""
    if file_val in rels:
        return file_val
    try:
        rel = Path(file_val).resolve().relative_to(repo).as_posix()
    except (ValueError, OSError):  # code-health: ignore except an unmappable path means the finding
        # is for a file outside this scan set — drop it, not a failure to surface
        rel = ""
    return rel if rel in rels else None


RUST_SCAN = _RustScan()

# the failure modes of a scanner invocation — a subprocess that dies, times
_SCANNER_FAILURES = (OSError, SubprocessError, json.JSONDecodeError, ValueError)


class _GraphContract:
    """The code-review-graph export contract, generated through the tool's own
    public API by code_health_graph_export.py — the gate never touches the
    SQLite schema or the DB location. None = tool missing or no graph."""

    def __init__(self) -> None:
        self._cache: dict[Path, Path | None] = {}
        self._adapter = Path(__file__).resolve().parent / "code_health_graph_export.py"

    def _interpreter(self) -> str:
        """The graph tool's own Python (its CLI shebang), else this env."""
        exe = shutil.which("code-review-graph")
        if exe:
            try:
                first = Path(exe).read_text(encoding="utf-8", errors="replace").splitlines()[0]
                if first.startswith("#!"):
                    interp = first[2:].split()[0]
                    if Path(interp).exists():
                        return interp
            except OSError:  # code-health: ignore except a missing interpreter degrades to this env
                pass
        return sys.executable

    def contract(self, repo: Path) -> Path | None:
        """The contract JSON path for the repo, or None (no graph available)."""
        if repo in self._cache:
            return self._cache[repo]
        result: Path | None = None
        try:
            proc = subprocess.run(
                [self._interpreter(), str(self._adapter), "--repo", str(repo)],
                capture_output=True, text=True, timeout=180,
            )
            if proc.returncode == 0 and proc.stdout.strip():
                tmp = Path(tempfile.mkstemp(prefix="code-health-graph-", suffix=".json")[1])
                tmp.write_text(proc.stdout, encoding="utf-8")
                result = tmp
        except _SCANNER_FAILURES:  # code-health: ignore except no graph contract means the gate
            # degrades to the non-graph families with a log, never a crash
            result = None
        self._cache[repo] = result
        return result


GRAPH_CONTRACT = _GraphContract()
def parse_args() -> argparse.Namespace:
    p = argparse.ArgumentParser(
        description="deterministic code-health gate: a Rust scan core under a thin orchestrator"
    )
    p.add_argument("--repo", type=Path, default=Path.cwd(), help="repository root (default: cwd)")
    p.add_argument("--file", type=str, default=None,
                   help="scan ONE repo-relative .py file (the LSP mode): per-file findings only, "
                        "no git history / graph / coverage / repo-wide scans")
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
    p.add_argument(
        "--base",
        type=str,
        default="",
        help=(
            "git ref to diff against; actions in files your branch changed are marked "
            "'in your diff' (default: origin/main, then main)"
        ),
    )
    p.add_argument("--json", action="store_true", help="emit actions as JSON object (meta + actions) on stdout")
    p.add_argument(
        "--refresh-coverage",
        action="store_true",
        help="run the repo's coverage suite (make coverage) before scanning so coverage verdicts are fresh (slow)",
    )
    p.add_argument("--warn", action="store_true", help="exit 0 even when actions exist (informational run)")
    return p.parse_args()


def action_key(a: Action) -> str:
    return f"{a.kind}:{a.file}:{a.line}:{a.function}"


def changed_files(repo: Path, base: str) -> set[str]:
    """Files touched by the current branch vs base ref (best-effort)."""
    refs = [base] if base else ["origin/main", "main"]
    for ref in refs:
        try:
            proc = subprocess.run(
                ["git", "-C", str(repo), "diff", "--name-only", f"{ref}...HEAD"],
                capture_output=True,
                text=True,
                timeout=30,
            )
        except (OSError, SubprocessError):  # code-health: ignore except git unavailable/corrupt — no diff awareness
            log(f"git diff against {ref} unavailable — diff awareness skipped")
            continue
        if proc.returncode == 0 and proc.stdout.strip():
            return {ln.strip() for ln in proc.stdout.splitlines() if ln.strip()}
    return set()


def _coverage_context(repo: Path, covered, coverage_source: str) -> CoverageContext:
    """Coverage provenance label + staleness verdict."""
    if coverage_source == ".coverage" and (repo / ".coverage").exists():
        coverage_source += (
            " (mtime " + time.strftime("%Y-%m-%d %H:%M", time.localtime((repo / ".coverage").stat().st_mtime)) + ")"
        )
    graph_preferred = False
    stale_note = ""
    if covered is not None and (repo / ".coverage").exists():
        cov_mtime = (repo / ".coverage").stat().st_mtime
        newest_test = max((p.stat().st_mtime for p in (repo / "tests").rglob("*.py")), default=0.0)
        if newest_test > cov_mtime:
            graph_preferred = True
            stale_note = (
                " (coverage snapshot older than the repo's tests — graph verdict used; "
                "verify against htmlcov/ if present)"
            )
    return CoverageContext(label=coverage_source, graph_preferred=graph_preferred, stale_note=stale_note)


def _git_head(repo: Path) -> GitHead:
    """Current branch and short commit for report provenance.
    Returns empty strings when git is unavailable."""
    branch = commit = ""
    try:
        branch = subprocess.run(
            ["git", "-C", str(repo), "branch", "--show-current"], capture_output=True, text=True, timeout=30
        ).stdout.strip()
        commit = subprocess.run(
            ["git", "-C", str(repo), "rev-parse", "--short", "HEAD"], capture_output=True, text=True, timeout=30
        ).stdout.strip()
    # code-health: ignore except git-absent is a supported mode — the gate runs on the working tree alone
    except (OSError, SubprocessError):
        log(f"git unavailable in {repo} — report shows no branch/commit")
    return GitHead(branch=branch, commit=commit)


def _dedupe(actions: list[Action]) -> list[Action]:
    """Same kind+file+line+function fires once (graph and radon can both flag a function)."""
    seen: dict[tuple, Action] = {}
    for a in actions:
        key = (a.kind, a.file, a.line, a.function)
        if key not in seen or a.raw > seen[key].raw:
            seen[key] = a
    return list(seen.values())


def _percentile_rank(unique: list[Action], diff: set[str]) -> None:
    """Rank raw risk 1-99 (percentile) so the list spreads; tag in-diff actions."""
    if not unique:
        return
    lo, hi = min(a.raw for a in unique), max(a.raw for a in unique)
    for a in unique:
        a.priority = 99 if hi <= lo else max(1, round(1 + 98 * (a.raw - lo) / (hi - lo)))
        a.in_diff = a.file in diff


def _merge_key(a: Action) -> tuple:
    """(file, function, kind-group): complexity and large-function are one fix family;
    every other kind (hotspot, hub-file, high-risk, record-shape) is its own target —
    a hub-file and a hotspot on the same file are different problems."""
    group = a.kind if a.kind not in ("complexity", "large-function") else "fn"
    return (a.file, a.function, group)


def _merge_targets(unique: list[Action]) -> list[Action]:
    """Per-target merge: complexity + large-function on the same function is one fix."""
    merged: dict[tuple, Action] = {}
    for a in sorted(unique, key=lambda a: (-a.raw, a.file, a.line)):
        key = _merge_key(a)
        if key not in merged:
            merged[key] = a
        else:
            prev = merged[key]
            prev.kinds = sorted({prev.kind, a.kind})
            prev.raw = max(prev.raw, a.raw)
            if a.note and a.note not in prev.note:
                prev.note = (prev.note + " " + a.note).strip()
            prev.line = min(prev.line, a.line)
            if a.severity == "fail":
                prev.severity = "fail"  # a warn merged into a fail target must not clear the gate
            if a.message != prev.message:
                extra = f"{a.severity.upper()}: {a.message}"
                if extra not in prev.note:
                    prev.note = (prev.note + " " + extra).strip()
    return list(merged.values())


def _lifecycle_notes(unique: list[Action]) -> None:
    """Facts only — low-churn scripts/tools. Delete-vs-refactor is the agent's call."""
    for a in unique:
        if a.file.startswith(("scripts/", "tools/")) and a.churn <= 2 and a.last_modified:
            a.note = (
                a.note + f" Lifecycle: {a.churn}x churn, last touched {a.last_modified} — "
                f"low-change file under scripts/tools."
            ).strip()


def _dedupe_merge(actions: list[Action], diff: set[str]) -> list[Action]:
    """Dedupe, rank, merge per-target kinds, then lifecycle notes."""
    unique = _dedupe(actions)
    _percentile_rank(unique, diff)
    unique = _merge_targets(unique)
    # Re-rank on the merged raw values, but KEEP the diff marking — the
    # merged actions must still show "[in your diff]" (PRD R10).
    _percentile_rank(unique, diff)
    unique.sort(key=lambda a: (-a.priority, a.file, a.line))
    _lifecycle_notes(unique)
    return unique


def _load_baseline(path) -> set[str]:
    """Acknowledged action keys from the baseline file (best-effort)."""
    if path and path.exists():
        try:
            return set(json.loads(path.read_text()).get("actions", []))
        except (json.JSONDecodeError, AttributeError):  # code-health: ignore except corrupt baseline; gate unbaselined
            log(f"baseline {path} unreadable — ignoring")
    return set()


def _render_json(repo: Path, args, unique: list[Action], branch: str, commit: str, coverage_source: str) -> None:
    print(
        json.dumps(
            {
                "meta": {
                    "repo": str(repo),
                    "branch": branch,
                    "commit": commit,
                    "generated_at": datetime.date.today().isoformat(),
                    "base_ref": args.base or "origin/main|main",
                    "coverage_source": coverage_source,
                    "thresholds": {
                        "max_complexity": 15,
                        "max_function_lines": 120,
                        "max_file_edges": 150,
                        "max_risk": 0.8,
                        "hotspot_top_frac": 0.1,
                        "hotspot_min_cc": 15.0,
                    },
                },
                "baseline": str(args.baseline) if args.baseline else "",
                "actions": [asdict(a) for a in unique],
            },
            indent=2,
        )
    )


def _render_summary(
    repo: Path,
    args,
    fails: list[Action],
    warns: list[Action],
    acks: list[Action],
    diff: set[str],
    coverage_source: str,
    graph_preferred: bool,
) -> None:
    """Gate verdict, scope, and formula lines."""
    top = fails[0]
    mine = sum(1 for a in fails if a.in_diff)
    mine_txt = f"; {mine} of {len(fails)} actions in files your diff touches" if diff else "; diff base unresolved"
    if args.baseline is None:
        mine_txt += " (no baseline — cannot tell what is new)"
    targets = len({(a.file, a.function) for a in fails})
    verdict = "GATE: FAIL" if not args.warn else "GATE: INFORMATIONAL (--warn)"
    print(
        f"{verdict} — {len(fails)} action(s) across {targets} distinct targets "
        f"(+{len(acks)} acknowledged in baseline, {len(warns)} warnings never-fail){mine_txt}, "
        f"top P{top.priority} {top.file}:{top.line} ({top.function or top.kind})"
    )
    print(
        "priority ranks change-cost (churn x fan-in), not brokenness — which item is worth "
        "fixing first is a judgement call; the hotspot entries are the usual starting set"
    )
    if graph_preferred:
        print(
            "WARNING: coverage snapshot predates the repo's tests — hard 'untested' claims are suppressed; "
            "run --refresh-coverage (make coverage) for definite test-status verdicts"
        )
    print(
        "priority = percentile of raw risk (metric norm x (1 + churn/30) x (1 + callers/5)); "
        "norms: CC/40, lines/200, edges/400, risk/1 (norm capped at 1.0, churn factor at 1.5, callers factor at 1.0) "
        "— the displayed thresholds are the fail bars, not the norms; "
        "thresholds: CC>=15, fn>=120 lines, file>=150 edges, risk>=0.8, hotspot top 10% "
        + f"by churn with CC>=15; coverage: {coverage_source}"
    )


def _render_file_group(file: str, items: list[Action]) -> None:
    """One file's actions, priority-ordered, with notes."""
    touched = " [in your diff]" if any(i.in_diff for i in items) else ""
    print(f"\n{file}{touched}")
    for a in items:
        loc = f":{a.line}" + (f" ({a.function})" if a.function else "")
        churn = f" [churn {a.churn}x]" if a.churn else ""
        kinds = ",".join(a.kinds) if a.kinds else a.kind
        tag = f"P{a.priority:02d}" if a.severity != "warn" else "warn"
        print(f"  [{tag}][{kinds}] {loc}{churn} — {a.message}")
        if a.note:
            print(f"      -> {a.note}")


def _kind_counts(actions: list[Action]) -> str:
    """Category-count roll-up: record-shape=261, standard=199, ... — the
    volume each rule contributes, so a 500-action report is scannable."""
    counts = Counter(a.kind for a in actions)
    return ", ".join(f"{k}={v}" for k, v in counts.most_common())


def _render_actions(repo: Path, args, fails: list[Action], acks: list[Action]) -> None:
    """Per-file grouped action lines, baseline acknowledgements, and the footer."""
    by_file: dict[str, list[Action]] = {}
    for a in fails:
        by_file.setdefault(a.file, []).append(a)
    for file, items in sorted(by_file.items(), key=lambda kv: -max(i.priority for i in kv[1])):
        _render_file_group(file, items)
    if acks:
        print(
            f"\nacknowledged in baseline ({len(acks)}): "
            + ", ".join(f"{a.file}:{a.line}" for a in acks[:5])
            + (" …" if len(acks) > 5 else "")
        )
    print(
        "\nre-run: python3 code_health.py --repo "
        + str(repo)
        + (" --baseline " + str(args.baseline) if args.baseline else "")
        + "   | tool lives in build-tools (github.com/ashbywinch/build-tools); thresholds and"
        + " per-action data in --json output"
    )
    print(
        "baseline: '--update-baseline --baseline code-health.json' acknowledges today's debt so the "
        "gate only fails on NEW actions; this report is a snapshot, not wired into CI"
    )


def _render_text(
    repo: Path,
    args,
    unique: list[Action],
    fails: list[Action],
    warns: list[Action],
    acks: list[Action],
    diff: set[str],
    coverage_source: str,
    graph_preferred: bool,
) -> None:
    if not unique:
        print("GATE: PASS — clean, no actions")
        return
    if not fails:
        warn_note = f" ({len(warns)} warnings reported, never fail)" if warns else ""
        print(f"GATE: PASS — {len(acks)} action(s) acknowledged in baseline{warn_note}")
        if warns:
            print(f"by kind — warnings: {_kind_counts(warns)}")
            _render_actions(repo, args, warns, [])
        return
    _render_summary(repo, args, fails, warns, acks, diff, coverage_source, graph_preferred)
    print(f"by kind — fails: {_kind_counts(fails)}; warnings: {_kind_counts(warns)}")
    _render_actions(repo, args, fails, acks)
    if warns:
        print(f"\nwarnings (reported, never fail) — {len(warns)}:")
        _render_actions(repo, args, warns, [])


def _write_baseline(args, unique: list[Action]) -> int:
    """--update-baseline: lock all current action keys and exit clean."""
    if not args.baseline:
        log("--update-baseline requires --baseline PATH")
        return 2
    keys = [action_key(a) for a in unique if a.severity != "warn"]
    args.baseline.write_text(json.dumps({"actions": keys}, indent=2))
    print(f"code-health: baseline written — {len(keys)} action(s) locked to {args.baseline}")
    return 0


def _apply_baseline(unique: list[Action], baseline_keys: set[str]) -> list[str]:
    """Mark acknowledged actions so they report but never fail the gate.

    Both-direction lock (the pyrefly-lock rule, same lesson): a baseline
    entry whose finding the code no longer produces is STALE drift and
    fails the gate — a one-way baseline lets a fix silently rot the
    baseline until someone re-runs update-baseline. Returns the stale keys.
    """
    # Stale comparison is location-INSENSITIVE: the key embeds file:line, so
    # an edit that shifts a function's line would otherwise make every
    # acknowledged entry look stale (false failures + baseline churn — the
    # line-keyed pyrefly baseline cost us exactly this all session). The
    # identity is (kind, file, function): the same debt at a new line is
    # still acknowledged; only debt that is genuinely gone is stale.
    current_ids = {(a.kind, a.file, a.function) for a in unique if a.severity != "warn"}
    baseline_ids = {_baseline_identity(k) for k in baseline_keys}
    stale = sorted(k for k in baseline_keys if _baseline_identity(k) not in current_ids)
    for a in unique:
        if (a.kind, a.file, a.function) in baseline_ids:
            a.severity = "ack"
    return stale


class BaselineIdentity(NamedTuple):
    """An action's (kind, file, function) — the line is excluded so location
    shifts do not rot acknowledged debt."""

    kind: str
    file: str
    function: str


def _baseline_identity(key: str) -> BaselineIdentity:
    parts = key.split(":", 3)
    if len(parts) == 4:
        return BaselineIdentity(parts[0], parts[1], parts[3])
    return BaselineIdentity(key, "", "")


def main() -> int:
    args = parse_args()
    repo = args.repo.resolve()

    if args.file:
        # Single-file / LSP mode: no git history, coverage, or diff — the
        # per-file findings are what an editor shows on save.
        fh = FileHistory(Counter(), {})
        cr = CoverageResult(None, "")
        cc = _coverage_context(repo, None, "")
        diff: set[str] = set()
    else:
        fh = file_history(repo)
        if args.refresh_coverage:
            _refresh_coverage(repo)
        cr = load_coverage(repo)
        cc = _coverage_context(repo, cr.lines, cr.source)
        diff = changed_files(repo, args.base)
    actions = _collect_actions(repo, args, fh.churn, fh.last_modified, only_rel=args.file)
    unique = _dedupe_merge(actions, diff)

    if args.update_baseline:
        return _write_baseline(args, unique)
    stale = _apply_baseline(unique, _load_baseline(args.baseline))
    fails = [a for a in unique if a.severity == "fail"]
    warns = [a for a in unique if a.severity == "warn"]
    acks = [a for a in unique if a.severity == "ack"]
    head = _git_head(repo)

    if args.json:
        _render_json(repo, args, unique, head.branch, head.commit, cc.label)
    else:
        _render_text(repo, args, unique, fails, warns, acks, diff, cc.label, cc.graph_preferred)

    return _gate_exit(stale, fails, args)


def _gate_exit(stale: list[str], fails: list[Action], args) -> int:
    """The gate verdict: stale baseline entries and fail actions both block;
    --warn renders everything informational and exits clean."""
    if args.warn:
        return 0
    if stale:
        log(f"{len(stale)} stale baseline entr{'y' if len(stale) == 1 else 'ies'} — the code no longer "
            f"produces these findings: {', '.join(stale[:5])}{'...' if len(stale) > 5 else ''}; "
            f"run --update-baseline to shrink the baseline")
    if fails:
        log(f"{len(fails)} action(s) found — failing (use --warn to run informational)")
    return 1 if (stale or fails) else 0

# --------------------------------------------------------------------------- scan driver

TEST_ONLY_SIGNALS = {"suppression", "type-ignore", "monkeypatch", "skipif", "fakefs", "allow-reason"}


def _actions_from_rust(
    rust: RustFindings, include_tests: bool, file_churn: Counter[str], last_modified: dict[str, str]
) -> list[Action]:
    """The contract findings ARE the action model — converted verbatim, with
    the orchestrator's fields (churn, provenance, raw risk) filled in."""
    actions: list[Action] = []
    for rel, findings in rust.by_rel.items():
        if not include_tests and is_test_path(rel):
            # test files are excluded from the health scan, but the rules
            # that live in tests are scanned for alone
            findings = [f for f in findings if f.signal in TEST_ONLY_SIGNALS]
        for f in findings:
            churn = file_churn.get(rel, 0)
            actions.append(
                Action(
                    kind=f.kind,
                    severity=f.severity,
                    file=rel,
                    line=f.line,
                    function=f.function,
                    message=f.message,
                    metric=f.metric,
                    churn=churn,
                    last_modified=last_modified.get(rel, ""),
                    tested="",
                    raw=_raw_score(f.kind, 1, churn),
                )
            )
    return actions


def _collect_actions(repo: Path, args, file_churn: Counter[str], last_modified: dict[str, str],
                     only_rel: str | None = None) -> list[Action]:
    """Every finding family computes in the Rust core (per-file, partition,
    test rules, duplicate/unused, record-shape, complexity, the graph
    families, hotspot, abstraction, docs); the orchestrator converts and
    renders. The thresholds live in the binary (schema 2)."""
    if not RUST_SCAN.active(repo):
        # no Python fallback — the binary is required; a silent empty scan
        # would report GATE: PASS without checking anything (fail-fast)
        raise RuntimeError(
            "the scan binary is required — build it with `make scanner-check` "
            "or install the lucidscan release bundle"
        )
    RUST_SCAN.prepare(repo, only_rel, args.include_tests, file_churn)
    files = _py_files(repo, only_rel)
    rust = RUST_SCAN.load(repo, files)
    if rust is None:
        raise RuntimeError("the Rust scan core failed — rebuild with `make scanner-check`")
    return _actions_from_rust(rust, args.include_tests, file_churn, last_modified)


def _refresh_coverage(repo: Path) -> None:
    """Run the repo's coverage suite so verdicts are fresh."""
    subprocess.run(["make", "-C", str(repo), "coverage"], capture_output=True, text=True, timeout=1800)

if __name__ == "__main__":
    sys.exit(main())

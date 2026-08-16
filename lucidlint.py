#!/usr/bin/env python3
# lucidlint: ignore-file complexity the orchestrator's git functions are single-pass protocol
# walks — decisions are path branches, not branching logic

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

The scan thresholds live in the binary (schema 2): CC>=15, fn>=120 lines,
file>=150 edges, risk>=0.8, hotspot top 10% by churn with CC>=15.
Philosophy: the metrics are *proxies* for code that is obviously correct
and cheap to change; each message says what to do in those terms.
"""

from __future__ import annotations

import argparse
import datetime
import difflib
import json
import os
import sqlite3
import subprocess
import sys
import tempfile
import time
import tomllib
from importlib import metadata

# the fix engine is optional — the fix command degrades to a clear error
# when the `fix` extra is not installed
try:
    import fix_engine
    # lucidlint: ignore swallow optional extra — fix degrades to a clear error
except ImportError:
    fix_engine = None
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
class _ScanFlags:
    """The optional scan inputs — one object instead of a 4-parameter tail."""
    graph: Path | None = None
    churn_json: Path | None = None
    include_tests: bool = False
    docs_root: str | None = None


@dataclass
class _RenderCtx:
    """The shared render context — one object instead of a 9-parameter tail."""
    repo: Path
    args: argparse.Namespace
    branch: str
    commit: str
    coverage_source: str
    graph_preferred: bool
    diff: set[str]

    def render_json(self, unique: list[Action]) -> None:
        repo = self.repo
        args = self.args
        branch, commit = self.branch, self.commit
        coverage_source = self.coverage_source
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

    def render_summary(self, fails: list[Action], warns: list[Action], acks: list[Action]) -> None:
        """Gate verdict, scope, and formula lines."""
        args = self.args
        diff = self.diff
        coverage_source = self.coverage_source
        graph_preferred = self.graph_preferred
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
            "norms: CC/40, lines/200, edges/400, risk/1 "
            "(norm capped at 1.0, churn factor at 1.5, callers factor at 1.0) "
            "— the displayed thresholds are the fail bars, not the norms; "
            "thresholds: CC>=15, fn>=120 lines, file>=150 edges, risk>=0.8, hotspot top 10% "
            + f"by churn with CC>=15; coverage: {coverage_source}"
        )

    def render_text(self, unique: list[Action], fails: list[Action], warns: list[Action], acks: list[Action]) -> None:
        repo, args = self.repo, self.args
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
        self.render_summary(fails, warns, acks)
        print(f"by kind — fails: {_kind_counts(fails)}; warnings: {_kind_counts(warns)}")
        _render_actions(repo, args, fails, acks)
        if warns:
            print(f"\nwarnings (reported, never fail) — {len(warns)}:")
            _render_actions(repo, args, warns, [])


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
    signal: str = ""  # suppression identity — the raw family kind from the scanner
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
# how much history the churn signal needs: 200 commits reaches ~2 weeks on
# an active repo and years on a quiet one — the percentile ranking is
# relative within the window, so the bound adapts to the repo's activity;
# the age cap is the hard floor for the "is this still live?" judgement
_CHURN_MAX_COMMITS = 200
_CHURN_MAX_AGE_DAYS = 730


def file_history(repo: Path) -> FileHistory:
    """The recent git history: per-file change counts and last-modified date.

    The walk is bounded (the last 200 commits or 730 days, whichever stops
    first) — old churn dilutes the CURRENT hotspot signal, so limiting the
    window is more signal, not less. The walk is deterministic for a given
    HEAD, so the result is cached per (HEAD, window) — repeat gate runs skip
    the walk entirely. `last_modified` is the NEWEST touch (the walk is
    newest-first; first-seen-wins).
    """
    head = ""
    cache_key = ""
    try:
        r = _pygit2.Repository(str(repo))
        head = str(r.head.target)
        cutoff = int(time.time()) - _CHURN_MAX_AGE_DAYS * 86400
        cutoff_day = time.strftime("%Y-%m-%d", time.localtime(cutoff))
        cache_key = f"churn-{head}-{_CHURN_MAX_COMMITS}-{cutoff_day}.json"
        cache_path = repo / ".lucidlint-cache" / cache_key
        try:
            data = json.loads(cache_path.read_text())
            return FileHistory(Counter(data["churn"]), data["last_modified"])
        except (OSError, ValueError, KeyError):  # lucidlint: ignore swallow a missing/corrupt cache just walks
            pass  # no cache yet — walk
    except Exception:  # lucidlint: ignore swallow pygit2 unavailable degrades to empty history
        head = ""  # no git — walk without caching

    churn: Counter[str] = Counter()
    last: dict[str, str] = {}
    try:
        r = _pygit2.Repository(str(repo))
        cutoff = int(time.time()) - _CHURN_MAX_AGE_DAYS * 86400
        for seen, commit in enumerate(r.walk(r.head.target, GIT_SORT_TIME)):
            if seen >= _CHURN_MAX_COMMITS or commit.commit_time < cutoff:
                break
            date = str(commit.commit_time)
            changed = set()
            if commit.parents:
                diff = commit.tree.diff_to_tree(commit.parents[0].tree)
                for patch in diff:
                    path = patch.delta.new_file.path
                    if path.endswith((".py", ".rs", ".md")) and not path.startswith((".venv/", "node_modules/")):
                        changed.add(path)
            else:
                # initial commit — diff against the empty tree gives every file
                for patch in commit.tree.diff_to_tree():
                    path = patch.delta.new_file.path
                    if path.endswith((".py", ".rs", ".md")) and not path.startswith((".venv/", "node_modules/")):
                        changed.add(path)
            for path in changed:
                churn[path] += 1
                if date and path not in last:
                    last[path] = date  # first-seen = newest in a newest-first walk
    except Exception:  # lucidlint: ignore swallow pygit2 unavailable degrades to empty history
        log(f"pygit2 history unavailable in {repo} — history-based signals are skipped")

    if cache_key:
        try:
            (repo / ".lucidlint-cache").mkdir(exist_ok=True)
            (repo / ".lucidlint-cache" / cache_key).write_text(
                json.dumps({"churn": dict(churn), "last_modified": last})
            )
        except OSError:  # lucidlint: ignore swallow the cache is best-effort — a read-only repo still works
            pass  # the cache is best-effort — a read-only repo still works
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
                except (TypeError, ValueError):  # lucidlint: ignore swallow malformed <line> elements are skipped
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
        r = _pygit2.Repository(str(repo))
        tracked = {e.path for e in r.index if e.path.endswith((".py", ".rs", ".md"))}
        # Untracked non-ignored files: walk working tree
        untracked = set()
        for root, dirs, files in os.walk(repo):
            if ".git" in dirs:
                dirs.remove(".git")
            for f in files:
                if f.endswith((".py", ".rs", ".md")):
                    full = Path(root) / f
                    rel = str(full.relative_to(repo))
                    if rel in tracked:
                        continue
                    try:
                        if not r.path_is_ignored(rel):
                            untracked.add(rel)
                    except ValueError:  # lucidlint: ignore swallow ambiguous path — treat as untracked
                        untracked.add(rel)
        rels = sorted(tracked | untracked)
        return [SourceFile(repo / rel, rel) for rel in rels]
    except Exception:
        log("pygit2 unavailable or no .git — falling back to rglob for the file list")
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


def _scanner_candidates(repo: Path, exe: str) -> list[Path]:
    """The binary locations tried in order: the CODE_HEALTH_SCANNER env var,
    the repo-local build, the tool-checkout build, then the release bundle.
    `exe` carries the platform suffix (.exe on Windows) — every candidate
    must use it, or a Windows build is silently skipped."""
    candidates: list[Path] = []
    env = os.environ.get("CODE_HEALTH_SCANNER")
    if env:
        candidates.append(Path(env))
    candidates.append(repo / "scanner" / "target" / "release" / f"lucidlint{exe}")
    candidates.append(Path(__file__).resolve().parent / "scanner" / "target" / "release" / f"lucidlint{exe}")
    bundle_dir = Path(__file__).resolve().parent / "bin"
    candidates.append(bundle_dir / f"lucidlint{exe}")
    return candidates


class _RustScan:
    """The Rust scan core — the required finding engine.

    One binary invocation per repo per run (or per file in --file / LSP mode);
    the versioned JSON findings ARE the action model (kind/severity owned by
    the core); this driver converts them to Actions verbatim and fills the
    orchestrator's fields (churn, priority, provenance)."""

    def __init__(self) -> None:
        self._cache: dict[tuple[Path, tuple[str, ...]], RustFindings | None] = {}
        self._binary_cache: dict[Path, Path | None] = {}

    def binary(self, repo: Path) -> Path | None:
        """The scan binary: env override, then the repo's own build, then the
        tool checkout's build, then the distribution bundle's sibling binary
        (a `lucidlint` release installs as <prefix>/bin/lucidlint next to
        lucidlint.py — the bundle is self-contained, no env needed).
        None when not built — the Python path takes over."""
        if repo in self._binary_cache:
            return self._binary_cache[repo]
        exe = ".exe" if os.name == "nt" else ""
        found = next((p for p in _scanner_candidates(repo, exe) if p.is_file()), None)
        self._binary_cache[repo] = found
        return found

    def load(self, repo: Path, files: list[SourceFile]) -> RustFindings | None:
        return self.load_with(repo, files)

    def load_with(
        self,
        repo: Path,
        files: list[SourceFile],
        flags: _ScanFlags | None = None,
    ) -> RustFindings | None:
        if flags is None:
            flags = self._flags()
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
                if flags.graph is not None:
                    cmd += ["--graph", str(flags.graph)]
                if flags.churn_json is not None:
                    cmd += ["--churn", str(flags.churn_json)]
                if flags.include_tests:
                    cmd.append("--include-tests")
                if flags.docs_root is not None:
                    cmd += ["--docs", flags.docs_root]
                # pass REPO-RELATIVE paths (the binary runs with the repo as
                # its cwd): findings — and the fix: directives they carry —
                # stay repo-relative, matching the Action model
                cmd += [str(sf.rel) for sf in files]
                proc = subprocess.run(cmd, capture_output=True, text=True, timeout=300, cwd=str(repo))
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
            except _SCANNER_FAILURES:  # lucidlint: ignore swallow degraded runs report nothing — visible
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
                tmp = Path(tempfile.mkstemp(prefix="lucidlint-churn-", suffix=".json")[1])
                tmp.write_text(json.dumps(dict(file_churn)), encoding="utf-8")
                self._pending_churn = tmp
            self._pending_docs = str(repo)

    def _flags(self) -> _ScanFlags:
        return _ScanFlags(self._pending_graph, self._pending_churn, self._pending_tests, self._pending_docs)

    def active(self, repo: Path) -> bool:
        """True when the Rust core is available (it is required)."""
        return self.binary(repo) is not None


def _rust_finding_rel(file_val: str, repo: Path, rels: set[str]) -> str | None:
    """The binary reports per-file findings with the path as passed and the
    repo-wide ones (duplicate/unused) with the repo-relative path — normalize."""
    if file_val in rels:
        return file_val
    try:
        # resolve BOTH sides: with --repo . (relative), a relative base makes
        # relative_to raise and the finding would be silently dropped
        rel = Path(file_val).resolve().relative_to(repo.resolve()).as_posix()
    except (ValueError, OSError):  # lucidlint: ignore swallow an unmappable path means the finding
        # is for a file outside this scan set — drop it, not a failure to surface
        rel = ""
    return rel if rel in rels else None


# Rule groups matching RULES.md — reference by name in the config file
# to suppress whole groups across the codebase.
# lucidlint: ignore record-shape wire-format dict — a class is ceremony for a rule-group map
RULE_GROUPS = {
    "architecture": {"complexity", "large-function", "closures", "partition", "strewing", "record-shape",
                     "duplicate", "layer-mix", "folder-mix", "hub-file", "high-risk",
                     "hotspot", "over-abstraction", "long-param-list", "churn-untested",
                     "detached-method"},
    "style": {"magic-number", "noop-statement", "unreachable", "vague-name", "class-module",
              "builtin-shadow", "broad-except", "swallow", "inline-import", "private-import",
              "global-state", "unused", "import-cycle", "docs-link", "docs-undiscoverable",
              "boolean-arg", "debug-artifact", "positional-literals",
              "guard-clauses", "latent-visitor", "conditional-polymorphism", "special-case",
              "middle-man", "unused-setter", "loop-pipeline"},
    "test-discipline": {"monkeypatch", "skipif", "fakefs", "no-assert-test"},
    "suppression": {"suppression", "type-ignore", "allow-reason", "noqa", "stale-suppression"},
}

# Cache for config loading
# lucidlint: ignore global-state per-repo cache of the config file — one entry per repo per run
_CONFIG_CACHE: dict[Path, dict] = {}


# lucidlint: ignore record-shape wire-format envelope for the config — a class is ceremony
def _load_lucidlint_config(repo: Path) -> dict:
    """Load the project-wide lucidlint config, looking for (in order):
    1. .lucidlint.toml in the repo root
    2. [tool.lucidlint] in pyproject.toml
    Returns a dict with 'global_ignore' (set of signal names) and
    'per_path_ignore' (list of (glob_pattern, set)) or empty defaults.
    The config lets a team suppress entire rule groups or specific signals
    without per-file suppression comments."""
    if repo in _CONFIG_CACHE and _CONFIG_CACHE.get(repo) is not None:
        return _CONFIG_CACHE[repo]

    # lucidlint: ignore record-shape wire-format envelope for the config
    result = {"global_ignore": set(), "per_path_ignore": []}

    # lucidlint: ignore record-shape wire-format envelope for the config
    def _merge_config(raw: dict) -> None:
        ignores = raw.get("ignore", raw.get("ignored_signals", []))
        for item in ignores:
            item = item.strip()
            if item.startswith("group:"):
                group_name = item[6:]
                group_signals = RULE_GROUPS.get(group_name)
                if group_signals:
                    result["global_ignore"].update(group_signals)
            else:
                result["global_ignore"].add(item)
        # Per-path overrides: keys that are glob patterns
        for key, val in raw.items():
            if key in ("ignore", "ignored_signals"):
                continue
            if isinstance(val, dict) and "ignore" in val:
                path_ignores = set()
                for item in val["ignore"]:
                    item = item.strip()
                    if item.startswith("group:"):
                        gs = RULE_GROUPS.get(item[6:])
                        if gs:
                            path_ignores.update(gs)
                    else:
                        path_ignores.add(item)
                result["per_path_ignore"].append((key, path_ignores))

    # Try .lucidlint.toml first (standalone, for Rust projects)
    toml_path = repo / ".lucidlint.toml"
    if toml_path.is_file():
        with open(toml_path, "rb") as f:
            _merge_config(tomllib.load(f).get("lucidlint", {}))
    else:
        # Fall back to pyproject.toml [tool.lucidlint]
        pyproject = repo / "pyproject.toml"
        if pyproject.is_file():
            with open(pyproject, "rb") as f:
                data = tomllib.load(f)
            tool_config = data.get("tool", {}).get("lucidlint", {})
            if tool_config:
                _merge_config(tool_config)

    _CONFIG_CACHE[repo] = result
    return result


RUST_SCAN = _RustScan()

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
    from pygit2 import GIT_SORT_TIME, GIT_SORT_TOPOLOGICAL
except ImportError:  # lucidlint: ignore swallow pygit2 is optional — degrades to gitless mode
    _pygit2 = None

CONTRACT_VERSION = 1

class _GraphContract:
    """The code-review-graph export contract, built through the tool's own
    public API (GraphStore + Registry) — the gate never touches the SQLite
    schema or the DB location, and never shells out. None = tool missing or
    no graph. The contract JSON is consumed by the Rust binary via --graph."""

    def __init__(self) -> None:
        self._cache: dict[Path, Path | None] = {}

    def contract(self, repo: Path) -> Path | None:
        if repo in self._cache:
            return self._cache[repo]
        if _GraphStore is None:
            self._cache[repo] = None
            return None
        result: Path | None = None
        try:
            data_dir = _Registry().get_data_dir_for_repo(str(repo))
            db = Path(data_dir) / "graph.db" if data_dir else repo / ".code-review-graph" / "graph.db"
            if not db.exists():
                self._cache[repo] = None
                return None
            store = _GraphStore(db)
            with store:
                community_ids = store.get_all_community_ids()
                nodes = []
                for file_path in store.get_all_files():
                    for gnode in store.get_nodes_by_file(file_path):
                        nodes.append({
                            "kind": gnode.kind,
                            "name": gnode.name,
                            "qualified_name": gnode.qualified_name,
                            "file_path": gnode.file_path,
                            "line_start": gnode.line_start,
                            "line_end": gnode.line_end,
                            "params": gnode.params,
                            "return_type": gnode.return_type,
                            "community_id": community_ids.get(gnode.qualified_name),
                        })
                edges = []
                for e in store.get_all_edges():
                    edges.append({
                        "kind": e.kind,
                        "source": e.source_qualified,
                        "target": e.target_qualified,
                        "file_path": e.file_path,
                    })
                communities = {}
                for row in store.get_communities_list():
                    communities[str(row["id"])] = row["name"]
            # lucidlint: ignore record-shape wire-format envelope — a class is ceremony for JSON
            contract = {
                "contract_version": CONTRACT_VERSION,
                "nodes": nodes,
                "edges": edges,
                "communities": communities,
            }
            tmp = Path(tempfile.mkstemp(prefix="lucidlint-graph-", suffix=".json")[1])
            tmp.write_text(json.dumps(contract, separators=(",", ":")), encoding="utf-8")
            result = tmp
        # surfaces via `result = None` — the caller falls back to non-graph
        # families; not a swallow. The direct code-review-graph API can raise
        # sqlite3.Error (corrupt/older graph.db), KeyError (rows without the
        # expected columns), or TypeError/AttributeError (version mismatch) —
        # all must degrade, not crash the scan (PRD R21)
        except (*_SCANNER_FAILURES, sqlite3.Error, KeyError, TypeError, AttributeError):
            log(f"graph contract export failed for {repo} — graph families skipped")
            result = None
        self._cache[repo] = result
        return result


GRAPH_CONTRACT = _GraphContract()
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
    p.add_argument("--file", type=str, default=None,
                   help="scan ONE repo-relative file (the LSP mode): per-file findings only, "
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


def _version() -> str:
    """The installed package version, or the dev fallback."""
    try:
        return metadata.version("lucidlint")
    except Exception:
        return "0.1.0"


_VERSION = _version()


def action_key(a: Action) -> str:
    return f"{a.kind}:{a.file}:{a.line}:{a.function}"


def changed_files(repo: Path, base: str) -> set[str]:
    """Files touched by the current branch vs base ref (best-effort)."""
    refs = [base] if base else ["origin/main", "main"]
    for ref in refs:
        try:
            r = _pygit2.Repository(str(repo))
            try:
                ref_oid = r.lookup_reference(f"refs/remotes/{ref}").target
            except KeyError:  # lucidlint: ignore swallow ref missing — fall back to the local branch
                ref_oid = r.lookup_reference(f"refs/heads/{ref}").target
            base_oid = r.merge_base(r.head.target, ref_oid)
            changed = set()
            w = r.walk(r.head.target, GIT_SORT_TOPOLOGICAL)
            w.hide(base_oid)
            for commit in w:
                if commit.parents:
                    diff = commit.tree.diff_to_tree(commit.parents[0].tree)
                    for patch in diff:
                        changed.add(patch.delta.new_file.path)
                else:
                    for entry in commit.tree:
                        changed.add(entry.name)
            if changed:
                return changed
        except Exception:
            log(f"diff against {ref} unavailable — diff awareness skipped")
            continue
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
    """Current branch and short commit for report provenance, via pygit2.
    Returns empty strings when git or pygit2 is unavailable."""
    if _pygit2 is None:
        return GitHead(branch="", commit="")
    try:
        r = _pygit2.Repository(str(repo))
        return GitHead(branch=r.head.shorthand or "", commit=str(r.head.target)[:7])
    # git-absent is a supported mode — the handler returns the empty head
    except Exception:
        return GitHead(branch="", commit="")


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
    """complexity + large-function on the same function are ONE fix; every
    other kind is per-LINE — two positional-literals calls in one function
    are two different fixes at two lines, and merging them would hide one
    finding behind the other (the fix loop would thrash)."""
    if a.kind in ("complexity", "large-function"):
        return (a.file, a.function, "fn")
    return (a.file, a.function, a.line, a.kind)


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
        except (json.JSONDecodeError, AttributeError):  # lucidlint: ignore swallow corrupt baseline; gate unbaselined
            log(f"baseline {path} unreadable — ignoring")
    return set()


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
        "\nre-run: python3 lucidlint.py --repo "
        + str(repo)
        + (" --baseline " + str(args.baseline) if args.baseline else "")
        + "   | tool lives in build-tools (github.com/ashbywinch/build-tools); thresholds and"
        + " per-action data in --json output"
    )
    print(
        "baseline: '--update-baseline --baseline lucidlint.json' acknowledges today's debt so the "
        "gate only fails on NEW actions; this report is a snapshot, not wired into CI"
    )


def _write_baseline(args, unique: list[Action]) -> int:
    """--update-baseline: lock all current action keys and exit clean."""
    if not args.baseline:
        log("--update-baseline requires --baseline PATH")
        return 2
    keys = [action_key(a) for a in unique if a.severity != "warn"]
    args.baseline.write_text(json.dumps({"actions": keys}, indent=2))
    print(f"lucidlint: baseline written — {len(keys)} action(s) locked to {args.baseline}")
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
        return BaselineIdentity(kind=parts[0], file=parts[1], function=parts[3])
    return BaselineIdentity(kind=key, file="", function="")


def main() -> int:
    args = parse_args()
    repo = args.repo.resolve()

    if args.command == "fix":
        # the agent-driven fix surface: `lucidlint fix --kind X --file F --line N`
        opts = fix_engine.FixOptions(
            params=args.params.split(",") if args.params else None,
            name=args.name,
        )
        fix_kind = fix_engine.KIND_ALIASES.get(args.kind, args.kind)
        if args.line == 0:
            # R27: agents never compute line numbers — the tool owns its own
            # coordinates; when the file has exactly one finding of the kind,
            # no --line is needed
            lines = _finding_lines(repo, args.file, args.kind)
            if len(lines) == 1:
                args.line = lines[0]
            elif not lines:
                print(f"fix: no {args.kind} finding in {args.file} — nothing to fix")
                return 0
            else:
                print(
                    f"fix: {len(lines)} {args.kind} findings in {args.file} "
                    f"(lines {', '.join(map(str, lines))}) — pass --line to pick one"
                )
                return 0
        if fix_kind in fix_engine.PREVIEW_KINDS and not args.name and not args.confirm:
            # the name-free preview surface: show the proposed refactoring
            # as a diff (the seam with a placeholder name — no --fix-name
            # needed to see it). The agent reviews the seam, then re-runs
            # with --fix-name <name>; the name is the commitment, so the
            # named run applies — no --confirm dance
            new_source, description = fix_engine.propose_finding(
                args.kind, args.file, repo, args.line, opts
            )
            if new_source is None:
                print(f"fix: nothing to change for {args.kind} at {args.file}:{args.line}")
                return 0
            diff = list(
                difflib.unified_diff(
                    (repo / args.file).read_text().splitlines(),
                    new_source.splitlines(),
                    fromfile=args.file,
                    tofile=args.file + " (proposed)",
                    lineterm="",
                )
            )
            if fix_kind == "extract-method":
                # the C shape (subtask-tested): the call-site hunk plus the
                # EXTRACTED METHOD's first lines — agents name and comprehend
                # from the method being created, not the diff head; a bare
                # diff-head truncation left them wanting the cut part and
                # unsure whether _extracted was the final name
                name = args.name or "_extracted"
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
                    (h for h in hunks
                     if any(ln.startswith("+") and f"{name}(" in ln for ln in h)),
                    hunks[0] if hunks else [],
                )
                # keep the hunk's shape, not its bulk: header + a few context
                # lines + the inserted call + the next context line — the
                # removed lines duplicate the extracted body shown below
                if call_hunk:
                    call_idx = next(
                        i for i, ln in enumerate(call_hunk)
                        if ln.startswith("+") and f"{name}(" in ln
                    )
                    head = [ln for ln in call_hunk[1:call_idx] if ln.startswith(" ")][:3]
                    tail = next((ln for ln in call_hunk[call_idx + 1:] if ln.startswith(" ")), None)
                    shown = [call_hunk[0]] + head + [call_hunk[call_idx]]
                    if tail:
                        shown.append(tail)
                    omitted = len(call_hunk) - len(shown)
                    if omitted > 0:
                        shown.append(f"... ({omitted} diff lines omitted)")
                    call_hunk = shown
                src_lines = new_source.splitlines()
                def_idx = next(
                    (i for i, ln in enumerate(src_lines)
                     if ln.startswith("def ") and name in ln),
                    None,
                )
                body: list[str] = []
                if def_idx is not None:
                    for ln in src_lines[def_idx + 1:]:
                        if ln and not ln[0].isspace():
                            break
                        body.append(ln)
                print("\n".join(diff[:2]))  # --- / +++ file headers
                print("\n".join(call_hunk))
                print(f"# seam: {description}")
                if def_idx is not None:
                    print("\nExtracted (the method being created, first lines):\n")
                    print(src_lines[def_idx])
                    print("\n".join(body[:10]))
                    if len(body) > 10:
                        print(f"... ({len(body) - 10} more lines omitted)")
            else:
                if len(diff) > 40:
                    diff = diff[:40] + [f"... ({len(diff) - 40} more lines omitted)"]
                print("\n".join(diff))
            print(f"# the name `{args.name or '_extracted'}` is a placeholder — pick a real one; "
                  f"apply it: lucidlint fix --kind {args.kind} --file {args.file} "
                  f"--line {args.line} --name <name>")
            return 0
        description = fix_engine.fix_finding(
            args.kind, args.file, repo, args.line, opts
        )
        if description is None:
            print(f"fix: nothing to change for {args.kind} at {args.file}:{args.line}")
            return 0
        print(f"fix: {description} — {args.file}:{args.line} ({args.kind})")
        return 0

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
    # Apply project-wide config (global ignore / per-path ignore)
    ignored_config = _load_lucidlint_config(repo)
    if ignored_config["global_ignore"] or ignored_config["per_path_ignore"]:
        actions = [a for a in actions if a.signal not in ignored_config["global_ignore"]]
        # Per-path removals
        for a in list(actions):
            for pattern, path_ignored in ignored_config["per_path_ignore"]:
                if Path(a.file).match(pattern) and a.signal in path_ignored:
                    actions.remove(a)
                    break
    unique = _dedupe_merge(actions, diff)

    if args.update_baseline:
        return _write_baseline(args, unique)
    stale = _apply_baseline(unique, _load_baseline(args.baseline))
    fails = [a for a in unique if a.severity == "fail"]
    warns = [a for a in unique if a.severity == "warn"]
    acks = [a for a in unique if a.severity == "ack"]
    head = _git_head(repo)
    rc = _RenderCtx(repo, args, head.branch, head.commit, cc.label, cc.graph_preferred, diff)

    if args.json:
        rc.render_json(unique)
    else:
        rc.render_text(unique, fails, warns, acks)

    return _gate_exit(stale, fails, args)


def _finding_lines(repo: Path, rel: str, kind: str) -> list[int]:
    """The lines of every finding of `kind` in one file — the R27 line
    resolution: the tool scans, the agent never counts lines."""
    found = []
    for line, k in _scan_single_file(repo, rel):
        if fix_engine.KIND_ALIASES.get(k, k) == fix_engine.KIND_ALIASES.get(kind, kind):
            found.append(line)
    return sorted(found)


def _scan_single_file(repo: Path, rel: str):
    """Scan one file, yielding (line, kind) per finding."""
    let_include_tests = False  # the fix resolves one file's findings
    RUST_SCAN.prepare(repo, rel, let_include_tests, {})
    files = _py_files(repo, rel)
    rust = RUST_SCAN.load(repo, files)
    if rust is None:
        return
    for f in rust.by_rel.get(rel, []):
        yield f.line, f.kind


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
                    signal=f.signal,
                    churn=churn,
                    last_modified=last_modified.get(rel, ""),
                    tested="",
                    raw=_raw_score(f.kind, f.metric or 1, churn),
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
            "or install the lucidlint release bundle"
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

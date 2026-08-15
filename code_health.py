#!/usr/bin/env python3
"""code_health.py — CodeScene-lite: complexity + dependency + hotspot analysis.

Replicates the *actionable* core of CodeScene on top of what we already run:
  - code-review-graph (per-repo SQLite at <repo>/.code-review-graph/graph.db)
  - radon (cyclomatic complexity) — run via `uv run --with radon`
  - git history (hotspot = high change frequency AND high complexity)

Outputs a machine-readable list of actions to address. Exit code is 1 when
any action exists, so the script doubles as a failing gate in CI/tests:

    uv run --with radon python3 code_health.py --repo /path/to/repo --json
    echo $?   # 1 when there is work to do

Thresholds are flags with sane defaults; calibrate per repo.

Philosophy: the metrics (complexity, size, edges, churn, risk) are *proxies*.
The requirement they approximate is code that is obviously correct and cheap
to change: readability, maintainability, anti-fragility. The real levers are
separation of concerns, domain language, and effective encapsulation — so
each action's message says what to do in those terms, not just what the
number is. Gaming the metric (splitting a function to lower a count without
clarifying it) is not the point; making the code easy to read and safe to
change is. A name is the cheapest form of encapsulation: a domain noun in
the type (CoverageLines, PropertyId) communicates to the next reader, where
a bare dict or tuple says nothing — this applies to legitimate maps too,
not only to records. The name must carry meaning: CoverageLines is a
domain noun, JsonDict is the smell renamed — and data crossing a boundary
(parsing/serialization) belongs to a domain class at that boundary, not a
named container.
"""

from __future__ import annotations

import argparse
import ast
import datetime
from dataclasses import asdict, dataclass, field
import json
import os
import re
import sqlite3
import subprocess
import sys
import check_records
import time
from collections import Counter, defaultdict
from pathlib import Path

EXCLUDED_DIRS = {".git", ".venv", "node_modules", "__pycache__", "dist", "build", ".mypy_cache", ".pytest_cache", ".ruff_cache"}

ACTION_KINDS = ("complexity", "large-function", "hub-file", "hotspot", "high-risk", "record-shape", "latent-class")

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
class NodeInfo:
    """Graph node facts for one function (tested, signature, span)."""

    name: str
    qualified_name: str
    file_path: str
    tested: str
    params: str
    return_type: str
    line_start: int | None
    line_end: int | None
    def_sig: str = ""


@dataclass
class Cluster:
    """One resolved callee subsystem: its name, call count, and example callees."""

    name: str
    count: int
    callees: list[str] = field(default_factory=list)


@dataclass
class Clusters:
    """Concern-seam result: resolved clusters, evidence strength, unresolved callees."""

    clusters: list[Cluster]
    strong: bool
    unresolved: list[str]


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


@dataclass
class Callers:
    """Resolved callers of a node plus the display wording for them."""

    callers: list[str]
    text: str


@dataclass
class LatentFinding:
    """One unextracted-class signal: closures or a field-disjoint partition."""

    signal: str
    function: str
    line: int
    metric: int
    detail: str
    inner: list[str]


@dataclass
class VolatilePart:
    """One complex function inside a hotspot file, with its own churn."""

    churn: int
    complexity: int
    name: str
    line: int


# Named map types: lookup tables, not records. Named by their meaning (the
# lines covered per file; groups of method names sharing fields), never as
# SomethingDict — the name is the communication.
CoverageLines = dict[str, set[int]]
MethodFields = dict[str, set[str]]
MethodGroups = list[list[str]]


# Injectable seam: tests assign code_health.radon_visitor to a fake. Loaded
# lazily so the tool runs without radon until complexity analysis is needed.
radon_visitor = None


def _radon_visitor(required: bool = True):
    global radon_visitor
    if radon_visitor is None:
        try:
            from radon.visitors import ComplexityVisitor
        except ImportError:
            if required:
                log("radon not importable — run via `uv run --with radon python3 code_health.py`")
                sys.exit(2)
            return None
        radon_visitor = ComplexityVisitor
    return radon_visitor

# Fix guidance per action kind. One sentence each: what to do, not just what's
# wrong. Tied to the real requirements (readability, maintainability,
# anti-fragility) via separation of concerns, domain language, encapsulation.
# Deliberately resists gaming the metric: splitting a function to lower a
# count without clarifying it is not a fix.
GUIDANCE = {
    "complexity": "Extract each decision branch into a named method that says what it decides in domain terms — one decision per method, happy path reads top-to-bottom. If the body is repeated similar blocks rather than distinct decisions, prefer a data table + loop over more methods. Where it mixes subsystems, extract a class per concern — for endpoints that usually means service-layer functions behind the Services DI, not new classes.",
    "large-function": "Split by responsibility into named steps that read like a procedure in the domain; one job per step, each independently testable.",
    "hub-file": "Decide what this file is first: if it is an assembly/composition root whose job is wiring (app layer, router), move handler logic out to the service layer and keep the assembly thin — the cross-module orchestration is its job, not a smell. Otherwise separate the concerns it mixes into modules with narrow, stable interfaces.",
    "hotspot": "Make the volatile part small and data-driven behind a stable interface — frequent changes become cheap and cannot disturb the stable core.",
    "high-risk": "Pin behavior with tests, then reduce the caller surface — when many things depend on it, the simplest code is the safest.",
    "latent-class": "Closures that capture state are a class in disguise — if the inner functions form behavior groups, extract a class per group and hoist the closures to its methods (the captured state becomes fields). If methods touch disjoint field sets, that partition is the latent seam: extract a class per group and let the connectors compose them. If the grouping is incidental (no shared state, no shared fields), leave it — the evidence is state and field access, not a guess.",
    "record-shape": "The record wants a class — named fields with domain meaning, so a reader sees what the data IS without tracing it (encapsulation, obvious correctness). Make a small domain class/dataclass. If the shape is genuinely a map, name it by what it MEANS (CoverageLines = dict[str, set[int]]: the lines covered per file), never as SomethingDict — a *Dict alias just renames the smell. If the data crosses a boundary (parsing or serialization), the fix is to ingest it into a domain class at that boundary: parse into the type and carry the type, don't carry the bare mapping. Constant lookup tables stay at module scope, never in an interface.",
}


def rel_path(repo: Path, p: str) -> str:
    """Graph stores absolute paths; radon/git use repo-relative. Normalize."""
    p = p.replace("\\", "/")
    root = str(repo.resolve()).replace("\\", "/") + "/"
    if p.startswith(root):
        p = p[len(root):]
    return p


def is_test_path(rel: str) -> bool:
    parts = rel.split("/")
    return (
        "tests" in parts
        or "__tests__" in parts
        or any(part.startswith("test_") or part.endswith("_test.py") for part in parts[-1:])
    )


def _graph_conn(repo: Path) -> sqlite3.Connection | None:
    db_path = _graph_db(repo)
    if db_path is None:
        return None
    conn = sqlite3.connect(db_path)
    conn.row_factory = sqlite3.Row
    return conn


def _module_key(repo: Path, file_path: str) -> str:
    """Concern label for a file: first two *directory* segments inside the repo root."""
    rel = rel_path(repo, file_path)
    parts = rel.split("/")[:-1]  # drop filename
    if not parts:
        return rel
    return "/".join(parts[:2])


def concern_clusters(conn: sqlite3.Connection, repo: Path, source_qn: str | None = None,
                     source_prefix: str | None = None, own_module: str | None = None) -> Clusters:
    """Group a function's (or file's) cross-module callees by subsystem.

    clusters always lists what resolved (even a single subsystem — the caller
    decides how to word it); strong is True when >= 2 distinct subsystems
    with >= 3 total calls — the evidence bar for claiming a real seam mix.
    Never silently empty: callers show an explicit "unresolved" marker.
    """
    if source_qn is not None:
        rows = list(conn.execute(
            "SELECT DISTINCT target_qualified FROM edges WHERE source_qualified = ? AND kind = 'CALLS'",
            (source_qn,),
        ))
    else:
        rows = list(conn.execute(
            "SELECT DISTINCT target_qualified FROM edges WHERE source_qualified LIKE ? AND kind = 'CALLS'",
            (source_prefix + "%",),
        ))
    counts: Counter[str] = Counter()
    names: dict[str, list[str]] = {}
    for r in rows:
        mod = _resolve_callee_module(conn, repo, r["target_qualified"])
        if mod and mod != own_module:
            counts[mod] += 1
            if len(names.get(mod, [])) < 3:
                names.setdefault(mod, []).append(r["target_qualified"].split("::")[-1].split(".")[-1])
    clusters = [Cluster(name=m, count=counts[m], callees=names.get(m, [])) for m, _ in counts.most_common(4)]
    strong = len(clusters) >= 2 and sum(c.count for c in clusters) >= 3
    unresolved = [r["target_qualified"].split("::")[-1].split(".")[-1] for r in rows][:6]
    return Clusters(clusters=clusters, strong=strong, unresolved=unresolved)


def mix_text(clusters: list[Cluster], strong: bool) -> str:
    """Wording for a concern mix: claim strength honestly."""
    if not clusters:
        return ""
    if strong:
        return "mixes concerns: " + fmt_clusters(clusters)
    return "possible seams (weak signal): " + fmt_clusters(clusters)


def _resolve_callee_module(conn: sqlite3.Connection, repo: Path, target: str) -> str | None:
    """Resolve a CALLS target to its defining module's cluster key (or None)."""
    row = conn.execute("SELECT file_path FROM nodes WHERE qualified_name = ?", (target,)).fetchone()
    if row is None and "::" in target:
        name = target.split("::")[-1].split(".")[-1]
        row = conn.execute("SELECT file_path FROM nodes WHERE name = ? LIMIT 1", (name,)).fetchone()
    if row is None:
        return None
    return _module_key(repo, row["file_path"])


def fmt_clusters(clusters: list[Cluster]) -> str:
    parts = []
    for cl in clusters:
        n = f" ({', '.join(cl.callees)})" if cl.callees else ""
        parts.append(f"{cl.name} ({cl.count}{n})")
    return ", ".join(parts)


def contract_text(name: str, params: str, return_type: str, def_sig: str = "") -> str:
    """Human hint for the behavior to pin with tests: the function's signature."""
    if not params and def_sig:
        return def_sig
    params = re.sub(r"\s+", " ", params or "").strip()
    if params.startswith("(") and params.endswith(")"):
        params = params[1:-1].strip()
    ret = return_type or "…"
    return f"{name}({params or '…'}) -> {ret}"


def _test_file_for(repo: Path, rel: str) -> str:
    """The repo's mirrored test file for a module (tests/unit/...), if it exists."""
    parts = rel.split("/")
    base = parts[-1][:-3] if parts[-1].endswith(".py") else parts[-1]
    dirs = parts[:-1]
    candidates = ["/".join(["tests", "unit"] + dirs + ["test_" + base + ".py"]),
                  "/".join(["tests", "unit", "test_" + base + ".py"])]
    for c in candidates:
        if (repo / c).exists():
            return c
    return ""


def _def_signature(source: str, lineno: int) -> str:
    """The function's def line(s) flattened — real signature when the graph has none."""
    lines = source.splitlines()
    if lineno < 1 or lineno > len(lines):
        return ""
    buf = lines[lineno - 1]
    depth = buf.count("(") - buf.count(")")
    i = lineno
    while depth > 0 and i < len(lines):
        i += 1
        if i - 1 >= len(lines):
            break
        buf += " " + lines[i - 1]
        depth += lines[i - 1].count("(") - lines[i - 1].count(")")
    return re.sub(r"\s+", " ", buf).strip()


def function_churn(repo: Path, rel_file: str, start: int, end: int) -> int:
    """Commits touching a line range, via git log -L. 0 on error/no history."""
    try:
        proc = subprocess.run(
            ["git", "-C", str(repo), "log", "--oneline", "-L", f"{start},{end}:{rel_file}"],
            capture_output=True, text=True, timeout=20,
        )
    except subprocess.TimeoutExpired:
        return 0
    if proc.returncode != 0:
        return 0
    return len(re.findall(r"^[0-9a-f]{7,40}", proc.stdout, flags=re.M))


def file_history(repo: Path) -> FileHistory:
    """One pass over git history: per-file change counts and last-modified date.

    Cheap even on long histories; drives the 'is this still live?' judgement
    and the priority ranking.
    """
    churn: Counter[str] = Counter()
    last: dict[str, str] = {}
    proc = subprocess.run(
        ["git", "-C", str(repo), "log", "--name-only", "--pretty=format:%ad", "--date=short"],
        capture_output=True, text=True, timeout=120,
    )
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
        if line.endswith(".py") and not line.startswith((".venv/", "node_modules/")):
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
    import xml.etree.ElementTree as ET
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
                except (TypeError, ValueError):
                    pass
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


def covered_span(covered: CoverageLines | None, rel: str, start: int, end: int) -> bool | None:
    """True/False/None: covered, uncovered, or *unknown*.

    A file absent from the coverage snapshot is UNKNOWN (None), not
    uncovered — stale snapshots predate files and would otherwise
    mislabel freshly-tested code as untested. Only a file present in the
    snapshot with no hits in the span counts as uncovered.
    """
    if covered is None:
        return None
    lines = covered.get(rel)
    if lines is None:
        return None
    return any(l in lines for l in range(start, end + 1))


def _node_info(conn: sqlite3.Connection | None, repo: Path, rel_file: str, name: str) -> NodeInfo | None:
    """Graph node facts for a function, or None when the graph has no such node."""
    if conn is None:
        return None
    abs_file = str(repo.resolve()) + "/" + rel_file
    row = conn.execute(
        "SELECT n.qualified_name, n.file_path, n.params, n.return_type, "
        "n.line_start, n.line_end, r.test_coverage "
        "FROM nodes n LEFT JOIN risk_index r ON r.node_id = n.id "
        "WHERE n.file_path = ? AND n.name = ? AND n.kind IN ('Function', 'Method') LIMIT 1",
        (abs_file, name),
    ).fetchone()
    if row is None:
        return None
    return NodeInfo(
        name=name,
        qualified_name=row["qualified_name"],
        file_path=row["file_path"],
        tested=row["test_coverage"] or "",
        params=row["params"] or "",
        return_type=row["return_type"] or "",
        line_start=row["line_start"],
        line_end=row["line_end"],
    )


def _raw_score(kind: str, metric: float, churn: int, callers: int | None = None) -> float:
    """Continuous risk score: normalized metric x churn factor (x fan-in for high-risk).

    Normalized to a 1-99 percentile ranking in main, so the list spreads
    instead of saturating at 99.
    """
    norm = {
        "latent-class": 0.7,
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


def log(msg: str) -> None:
    print(msg, file=sys.stderr)


# --------------------------------------------------------------------------- complexity (radon)
def complexity_actions(repo: Path, max_cc: int, include_tests: bool,
                       file_churn: Counter[str], last_modified: dict[str, str],
                       covered: CoverageLines | None, graph_preferred: bool, stale_note: str) -> list[Action]:
    """Cyclomatic complexity per function via radon's fast pure-Python analyzer."""
    ComplexityVisitor = _radon_visitor()
    conn = _graph_conn(repo)
    actions: list[Action] = []
    for py in sorted(repo.rglob("*.py")):
        rel = py.relative_to(repo).as_posix()
        if any(part in EXCLUDED_DIRS for part in py.parts):
            continue
        if not include_tests and ("/test" in f"/{rel}" or rel.startswith("test")):
            continue
        try:
            source = py.read_text(encoding="utf-8", errors="replace")
            visitor = ComplexityVisitor.from_code(source)
        except (SyntaxError, UnicodeDecodeError, RecursionError):
            continue
        for fn in visitor.functions:
            if fn.complexity < max_cc:
                continue
            actions.append(_complexity_action(
                repo, rel, fn, max_cc, conn, source, covered, graph_preferred, stale_note,
                file_churn, last_modified))
    return actions


def _function_mix(conn, repo: Path, rel: str, name: str, info: NodeInfo | None) -> Clusters:
    """Concern-seam result for a function's graph node, or empty when no node."""
    if info is None:
        return Clusters([], False, [])
    return concern_clusters(conn, repo, source_qn=info.qualified_name, own_module=_module_key(repo, info.file_path))


def _complexity_action(repo: Path, rel: str, fn, max_cc: int, conn, source: str,
                       covered, graph_preferred: bool, stale_note: str,
                       file_churn: Counter[str], last_modified: dict[str, str]) -> Action:
    """One complexity action: finding + seam wording + coverage note."""
    info = _node_info(conn, repo, rel, fn.name)
    mix = _function_mix(conn, repo, rel, fn.name, info)
    if info and not info.params:
        info.def_sig = _def_signature(source, fn.lineno)
    finding = f"cyclomatic complexity {fn.complexity} (>= {max_cc})"
    if mix.clusters:
        message = f"{finding} — {mix_text(mix.clusters, mix.strong)} — extract a class per concern; the seams are those subsystem boundaries, not line breaks."
    else:
        snippet = "calls: " + ", ".join(mix.unresolved) if mix.unresolved else "no cross-module callees resolved"
        message = f"{finding} — {GUIDANCE['complexity']} [concern mix unresolved — {snippet}]"
    note = coverage_note(covered, repo, rel, info, info.tested, graph_preferred, stale_note) if info else ""
    churn = file_churn.get(rel, 0)
    return Action(
        kind="complexity", severity="fail", file=rel, line=fn.lineno, function=fn.name,
        message=message, metric=fn.complexity, churn=churn,
        last_modified=last_modified.get(rel, ""),
        tested=final_tested(covered, rel, info, graph_preferred) if info else "",
        note=note, raw=_raw_score("complexity", fn.complexity, churn),
    )


def _verdict(covered: CoverageLines | None, rel: str, info: NodeInfo, graph_tested: str, graph_preferred: bool) -> str:
    """Single coverage verdict: 'tested', 'untested', or 'unknown'.

    One source of truth for both the JSON field and the prose note, so they
    can never contradict. Stale snapshot (graph_preferred): trust the graph's
    TESTED_BY, but a hit in even the stale snapshot is still evidence of
    coverage. Fresh data: the snapshot decides.
    """
    start = info.line_start or 1
    cov = covered_span(covered, rel, start, info.line_end or start)
    if graph_preferred:
        # Stale snapshot: only 'tested' is provable. Anything else is UNKNOWN —
        # the snapshot may predate the tests and the graph is blind to HTTP-path
        # and in-body-import tests, so a hard 'untested' would be a false
        # "write the failing tests first" imperative.
        if graph_tested == "tested" or cov is True:
            return "tested"
        return "unknown"
    if cov is not None:
        return "tested" if cov else "untested"
    return graph_tested or "unknown"


def final_tested(covered: CoverageLines | None, rel: str, info: NodeInfo, graph_preferred: bool = False) -> str:
    return _verdict(covered, rel, info, info.tested, graph_preferred)


def coverage_note(covered: CoverageLines | None, repo: Path, rel: str, info: NodeInfo, graph_tested: str,
                  graph_preferred: bool = False, stale_note: str = "") -> str:
    """Append a coverage-based instruction when the function is untested."""
    contract = contract_text(info.name, info.params, info.return_type, info.def_sig)
    tfile = _test_file_for(repo, rel)
    extend = f" Extend {tfile}." if tfile else ""
    verdict = _verdict(covered, rel, info, graph_tested, graph_preferred)
    if verdict == "tested":
        return ""
    if verdict == "untested":
        return f" Not covered by the repo's coverage data — write the failing tests first. Contract to pin: {contract}.{extend}"
    # unknown: stale snapshot and graph blind to it — verify, never assert
    return (f" Coverage snapshot is older than the repo's tests and the graph's TESTED_BY edges "
            f"don't reach this function (in-body imports and HTTP-path tests are invisible to it) — "
            f"verify with make coverage / htmlcov; if truly uncovered, pin "
            f"{contract} with tests first.{extend}")


# --------------------------------------------------------------------------- graph (code-review-graph)
def _graph_db(repo: Path) -> Path | None:
    db = repo / ".code-review-graph" / "graph.db"
    return db if db.exists() else None


def graph_actions(repo: Path, max_fn_lines: int, max_file_edges: int, max_risk: float, include_tests: bool,
                  file_churn: Counter[str], last_modified: dict[str, str],
                  covered: CoverageLines | None, graph_preferred: bool, stale_note: str) -> list[Action]:
    """Repo-structure actions from the code-review-graph SQLite: large functions, hub files, high risk."""
    db_path = _graph_db(repo)
    if db_path is None:
        log(f"no graph at {repo / '.code-review-graph' / 'graph.db'} — run `code-review-graph build --repo {repo}` first")
        return []
    db = sqlite3.connect(db_path)
    db.row_factory = sqlite3.Row
    actions: list[dict] = []
    actions += _large_function_actions(db, repo, max_fn_lines, include_tests, file_churn, last_modified, covered, graph_preferred, stale_note)
    actions += _hub_file_actions(db, repo, max_file_edges, include_tests, file_churn, last_modified, covered, graph_preferred, stale_note)
    actions += _high_risk_actions(db, repo, max_risk, include_tests, file_churn, last_modified, covered, graph_preferred, stale_note)
    db.close()
    return actions


def _read_source(src_cache: dict[str, str], repo: Path, rel: str) -> str:
    """Cached file source for def-signature and fattest-handler extraction."""
    if rel not in src_cache:
        try:
            src_cache[rel] = (repo / rel).read_text(encoding="utf-8", errors="replace")
        except OSError:
            src_cache[rel] = ""
    return src_cache[rel]


def _mix_message(finding: str, mix: Clusters, guidance: str, seams: str) -> str:
    """Finding + concern-seam wording, or the guidance with an honest unresolved marker."""
    if mix.clusters:
        return f"{finding} — {mix_text(mix.clusters, mix.strong)} — {seams}"
    snippet = "calls: " + ", ".join(mix.unresolved) if mix.unresolved else "no cross-module callees resolved"
    return f"{finding} — {guidance} [concern mix unresolved — {snippet}]"


def _info_signature(info: NodeInfo, src_cache: dict[str, str], repo: Path, rel: str, line: int) -> NodeInfo:
    """Attach the real def signature when the graph has no params for the function."""
    if not info.params:
        info.def_sig = _def_signature(_read_source(src_cache, repo, rel), line)
    return info


def _large_function_actions(db, repo: Path, max_fn_lines: int, include_tests: bool,
                             file_churn: Counter[str], last_modified: dict[str, str],
                             covered, graph_preferred: bool, stale_note: str) -> list[Action]:
    """Functions whose node line span exceeds the threshold (non-test, Python)."""
    actions: list[Action] = []
    src_cache: dict[str, str] = {}
    test_filter = "" if include_tests else "AND kind != 'Test'"
    for row in db.execute(
        f"""
        SELECT n.name, n.qualified_name, n.file_path, n.line_start, n.line_end, n.kind,
               n.params, n.return_type, r.test_coverage
        FROM nodes n LEFT JOIN risk_index r ON r.node_id = n.id
        WHERE n.kind IN ('Function', 'Method') {test_filter}
          AND n.file_path LIKE '%.py'
          AND n.line_start IS NOT NULL AND n.line_end IS NOT NULL
          AND n.line_end - n.line_start >= ?
        ORDER BY (n.line_end - n.line_start) DESC
        """,
        (max_fn_lines,),
    ):
        span = row["line_end"] - row["line_start"] + 1
        rel = rel_path(repo, row["file_path"])
        if not include_tests and is_test_path(rel):
            continue
        mix = concern_clusters(db, repo, source_qn=row["qualified_name"], own_module=_module_key(repo, row["file_path"]))
        message = _mix_message(
            f"function spans {span} lines (>= {max_fn_lines})", mix,
            GUIDANCE["large-function"],
            "extract a class per concern, then split each class's methods into named domain steps.")
        info = _info_signature(
            NodeInfo(name=row["name"], qualified_name=row["qualified_name"], file_path=row["file_path"],
                     tested=row["test_coverage"] or "", params=row["params"] or "", return_type=row["return_type"] or "",
                     line_start=row["line_start"], line_end=row["line_end"]),
            src_cache, repo, rel, row["line_start"])
        note = coverage_note(covered, repo, rel, info, row["test_coverage"] or "", graph_preferred, stale_note)
        churn = file_churn.get(rel, 0)
        actions.append(Action(
            kind="large-function", severity="fail", file=rel, line=row["line_start"],
            function=row["name"], message=message, metric=span, churn=churn,
            last_modified=last_modified.get(rel, ""), tested=final_tested(covered, rel, info),
            note=note, raw=_raw_score("large-function", span, churn),
        ))
    return actions


def _hub_file_actions(db, repo: Path, max_file_edges: int, include_tests: bool,
                      file_churn: Counter[str], last_modified: dict[str, str],
                      covered, graph_preferred: bool, stale_note: str) -> list[Action]:
    """Files with heavy coupling (CALLS/IMPORTS_FROM/INHERITS/REFERENCES, no test-harness edges)."""
    actions: list[Action] = []
    src_cache: dict[str, str] = {}
    hub_kinds = ("CALLS", "IMPORTS_FROM", "INHERITS", "REFERENCES")
    for row in db.execute(
        f"""
        SELECT file_path, COUNT(*) AS edge_count
        FROM edges
        WHERE kind IN {hub_kinds}
          AND file_path LIKE '%.py'
        GROUP BY file_path
        HAVING edge_count >= ?
        ORDER BY edge_count DESC
        """,
        (max_file_edges,),
    ):
        rel = rel_path(repo, row["file_path"])
        if not include_tests and is_test_path(rel):
            continue
        abs_file = str(repo.resolve()) + "/" + rel
        mix = concern_clusters(db, repo, source_prefix=abs_file + "::", own_module=_module_key(repo, row["file_path"]))
        message = _mix_message(
            f"{row['edge_count']} call/import edges (>= {max_file_edges})", mix,
            GUIDANCE["hub-file"],
            "split into one module per concern with narrow, stable interfaces so changes stay contained.")
        # Point at the file's fattest handlers: top-3 by cyclomatic complexity.
        first = db.execute(
            "SELECT MIN(line_start) FROM nodes WHERE file_path = ? AND kind IN ('Function', 'Method')",
            (row["file_path"],),
        ).fetchone()[0]
        fat = ""
        try:
            Visitor = _radon_visitor(required=False)
            if Visitor is not None:
                source = _read_source(src_cache, repo, rel)
                fns = Visitor.from_code(source).functions if source else []
                top = sorted(fns, key=lambda f: f.complexity, reverse=True)[:3]
                if top:
                    fat = " fattest: " + ", ".join(f"{f.name}:{f.lineno} (CC {f.complexity})" for f in top)
                    anchor = top[0].lineno
        except Exception:
            pass
        message += fat
        churn = file_churn.get(rel, 0)
        actions.append(Action(
            kind="hub-file", severity="fail", file=rel, line=anchor if fat else (first or 1),
            function="", message=message, metric=row["edge_count"], churn=churn,
            last_modified=last_modified.get(rel, ""), tested="",
            raw=_raw_score("hub-file", row["edge_count"], churn),
        ))
    return actions


def _callers_text(db, row) -> Callers:
    """Distinct callers of a node from CALLS edges (qualified and bare-name targets)."""
    callers = [r[0].split("::")[-1] for r in db.execute(
        "SELECT DISTINCT source_qualified FROM edges WHERE kind = 'CALLS' "
        "AND (target_qualified = ? OR target_qualified = ? OR target_qualified LIKE ?)",
        (row["qualified_name"], row["name"], "%::" + row["name"]),
    )][:8]
    if not callers:
        return Callers([], "")
    text = f", callers: {', '.join(callers)}"
    if len(callers) < row["caller_count"]:
        text += f" ({len(callers)} distinct of {row['caller_count']} call sites per risk index — count includes repeated call sites)"
    return Callers(callers, text)


def _high_risk_actions(db, repo: Path, max_risk: float, include_tests: bool,
                       file_churn: Counter[str], last_modified: dict[str, str],
                       covered, graph_preferred: bool, stale_note: str) -> list[Action]:
    """Graph risk-index nodes above the threshold, with resolved callers."""
    actions: list[Action] = []
    src_cache: dict[str, str] = {}
    for row in db.execute(
        """
        SELECT n.file_path, n.line_start, n.line_end, n.name, n.qualified_name,
               n.params, n.return_type, r.risk_score, r.caller_count, r.test_coverage
        FROM risk_index r JOIN nodes n ON n.id = r.node_id
        WHERE r.risk_score >= ? AND n.file_path LIKE '%.py' {extra}
        ORDER BY r.risk_score DESC
        LIMIT 50
        """.format(extra="" if include_tests else "AND n.kind != 'Test'"),
        (max_risk,),
    ):
        action = _high_risk_action(db, repo, row, max_risk, include_tests, src_cache,
                                   file_churn, last_modified, covered, graph_preferred, stale_note)
        if action:
            actions.append(action)
    return actions


def _high_risk_action(db, repo: Path, row, max_risk: float, include_tests: bool, src_cache: dict[str, str],
                      file_churn: Counter[str], last_modified: dict[str, str],
                      covered, graph_preferred: bool, stale_note: str) -> Action | None:
    """One high-risk action, or None for test files."""
    rel = rel_path(repo, row["file_path"])
    if not include_tests and is_test_path(rel):
        return None
    resolved = _callers_text(db, row)
    callers = resolved.callers
    message = (
        f"graph risk {row['risk_score']:.2f} (>= {max_risk}), {len(callers) or row['caller_count']} call site(s){resolved.text} — "
        f"{GUIDANCE['high-risk']}"
    )
    info = _info_signature(
        NodeInfo(name=row["name"], qualified_name=row["qualified_name"], file_path=row["file_path"],
                 tested=row["test_coverage"] or "", params=row["params"] or "", return_type=row["return_type"] or "",
                 line_start=row["line_start"] or 1, line_end=row["line_end"] or row["line_start"] or 1),
        src_cache, repo, rel, row["line_start"] or 1)
    note = coverage_note(covered, repo, rel, info, row["test_coverage"] or "", graph_preferred, stale_note)
    churn = file_churn.get(rel, 0)
    return Action(
        kind="high-risk", severity="fail", file=rel, line=row["line_start"] or 1,
        function=row["name"], message=message, metric=round(row["risk_score"], 2), churn=churn,
        last_modified=last_modified.get(rel, ""), tested=final_tested(covered, rel, info),
        callers=callers, note=note,
        raw=_raw_score("high-risk", row["risk_score"], churn, len(callers) if callers else row["caller_count"]),
    )


# --------------------------------------------------------------------------- hotspots (git history x complexity)
def _volatile_parts(conn, repo: Path, rel: str, fns, min_cc: float) -> list[VolatilePart]:
    """Complex functions in a hotspot file with their own churn (git log -L)."""
    volatile: list[VolatilePart] = []
    if conn is None:
        return volatile
    abs_file = str(repo.resolve()) + "/" + rel
    nodes = {r["name"]: r for r in conn.execute(
        "SELECT name, line_start, line_end FROM nodes WHERE file_path = ? "
        "AND kind IN ('Function', 'Method')",
        (abs_file,),
    )}
    for fn in fns:
        if fn.complexity < min_cc:
            continue
        node = nodes.get(fn.name)
        if node is None or node["line_start"] is None or node["line_end"] is None:
            continue
        churn = function_churn(repo, rel, node["line_start"], node["line_end"])
        volatile.append(VolatilePart(churn=churn, complexity=fn.complexity, name=fn.name, line=node["line_start"]))
    return volatile


def hotspot_actions(repo: Path, top_frac: float, min_cc: float,
                    file_churn: Counter[str], last_modified: dict[str, str]) -> list[Action]:
    """CodeScene hotspot: files that change often AND are complex.

    Change frequency from the shared `git log --name-only` pass; complexity =
    max cyclomatic complexity of the file's functions (radon). Max, not mean:
    mean dilutes when a file mixes many small functions with one monster —
    CodeScene's hotspot signal is the concentration of complexity in a
    frequently-changed file.
    """
    ComplexityVisitor = _radon_visitor()
    cutoff = max(1, int(len(file_churn) * top_frac))
    hottest = {f for f, c in file_churn.most_common(cutoff) if c >= 2}

    actions: list[dict] = []
    conn = _graph_conn(repo)
    for rel in sorted(hottest):
        fpath = repo / rel
        if not fpath.exists():
            continue
        try:
            source = fpath.read_text(encoding="utf-8", errors="replace")
            visitor = ComplexityVisitor.from_code(source)
        except (SyntaxError, UnicodeDecodeError):
            continue
        fns = visitor.functions
        if not fns:
            continue
        max_cc = max(f.complexity for f in fns)
        if max_cc < min_cc:
            continue
        # Name the volatile part: complex functions in this file, with their own
        # churn (git log -L over the graph's line range). Cap at 3 per file.
        volatile = sorted(_volatile_parts(conn, repo, rel, fns, min_cc),
                           key=lambda v: (v.churn, v.complexity), reverse=True)
        parts = []
        for v in volatile[:3]:
            parts.append(f"{v.name}:{v.line} (CC {v.complexity}" + (f", {v.churn}x churn)" if v.churn else ")"))
        if not parts:
            parts = [f"max CC {max_cc} in {rel}"]
        churn = file_churn.get(rel, 0)
        actions.append(
            Action(
                kind="hotspot",
                severity="fail",
                file=rel,
                line=1,
                function="",
                message=f"changed {churn}x (top {top_frac:.0%} by churn) — volatile part: "
                f"{', '.join(parts)} — {GUIDANCE['hotspot']}",
                metric=max_cc,
                churn=churn,
                last_modified=last_modified.get(rel, ""),
                tested="",
                raw=_raw_score("hotspot", max_cc, churn),
            )
        )
    return actions


# --------------------------------------------------------------------------- main
def parse_args() -> argparse.Namespace:
    p = argparse.ArgumentParser(description="CodeScene-lite: complexity/dependency/hotspot actions from code-review-graph + radon + git")
    p.add_argument("--repo", type=Path, default=Path.cwd(), help="repository root (default: cwd)")
    p.add_argument("--max-complexity", type=int, default=15, help="fail functions with cyclomatic complexity >= N (default 15)")
    p.add_argument("--max-function-lines", type=int, default=120, help="fail functions spanning >= N lines (default 120)")
    p.add_argument("--max-file-edges", type=int, default=150, help="fail files with >= N call/import edges (default 150)")
    p.add_argument("--max-risk", type=float, default=0.8, help="fail nodes with graph risk score >= N (default 0.8)")
    p.add_argument("--hotspot-top-frac", type=float, default=0.1, help="hotspot candidate set: top fraction of files by change count (default 0.1)")
    p.add_argument("--hotspot-min-cc", type=float, default=15.0, help="hotspot requires file max complexity >= N (default 15)")
    p.add_argument("--include-tests", action="store_true", help="also analyze test files/nodes")
    p.add_argument("--baseline", type=Path, default=None, help="baseline JSON of acknowledged actions; listed actions are reported but do not fail the gate")
    p.add_argument("--update-baseline", action="store_true", help="write all current action keys to --baseline and exit 0 (lock the list, like pyrefly baselines)")
    p.add_argument("--base", type=str, default="", help="git ref to diff against; actions in files your branch changed are marked 'in your diff' (default: origin/main, then main)")
    p.add_argument("--json", action="store_true", help="emit actions as JSON object (meta + actions) on stdout")
    p.add_argument("--refresh-coverage", action="store_true", help="run the repo's coverage suite (make coverage) before scanning so coverage verdicts are fresh (slow)")
    p.add_argument("--warn", action="store_true", help="exit 0 even when actions exist (informational run)")
    return p.parse_args()


def action_key(a: Action) -> str:
    return f"{a.kind}:{a.file}:{a.line}:{a.function}"


def changed_files(repo: Path, base: str) -> set[str]:
    """Files touched by the current branch vs base ref (best-effort)."""
    refs = [base] if base else ["origin/main", "main"]
    for ref in refs:
        proc = subprocess.run(
            ["git", "-C", str(repo), "diff", "--name-only", f"{ref}...HEAD"],
            capture_output=True, text=True, timeout=30,
        )
        if proc.returncode == 0 and proc.stdout.strip():
            return {ln.strip() for ln in proc.stdout.splitlines() if ln.strip()}
    return set()


def _coverage_context(repo: Path, covered, coverage_source: str) -> CoverageContext:
    """Coverage provenance label + staleness verdict."""
    if coverage_source == ".coverage" and (repo / ".coverage").exists():
        coverage_source += " (mtime " + time.strftime("%Y-%m-%d %H:%M", time.localtime((repo / ".coverage").stat().st_mtime)) + ")"
    graph_preferred = False
    stale_note = ""
    if covered is not None and (repo / ".coverage").exists():
        cov_mtime = (repo / ".coverage").stat().st_mtime
        newest_test = max((p.stat().st_mtime for p in (repo / "tests").rglob("*.py")), default=0.0)
        if newest_test > cov_mtime:
            graph_preferred = True
            stale_note = " (coverage snapshot older than the repo's tests — graph verdict used; verify against htmlcov/ if present)"
    return CoverageContext(label=coverage_source, graph_preferred=graph_preferred, stale_note=stale_note)


def _git_head(repo: Path) -> GitHead:
    """Current branch and short commit for report provenance."""
    branch = subprocess.run(["git", "-C", str(repo), "branch", "--show-current"],
                            capture_output=True, text=True).stdout.strip()
    commit = subprocess.run(["git", "-C", str(repo), "rev-parse", "--short", "HEAD"],
                            capture_output=True, text=True).stdout.strip()
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
    return list(merged.values())


def _lifecycle_notes(unique: list[Action]) -> None:
    """Facts only — low-churn scripts/tools. Delete-vs-refactor is the agent's call."""
    for a in unique:
        if a.file.startswith(("scripts/", "tools/")) and a.churn <= 2 and a.last_modified:
            a.note = (a.note + f" Lifecycle: {a.churn}x churn, last touched {a.last_modified} — "
                      f"low-change file under scripts/tools.").strip()


def _dedupe_merge(actions: list[Action], diff: set[str]) -> list[Action]:
    """Dedupe, rank, merge per-target kinds, then lifecycle notes."""
    unique = _dedupe(actions)
    _percentile_rank(unique, diff)
    unique = _merge_targets(unique)
    _percentile_rank(unique, set())
    unique.sort(key=lambda a: (-a.priority, a.file, a.line))
    _lifecycle_notes(unique)
    return unique


def _load_baseline(path) -> set[str]:
    """Acknowledged action keys from the baseline file (best-effort)."""
    if path and path.exists():
        try:
            return set(json.loads(path.read_text()).get("actions", []))
        except (json.JSONDecodeError, AttributeError):
            log(f"baseline {path} unreadable — ignoring")
    return set()


def _render_json(repo: Path, args, unique: list[Action], branch: str, commit: str, coverage_source: str) -> None:
    print(json.dumps({
        "meta": {
            "repo": str(repo), "branch": branch, "commit": commit,
            "generated_at": datetime.date.today().isoformat(),
            "base_ref": args.base or "origin/main|main",
            "coverage_source": coverage_source,
            "thresholds": {
                "max_complexity": args.max_complexity,
                "max_function_lines": args.max_function_lines,
                "max_file_edges": args.max_file_edges,
                "max_risk": args.max_risk,
                "hotspot_top_frac": args.hotspot_top_frac,
                "hotspot_min_cc": args.hotspot_min_cc,
            },
        },
        "baseline": str(args.baseline) if args.baseline else "",
        "actions": [asdict(a) for a in unique],
    }, indent=2))


def _render_summary(repo: Path, args, fails: list[Action], acks: list[Action],
                    diff: set[str], coverage_source: str, graph_preferred: bool) -> None:
    """Gate verdict, scope, and formula lines."""
    top = fails[0]
    mine = sum(1 for a in fails if a.in_diff)
    mine_txt = (f"; {mine} of {len(fails)} actions in files your diff touches"
                if diff else "; diff base unresolved")
    mine_txt += " (no baseline — cannot tell what is new)"
    targets = len({(a.file, a.function) for a in fails})
    verdict = "GATE: FAIL" if not args.warn else "GATE: INFORMATIONAL (--warn)"
    print(f"{verdict} — {len(fails)} action(s) across {targets} distinct targets "
          f"(+{len(acks)} acknowledged in baseline){mine_txt}, "
          f"top P{top.priority} {top.file}:{top.line} ({top.function or top.kind})")
    print("priority ranks change-cost (churn x fan-in), not brokenness — which item is worth fixing first is a judgement call; "
          "the hotspot entries are the usual starting set")
    if graph_preferred:
        print("WARNING: coverage snapshot predates the repo's tests — hard 'untested' claims are suppressed; "
              "run --refresh-coverage (make coverage) for definite test-status verdicts")
    print("priority = percentile of raw risk (metric norm x (1 + churn/30) x (1 + callers/5)); "
          "norms: CC/40, lines/200, edges/400, risk/1 (norm capped at 1.0, churn factor at 1.5, callers factor at 1.0) "
          "— the displayed thresholds are the fail bars, not the norms; "
          "thresholds: CC>=" + str(args.max_complexity) + ", fn>=" + str(args.max_function_lines) +
          " lines, file>=" + str(args.max_file_edges) + " edges, risk>=" + str(args.max_risk) +
          ", hotspot top " + f"{args.hotspot_top_frac:.0%}" + " by churn with CC>=" + str(args.hotspot_min_cc) +
          f"; coverage: {coverage_source}")


def _render_file_group(file: str, items: list[Action]) -> None:
    """One file's actions, priority-ordered, with notes."""
    touched = " [in your diff]" if any(i.in_diff for i in items) else ""
    print(f"\n{file}{touched}")
    for a in items:
        loc = f":{a.line}" + (f" ({a.function})" if a.function else "")
        churn = f" [churn {a.churn}x]" if a.churn else ""
        kinds = ",".join(a.kinds) if a.kinds else a.kind
        print(f"  [P{a.priority:02d}][{kinds}] {loc}{churn} — {a.message}")
        if a.note:
            print(f"      -> {a.note}")


def _render_actions(repo: Path, args, fails: list[Action], acks: list[Action]) -> None:
    """Per-file grouped action lines, baseline acknowledgements, and the footer."""
    by_file: dict[str, list[Action]] = {}
    for a in fails:
        by_file.setdefault(a.file, []).append(a)
    for file, items in sorted(by_file.items(), key=lambda kv: -max(i.priority for i in kv[1])):
        _render_file_group(file, items)
    if acks:
        print(f"\nacknowledged in baseline ({len(acks)}): " + ", ".join(f"{a.file}:{a.line}" for a in acks[:5]) + (" …" if len(acks) > 5 else ""))
    print("\nre-run: uv run --with radon python3 code_health.py --repo " + str(repo) +
          (" --baseline " + str(args.baseline) if args.baseline else "") +
          "   | tool lives in build-tools (github.com/ashbywinch/build-tools); thresholds and per-action data in --json output")
    print("baseline: '--update-baseline --baseline code-health.json' acknowledges today's debt so the "
          "gate only fails on NEW actions; this report is a snapshot, not wired into CI")


def _render_text(repo: Path, args, unique: list[Action], fails: list[Action], acks: list[Action],
                 diff: set[str], coverage_source: str, graph_preferred: bool) -> None:
    if not unique:
        print("GATE: PASS — clean, no actions")
        return
    if not fails:
        print(f"GATE: PASS — {len(acks)} action(s), all acknowledged in baseline")
        return
    _render_summary(repo, args, fails, acks, diff, coverage_source, graph_preferred)
    _render_actions(repo, args, fails, acks)


def _latent_class_actions(repo: Path, include_tests: bool,
                         file_churn: Counter[str], last_modified: dict[str, str]) -> list[Action]:
    """Fat classes/functions carrying unextracted classes inside them.

    Two factual signals, both gated so the finding is a plausible fix item:
    - nested closures: a method/function with >= 2 inner function definitions
      (closures capturing state = a class in disguise) AND complexity >= 15
      or a >= 60-line span;
    - field partition: a class with >= 6 methods and >= 150 lines whose
      methods split, after removing up to 2 connector methods, into >= 2
      connected groups (by shared self.<attr> access) of >= 2 methods each
      touching >= 2 distinct fields — the partition is the latent seam.
    Guidance is conditional: the evidence is stated, the interpretation is
    offered, coincidental grouping is left alone.
    """
    try:
        Visitor = _radon_visitor(required=False)
    except SystemExit:
        Visitor = None
    actions: list[Action] = []
    for py in sorted(repo.rglob("*.py")):
        rel = py.relative_to(repo).as_posix()
        if any(part in EXCLUDED_DIRS for part in py.parts):
            continue
        if not include_tests and ("/test" in f"/{rel}" or rel.startswith("test")):
            continue
        try:
            source = py.read_text(encoding="utf-8", errors="replace")
            tree = ast.parse(source)
        except (SyntaxError, UnicodeDecodeError):
            continue
        fn_map = {}
        if Visitor is not None:
            for f in Visitor.from_code(source).functions:
                fn_map[(f.name, f.lineno)] = f.complexity
        for finding in _closure_findings(tree, rel, fn_map):
            actions.append(_latent_action(repo, rel, finding, file_churn, last_modified))
        for finding in _partition_findings(tree, rel):
            actions.append(_latent_action(repo, rel, finding, file_churn, last_modified))
    return actions


def _closure_findings(tree: ast.Module, rel: str, fn_map: dict[tuple[str, int], int]) -> list[LatentFinding]:
    """Functions/methods with >= 2 inner function defs and size/complexity to match."""
    findings: list[LatentFinding] = []
    for fn in [n for n in ast.walk(tree) if isinstance(n, (ast.FunctionDef, ast.AsyncFunctionDef))]:
        span = (fn.end_lineno or fn.lineno) - fn.lineno
        cc = fn_map.get((fn.name, fn.lineno), 0)
        if cc < 15 and span < 60:
            continue
        inner = [n.name for n in ast.walk(fn)
                 if n is not fn and isinstance(n, (ast.FunctionDef, ast.AsyncFunctionDef))]
        lambdas = sum(1 for n in ast.walk(fn) if isinstance(n, ast.Lambda))
        if len(inner) + lambdas < 2:
            continue
        findings.append(LatentFinding(
            signal="closures", function=fn.name, line=fn.lineno,
            metric=len(inner) + lambdas,
            detail=_closure_detail(inner, lambdas, cc, span),
            inner=inner[:6],
        ))
    return findings


def _closure_detail(inner: list[str], lambdas: int, cc: int, span: int) -> str:
    """Wording: how many inner functions (named), and why the size gate fired."""
    count = len(inner) + lambdas
    names = ", ".join(inner) if inner else ""
    names = f" — {names}" if names else ""
    reason = "closing over its state — a class in disguise" if cc >= 15 else f"in a {span}-line body"
    lambda_note = f" including {lambdas} lambda(s)" if lambdas else ""
    return f"defines {count} inner function(s){names}{lambda_note} ({reason})"


def _partition_findings(tree: ast.Module, rel: str) -> list[LatentFinding]:
    """Classes whose methods split into field-disjoint groups (latent classes)."""
    findings: list[LatentFinding] = []
    for cls in [n for n in tree.body if isinstance(n, ast.ClassDef)]:
        finding = _partition_for_class(cls)
        if finding is not None:
            findings.append(finding)
    return findings


def _partition_for_class(cls: ast.ClassDef) -> LatentFinding | None:
    """One class: the smallest connector removal exposing >= 2 field-disjoint method groups."""
    methods = [n for n in cls.body if isinstance(n, (ast.FunctionDef, ast.AsyncFunctionDef))]
    if len(methods) < 6 or (cls.end_lineno or cls.lineno) - cls.lineno < 150:
        return None
    mf = {}
    for m in methods:
        mf[m.name] = {node.attr for node in ast.walk(m)
                      if isinstance(node, ast.Attribute) and isinstance(node.value, ast.Name) and node.value.id == "self"}
    partition = _find_partition(list(mf), mf)
    if partition is None:
        return None
    connectors, groups = partition
    return LatentFinding(
        signal="partition", function=cls.name, line=cls.lineno,
        metric=sum(len(g) for g in groups),
        detail=_partition_detail(connectors, groups),
        inner=[],
    )


def _partition_detail(connectors: list[str], groups: MethodGroups) -> str:
    """Wording: the field-disjoint groups and which connectors were removed."""
    groups_text = "/".join("{" + ",".join(g) + "}" for g in groups)
    conn_text = "{" + ",".join(connectors) + "}" if connectors else "none"
    return (f"methods split into {len(groups)} field-disjoint groups ({groups_text}), "
            f"connectors removed: {conn_text} — each group touches only its own fields, "
            f"so each is a latent class")


def _find_partition(names: list[str], mf: MethodFields):
    """Smallest connector removal that splits the shared-field graph into
    >= 2 groups of >= 2 methods each touching >= 2 distinct fields."""
    from itertools import combinations
    for removal in range(3):
        for removed in combinations(names, removal):
            kept = [n for n in names if n not in removed]
            groups = _connected_groups(kept, mf)
            big = [g for g in groups if len(g) >= 2 and len({f for m in g for f in mf[m]}) >= 2]
            if len(big) >= 2:
                return list(removed), big
    return None


def _connected_groups(kept: list[str], mf: MethodFields) -> MethodGroups:
    """Methods connected by sharing at least one field."""
    groups: list[list[str]] = []
    seen: set[str] = set()
    for start in kept:
        if start in seen:
            continue
        group = []
        stack = [start]
        while stack:
            m = stack.pop()
            if m in seen:
                continue
            seen.add(m)
            group.append(m)
            for other in kept:
                if other not in seen and mf[m] & mf[other]:
                    stack.append(other)
        groups.append(group)
    return groups


def _latent_action(repo: Path, rel: str, finding: LatentFinding,
                   file_churn: Counter[str], last_modified: dict[str, str]) -> Action:
    churn = file_churn.get(rel, 0)
    return Action(
        kind="latent-class", severity="fail", file=rel, line=finding.line, function=finding.function,
        message=f"{finding.detail} — {GUIDANCE['latent-class']}",
        metric=finding.metric, churn=churn,
        last_modified=last_modified.get(rel, ""), tested="",
        raw=_raw_score("latent-class", finding.metric, churn),
    )


def _record_actions(repo: Path, include_tests: bool,
                    file_churn: Counter[str], last_modified: dict[str, str]) -> list[Action]:
    """Record-shaped collections (bare dicts/tuples as records) via check_records."""
    actions: list[Action] = []
    for finding in check_records.scan([repo]).findings:
        rel = rel_path(repo, finding.split(":", 1)[0])
        if not include_tests and is_test_path(rel):
            continue
        line = 1
        m = re.search(r"\(line (\d+)\)", finding)
        if m:
            line = int(m.group(1))
        fn = ""
        m = re.search(r"of (\w+) \(line", finding)
        if m:
            fn = m.group(1)
        body = finding.split(": ", 1)[1] if ": " in finding else finding
        churn = file_churn.get(rel, 0)
        actions.append(Action(
            kind="record-shape", severity="fail", file=rel, line=line, function=fn,
            message=f"{body} — {GUIDANCE['record-shape']}", metric=1, churn=churn,
            last_modified=last_modified.get(rel, ""), tested="",
            raw=_raw_score("record-shape", 1, churn),
        ))
    return actions


def _collect_actions(repo: Path, args, file_churn, last_modified, covered, graph_preferred: bool, stale_note: str) -> list[Action]:
    actions: list[Action] = []
    actions += complexity_actions(repo, args.max_complexity, args.include_tests, file_churn, last_modified, covered, graph_preferred, stale_note)
    actions += graph_actions(repo, args.max_function_lines, args.max_file_edges, args.max_risk, args.include_tests, file_churn, last_modified, covered, graph_preferred, stale_note)
    actions += hotspot_actions(repo, args.hotspot_top_frac, args.hotspot_min_cc, file_churn, last_modified)
    actions += _record_actions(repo, args.include_tests, file_churn, last_modified)
    actions += _latent_class_actions(repo, args.include_tests, file_churn, last_modified)
    return actions


def _refresh_coverage(repo: Path) -> None:
    """Regenerate the repo's own coverage data so verdicts are fresh (slow)."""
    log("refreshing coverage (make coverage) — this can take minutes…")
    proc = subprocess.run(["make", "coverage"], cwd=repo, capture_output=True, text=True, timeout=1800)
    log("coverage refresh exit " + str(proc.returncode))


def _write_baseline(args, unique: list[Action]) -> int:
    """--update-baseline: lock all current action keys and exit clean."""
    if not args.baseline:
        log("--update-baseline requires --baseline PATH")
        return 2
    keys = [action_key(a) for a in unique]
    args.baseline.write_text(json.dumps({"actions": keys}, indent=2))
    print(f"code-health: baseline written — {len(keys)} action(s) locked to {args.baseline}")
    return 0


def _apply_baseline(unique: list[Action], baseline_keys: set[str]) -> None:
    """Mark acknowledged actions so they report but never fail the gate."""
    for a in unique:
        if action_key(a) in baseline_keys:
            a.severity = "ack"


def main() -> int:
    args = parse_args()
    repo = args.repo.resolve()
    if not (repo / ".git").exists():
        log(f"{repo} is not a git repository")
        return 2

    fh = file_history(repo)
    if args.refresh_coverage:
        _refresh_coverage(repo)
    cr = load_coverage(repo)
    cc = _coverage_context(repo, cr.lines, cr.source)
    diff = changed_files(repo, args.base)
    actions = _collect_actions(repo, args, fh.churn, fh.last_modified, cr.lines, cc.graph_preferred, cc.stale_note)
    unique = _dedupe_merge(actions, diff)

    if args.update_baseline:
        return _write_baseline(args, unique)
    _apply_baseline(unique, _load_baseline(args.baseline))
    fails = [a for a in unique if a.severity != "ack"]
    acks = [a for a in unique if a.severity == "ack"]
    head = _git_head(repo)

    if args.json:
        _render_json(repo, args, unique, head.branch, head.commit, cc.label)
    else:
        _render_text(repo, args, unique, fails, acks, diff, cc.label, cc.graph_preferred)

    if fails and not args.warn:
        log(f"{len(fails)} action(s) found — failing (use --warn to run informational)")
        return 1
    return 0


if __name__ == "__main__":
    sys.exit(main())

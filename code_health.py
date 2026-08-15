#!/usr/bin/env python3
"""code_health.py — deterministic code-health gate: complexity + dependency + hotspot analysis.

Built on what we already run:
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
import builtins
import datetime
import os
import xml.etree.ElementTree as ET
from dataclasses import asdict, dataclass, field
from itertools import combinations
from subprocess import SubprocessError
from typing import NamedTuple

try:
    from radon.visitors import ComplexityVisitor  # optional dependency
except ImportError:  # code-health: ignore except radon is an optional dependency; absence is handled explicitly below
    ComplexityVisitor = None
import io
import json
import re
import sqlite3
import subprocess
import sys
import time
import tokenize
from collections import Counter, defaultdict
from pathlib import Path

import check_records

# Role/pattern suffixes from coding-standards.md: communicative for a thin
# framework-role class (MVC controller, event handler) that delegates; a smell
# when they hide load-bearing code under a vague name.
VAGUE_SUFFIXES = ("Manager", "Orchestrator", "Handler", "Store", "Repository", "Controller", "Utils", "Info")


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
    """One structural signal before it becomes an Action.

    severity: 'fail' (fails the gate) or 'warn' (reported, never fails) —
    warn carries the noisy-but-useful signals: magic numbers, duplication,
    unused functions, broad excepts.
    """

    signal: str
    function: str
    line: int
    metric: int
    detail: str
    inner: list[str]
    severity: str = "fail"


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
StatementBlocks = list[list[ast.stmt]]
NameGroups = list[list[str]]


@dataclass(frozen=True)
class ClassRef:
    """A class identified by its defining file and name — the two have distinct
    meanings, so a bare (file, name) tuple or a 'Key' alias would erase them."""

    file: str
    name: str


@dataclass(frozen=True)
class FunctionRecord:
    """A function's identity and structural skeleton, for the duplication search."""

    rel: str
    name: str
    line: int
    skeleton: list[str]


@dataclass(frozen=True)
class ReferenceScan:
    """One repo pass for the unused-function check.

    definitions: module-level functions per production file.
    prod_references: names, import aliases, and decorated function names
        from production files — a decorator is framework registration
        (routes, middleware), so a decorated function is referenced.
    test_references: names and aliases from test files — a function found
        ONLY here is a test seam or test-only dead code, not a live caller.
    strings: production string literals (CLI dispatch by name).
    """

    definitions: dict[str, dict[str, int]]
    prod_references: set[str]
    test_references: set[str]
    strings: list[str]


@dataclass(frozen=True)
class InvalidFileSuppression:
    """A `# code-health: ignore-file` comment that omitted the required why."""

    line: int
    signal: str


@dataclass(frozen=True)
class FileSuppressions:
    """File-scoped exemptions: explained ones, and the invalid (why-less) ones."""

    exemptions: dict[str, str]
    invalid: list[InvalidFileSuppression]


ModuleGraph = dict[str, set[str]]


@dataclass(frozen=True)
class DirFile:
    """A file inside a scanned folder and the graph community its code belongs to."""

    file: str
    community: int


@dataclass(frozen=True)
class ImportedSymbol:
    """A symbol brought in by an import: the dotted module and its original name."""

    module: str
    name: str


@dataclass
class ClassScan:
    """One repo-wide pass over classes: the registry, per-module import maps, and the class list."""

    classes: dict[ClassRef, ast.ClassDef]
    imports: dict[str, ImportAliases]
    rels: list[ClassRef]


ImportAliases = dict[str, ImportedSymbol]


class _RadonProvider:
    """Lazy radon services object: tests inject a fake via .visitor at setup.

    One module-level instance (RADON), populated at the entry point or in
    test setup with whatever object the run needs — the standard's
    'one global services object' pattern, never a bare global.
    """

    visitor: type | None = None

    def get(self, required: bool = True):
        if self.visitor is None:
            if ComplexityVisitor is None:
                if required:
                    log("radon not importable — run via `uv run --with radon python3 code_health.py`")
                    sys.exit(2)
                return None
            self.visitor = ComplexityVisitor
        return self.visitor


RADON = _RadonProvider()


def _radon_visitor(required: bool = True):
    return RADON.get(required)


# Fix guidance per action kind. One sentence each: what to do, not just what's
# wrong. Tied to the real requirements (readability, maintainability,
# anti-fragility) via separation of concerns, domain language, encapsulation.
# Deliberately resists gaming the metric: splitting a function to lower a
# count without clarifying it is not a fix.
GUIDANCE = {
    "complexity": (
        "Extract each decision branch into a named method that says what it decides in domain terms — one "
        "decision per method, happy path reads top-to-bottom. If the body is repeated similar blocks rather than "
        "distinct decisions, prefer a data table + loop over more methods. Where it mixes subsystems, extract a "
        "class per concern — for endpoints that usually means service-layer functions behind the Services DI, not "
        "new classes."
    ),
    "large-function": (
        "Split by responsibility into named steps that read like a procedure in the domain; one job per step, "
        "each independently testable."
    ),
    "hub-file": (
        "Decide what this file is first: if it is an assembly/composition root whose job is wiring (app layer, "
        "router), move handler logic out to the service layer and keep the assembly thin — the cross-module "
        "orchestration is its job, not a smell. Otherwise separate the concerns it mixes into modules with "
        "narrow, stable interfaces."
    ),
    "hotspot": (
        "Make the volatile part small and data-driven behind a stable interface — frequent changes become cheap "
        "and cannot disturb the stable core."
    ),
    "high-risk": (
        "Pin behavior with tests, then reduce the caller surface — when many things depend on it, the simplest "
        "code is the safest."
    ),
    "standard": (
        "A coding-standard rule with a checkable form is enforced in code, not left to review — fix it at its "
        "site; the fix is stated in the finding."
    ),
    "over-abstraction": (
        "An abstract base class with a single concrete implementation is ceremony, not design — the standard "
        "names it directly. Fold the one subclass into the base (or drop the ABC); an abstraction earns its keep "
        "at two real, differing implementations."
    ),
    "folder-mix": (
        "A folder whose direct files split across graph communities mixes concerns — each community is a "
        "dependency-tied group that wants its own sub-folder (folder-discipline: large clusters get their own "
        "folder). Extract a sub-folder per community; if the split is coincidental (the files genuinely share one "
        "reason to change), leave it — the evidence is the community graph, not intent."
    ),
    "layer-mix": (
        "A file whose functions partition by the subsystem they call into mixes architecture layers — the call "
        "graph is the seam: functions calling the model, the sheets, and the web layer belong in different "
        "modules. Extract a module per layer; a single dominant caller for all functions is not a finding."
    ),
    "docs": (
        "A documentation standard with a checkable form is enforced in code: every relative markdown link must "
        "resolve, and every doc must be reachable from AGENTS.md through links — several hops are the norm, not a "
        "finding. AGENTS.md carries only content relevant to every agent and links group indexes; it never "
        "flat-lists the whole doc tree, and each doc keeps one distinct purpose and audience."
    ),
    "vague-name": (
        "A role-suffix name (Controller, Handler, Store, Repository, Manager, Orchestrator, Utils, Info) is "
        "communicative only for a thin framework-role class that delegates — an MVC controller or event handler "
        "named for its role. This class carries real weight (see the span and method count): the domain noun it "
        "operates on should be taking the name and the logic. Name the class for that noun, or move the logic "
        "into the domain classes it should be delegating to; a genuinely thin role class is fine as-is."
    ),
    "latent-class": (
        "Closures that capture state are a class in disguise — if the inner functions form behavior groups, "
        "extract a class per group and hoist the closures to its methods (the captured state becomes fields). If "
        "methods touch disjoint field sets, that partition is the latent seam: extract a class per group and let "
        "the connectors compose them. If the grouping is incidental (no shared state, no shared fields), leave it "
        "— the evidence is state and field access, not a guess."
    ),
    "record-shape": (
        "The record wants a class — named fields with domain meaning, so a reader sees what the data IS without "
        "tracing it (encapsulation, obvious correctness). Make a small class/dataclass. If the data crosses a "
        "boundary (parsing or serialization), the fix is to ingest it into a domain class at that boundary: parse "
        "into the type and carry the type, don't carry the bare mapping. Constant lookup tables stay at module "
        "scope, never in an interface. If the keys are genuinely data (a true map: label -> value), suppress with "
        "`# code-health: ignore record-shape <why>` and name the alias by meaning — CoverageLines, never "
        "SomethingDict (a *Dict alias just renames the smell)."
    ),
}


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


def concern_clusters(
    conn: sqlite3.Connection,
    repo: Path,
    source_qn: str | None = None,
    source_prefix: str | None = None,
    own_module: str | None = None,
) -> Clusters:
    """Group a function's (or file's) cross-module callees by subsystem.

    clusters always lists what resolved (even a single subsystem — the caller
    decides how to word it); strong is True when >= 2 distinct subsystems
    with >= 3 total calls — the evidence bar for claiming a real seam mix.
    Never silently empty: callers show an explicit "unresolved" marker.
    """
    if source_qn is not None:
        rows = list(
            conn.execute(
                "SELECT DISTINCT target_qualified FROM edges WHERE source_qualified = ? AND kind = 'CALLS'",
                (source_qn,),
            )
        )
    else:
        rows = list(
            conn.execute(
                "SELECT DISTINCT target_qualified FROM edges WHERE source_qualified LIKE ? AND kind = 'CALLS'",
                (source_prefix + "%",),
            )
        )
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
    candidates = [
        "/".join(["tests", "unit"] + dirs + ["test_" + base + ".py"]),
        "/".join(["tests", "unit", "test_" + base + ".py"]),
    ]
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
            capture_output=True,
            text=True,
            timeout=20,
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
    try:
        proc = subprocess.run(
            ["git", "-C", str(repo), "log", "--name-only", "--pretty=format:%ad", "--date=short"],
            capture_output=True,
            text=True,
            timeout=120,
        )
    except subprocess.TimeoutExpired:  # code-health: ignore except a slow checkout degrades to empty history
        log(f"git log timed out in {repo} — history-based signals are skipped")
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
    return any(ln in lines for ln in range(start, end + 1))


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


def log(msg: str) -> None:
    print(msg, file=sys.stderr)


# --------------------------------------------------------------------------- complexity (radon)
@dataclass(frozen=True)
class SourceFile:
    """One scanned .py file: its path and repo-relative name."""

    py: Path
    rel: str


class ParsedSource(NamedTuple):
    """One file's cached parse: the AST (None on parse failure) and the source text."""

    tree: ast.Module | None
    source: str


def _py_files(repo: Path, only_rel: str | None = None) -> list[SourceFile]:
    """The repo's own .py files — git's answer, not an invented list.

    `git ls-files --cached --others --exclude-standard` = tracked files plus
    untracked-not-ignored: exactly what the repo's .gitignore defines as its
    code (venvs, caches, generated output never qualify, whatever they are
    named). One call per run, NUL-split, no quoting issues. With only_rel
    set, returns just that file (the --file / LSP mode).
    """
    if only_rel is not None:
        py = repo / only_rel
        return [SourceFile(py, only_rel)] if py.is_file() and py.suffix == ".py" else []
    try:
        proc = subprocess.run(
            ["git", "-C", str(repo), "ls-files", "--cached", "--others", "--exclude-standard", "-z", "--", "*.py"],
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
            for py in sorted(repo.rglob("*.py"))
            if not any(_excluded_part(part) for part in py.parts)
        ]


class _SourceCache:
    """Memoized parse: the scan passes used to re-parse every file four times.

    Keyed by absolute path + mtime+size — a pure function of file content,
    not state: the same input always yields the same tree and source."""

    def __init__(self) -> None:
        self._files: dict[str, tuple[float, int, ast.Module | None, str]] = {}

    def get(self, py: Path) -> ParsedSource:
        st = py.stat()
        key = str(py.resolve())
        hit = self._files.get(key)
        if hit and hit[0] == st.st_mtime and hit[1] == st.st_size:
            return ParsedSource(hit[2], hit[3])
        source = py.read_text(encoding="utf-8", errors="replace")
        try:
            tree = ast.parse(source)
        except (SyntaxError, UnicodeDecodeError):  # code-health: ignore except an unparseable file is skipped
            tree = None
        self._files[key] = (st.st_mtime, st.st_size, tree, source)
        return ParsedSource(tree, source)


# code-health: ignore global-state the parse cache is a memo of file content — a pure
# function of inputs, not mutable state that changes behavior; without it every file is
# re-parsed four times per run
SOURCE_CACHE = _SourceCache()


class RustFindings(NamedTuple):
    """The Rust scan output for one file set, keyed by repo-relative path —
    a named object, not a bare map (the record-shape rule's escape hatch)."""

    by_rel: dict[str, list[LatentFinding]]

    def for_rel(self, rel: str) -> list[LatentFinding]:
        return self.by_rel.get(rel, [])


class _RustScan:
    """The Rust scan core as the default finding engine.

    One binary invocation per repo per run (or per file in --file / LSP mode);
    the JSON findings (already suppression-filtered) convert straight to
    LatentFindings. Falls back to the pure-Python path when the binary is
    missing or a run fails — the gate never silently loses findings.
    """

    def __init__(self) -> None:
        self._cache: dict[tuple[Path, tuple[str, ...]], RustFindings | None] = {}
        self._binary_cache: dict[Path, Path | None] = {}
        # test seam: the unit tests fake subprocess + radon and target the
        # Python engine (the parity suite validates the Rust core) — Env
        # flips this off so the binary never fires under a fake subprocess
        self.enabled = True

    # code-health: ignore global-state the scanner cache is a per-run memo of subprocess
    # output — a pure function of the repo + file set, not mutable state with behavior

    def binary(self, repo: Path) -> Path | None:
        """The code-health-scan binary: env override, then repo-relative, then
        tool-relative. None when not built — the Python path takes over."""
        if repo in self._binary_cache:
            return self._binary_cache[repo]
        candidates: list[Path] = []
        env = os.environ.get("CODE_HEALTH_SCANNER")
        if env:
            candidates.append(Path(env))
        candidates.append(repo / "scanner" / "target" / "release" / "code-health-scan")
        candidates.append(Path(__file__).resolve().parent / "scanner" / "target" / "release" / "code-health-scan")
        found = next((p for p in candidates if p.is_file()), None)
        self._binary_cache[repo] = found
        return found

    def load(self, repo: Path, files: list[SourceFile]) -> RustFindings | None:
        """Findings per rel for one file set; None = Rust unavailable (Python path)."""
        rels = tuple(sf.rel for sf in files)
        key = (repo, rels)
        if key in self._cache:
            return self._cache[key]
        if not self.enabled:
            # explicit Python-engine opt-in — the parity reference, tests only
            return None
        binary = self.binary(repo)
        if binary is None:
            raise RuntimeError(
                "the Rust scan core is required — build it with `make scanner-check` "
                "(the Python engine exists only as the parity-test reference)"
            )
        result: dict[str, list[LatentFinding]] | None = None
        if files:
            result = {}
            try:
                proc = subprocess.run(
                    [str(binary)] + [str(sf.py) for sf in files],
                    capture_output=True, text=True, timeout=180,
                )
                if proc.returncode == 0:
                    data = json.loads(proc.stdout)
                    rels_set = set(rels)
                    for f in data.get("findings", []):
                        rel = _rust_finding_rel(f.get("file", ""), repo, rels_set)
                        if rel is None:
                            continue
                        result.setdefault(rel, []).append(
                            LatentFinding(
                                signal=f.get("kind", ""),
                                function=f.get("function", ""),
                                line=int(f.get("line", 0)),
                                metric=1,
                                detail=f.get("message", ""),
                                inner=[],
                                severity=f.get("severity", "fail"),
                            )
                        )
                else:
                    result = None
            except _SCANNER_FAILURES:  # code-health: ignore except degraded runs report nothing — visible
                result = None
        wrapped = RustFindings(result) if result is not None else None
        self._cache[key] = wrapped
        return wrapped

    def active(self, repo: Path) -> bool:
        """True when the Rust backend is the live scan path for this repo."""
        return self.enabled and self.binary(repo) is not None


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
# out, or returns garbage degrades to the Python path, never to missing findings
_SCANNER_FAILURES = (OSError, SubprocessError, json.JSONDecodeError, ValueError)


def complexity_actions(
    repo: Path,
    max_cc: int,
    include_tests: bool,
    file_churn: Counter[str],
    last_modified: dict[str, str],
    covered: CoverageLines | None,
    graph_preferred: bool,
    stale_note: str,
    only_rel: str | None = None,
) -> list[Action]:
    """Cyclomatic complexity per function via radon's fast pure-Python analyzer."""
    complexity_visitor = _radon_visitor()
    conn = _graph_conn(repo)
    actions: list[Action] = []
    for sf in _py_files(repo, only_rel):
        py, rel = sf.py, sf.rel
        if not include_tests and ("/test" in f"/{rel}" or rel.startswith("test")):
            continue
        try:
            source = py.read_text(encoding="utf-8", errors="replace")
            visitor = complexity_visitor.from_code(source)
        except (
            SyntaxError,
            UnicodeDecodeError,
            RecursionError,
        ):  # code-health: ignore except an unparseable file is skipped, not a scan failure
            continue
        for fn in visitor.functions:
            if fn.complexity < max_cc:
                continue
            actions.append(
                _complexity_action(
                    repo, rel, fn, max_cc, conn, source, covered, graph_preferred, stale_note, file_churn, last_modified
                )
            )
    return actions


def _function_mix(conn, repo: Path, rel: str, name: str, info: NodeInfo | None) -> Clusters:
    """Concern-seam result for a function's graph node, or empty when no node."""
    if info is None:
        return Clusters([], False, [])
    return concern_clusters(conn, repo, source_qn=info.qualified_name, own_module=_module_key(repo, info.file_path))


def _complexity_action(
    repo: Path,
    rel: str,
    fn,
    max_cc: int,
    conn,
    source: str,
    covered,
    graph_preferred: bool,
    stale_note: str,
    file_churn: Counter[str],
    last_modified: dict[str, str],
) -> Action:
    """One complexity action: finding + seam wording + coverage note."""
    info = _node_info(conn, repo, rel, fn.name)
    mix = _function_mix(conn, repo, rel, fn.name, info)
    if info and not info.params:
        info.def_sig = _def_signature(source, fn.lineno)
    finding = f"cyclomatic complexity {fn.complexity} (>= {max_cc})"
    if mix.clusters:
        message = (
            f"{finding} — {mix_text(mix.clusters, mix.strong)} — extract a class per concern; "
            f"the seams are those subsystem boundaries, not line breaks."
        )
    else:
        snippet = "calls: " + ", ".join(mix.unresolved) if mix.unresolved else "no cross-module callees resolved"
        message = f"{finding} — {GUIDANCE['complexity']} [concern mix unresolved — {snippet}]"
    note = coverage_note(covered, repo, rel, info, info.tested, graph_preferred, stale_note) if info else ""
    churn = file_churn.get(rel, 0)
    return Action(
        kind="complexity",
        severity="fail",
        file=rel,
        line=fn.lineno,
        function=fn.name,
        message=message,
        metric=fn.complexity,
        churn=churn,
        last_modified=last_modified.get(rel, ""),
        tested=final_tested(covered, rel, info, graph_preferred) if info else "",
        note=note,
        raw=_raw_score("complexity", fn.complexity, churn),
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


def coverage_note(
    covered: CoverageLines | None,
    repo: Path,
    rel: str,
    info: NodeInfo,
    graph_tested: str,
    graph_preferred: bool = False,
    stale_note: str = "",
) -> str:
    """Append a coverage-based instruction when the function is untested."""
    contract = contract_text(info.name, info.params, info.return_type, info.def_sig)
    tfile = _test_file_for(repo, rel)
    extend = f" Extend {tfile}." if tfile else ""
    verdict = _verdict(covered, rel, info, graph_tested, graph_preferred)
    if verdict == "tested":
        return ""
    if verdict == "untested":
        return (
            f" Not covered by the repo's coverage data — write the failing tests first. "
            f"Contract to pin: {contract}.{extend}"
        )
    # unknown: stale snapshot and graph blind to it — verify, never assert
    return (
        f" Coverage snapshot is older than the repo's tests and the graph's TESTED_BY edges "
        f"don't reach this function (in-body imports and HTTP-path tests are invisible to it) — "
        f"verify with make coverage / htmlcov; if truly uncovered, pin "
        f"{contract} with tests first.{extend}"
    )


# --------------------------------------------------------------------------- graph (code-review-graph)
def _graph_db(repo: Path) -> Path | None:
    db = repo / ".code-review-graph" / "graph.db"
    return db if db.exists() else None


def graph_actions(
    repo: Path,
    max_fn_lines: int,
    max_file_edges: int,
    max_risk: float,
    include_tests: bool,
    file_churn: Counter[str],
    last_modified: dict[str, str],
    covered: CoverageLines | None,
    graph_preferred: bool,
    stale_note: str,
) -> list[Action]:
    """Repo-structure actions from the code-review-graph SQLite: large functions, hub files, high risk."""
    db_path = _graph_db(repo)
    if db_path is None:
        log(
            f"no graph at {repo / '.code-review-graph' / 'graph.db'} — run "
            f"`code-review-graph build --repo {repo}` first"
        )
        return []
    db = sqlite3.connect(db_path)
    db.row_factory = sqlite3.Row
    actions: list[dict] = []
    actions += _large_function_actions(
        db, repo, max_fn_lines, include_tests, file_churn, last_modified, covered, graph_preferred, stale_note
    )
    actions += _hub_file_actions(
        db, repo, max_file_edges, include_tests, file_churn, last_modified, covered, graph_preferred, stale_note
    )
    actions += _high_risk_actions(
        db, repo, max_risk, include_tests, file_churn, last_modified, covered, graph_preferred, stale_note
    )
    db.close()
    return actions


def _read_source(src_cache: dict[str, str], repo: Path, rel: str) -> str:
    """Cached file source for def-signature and fattest-handler extraction."""
    if rel not in src_cache:
        try:
            src_cache[rel] = (repo / rel).read_text(encoding="utf-8", errors="replace")
        except OSError:  # code-health: ignore except an unreadable file yields no def signature, not a scan failure
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


def _large_function_actions(
    db,
    repo: Path,
    max_fn_lines: int,
    include_tests: bool,
    file_churn: Counter[str],
    last_modified: dict[str, str],
    covered,
    graph_preferred: bool,
    stale_note: str,
) -> list[Action]:
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
        mix = concern_clusters(
            db, repo, source_qn=row["qualified_name"], own_module=_module_key(repo, row["file_path"])
        )
        message = _mix_message(
            f"function spans {span} lines (>= {max_fn_lines})",
            mix,
            GUIDANCE["large-function"],
            "extract a class per concern, then split each class's methods into named domain steps.",
        )
        info = _info_signature(
            NodeInfo(
                name=row["name"],
                qualified_name=row["qualified_name"],
                file_path=row["file_path"],
                tested=row["test_coverage"] or "",
                params=row["params"] or "",
                return_type=row["return_type"] or "",
                line_start=row["line_start"],
                line_end=row["line_end"],
            ),
            src_cache,
            repo,
            rel,
            row["line_start"],
        )
        note = coverage_note(covered, repo, rel, info, row["test_coverage"] or "", graph_preferred, stale_note)
        churn = file_churn.get(rel, 0)
        actions.append(
            Action(
                kind="large-function",
                severity="fail",
                file=rel,
                line=row["line_start"],
                function=row["name"],
                message=message,
                metric=span,
                churn=churn,
                last_modified=last_modified.get(rel, ""),
                tested=final_tested(covered, rel, info, graph_preferred),
                note=note,
                raw=_raw_score("large-function", span, churn),
            )
        )
    return actions


def _hub_edge_counts(db) -> Counter[str]:
    """Per-file coupling edges, excluding CALLS to true builtins (print/len/
    isinstance are not coupling) and non-CALLS-to-builtin noise."""
    counts: Counter[str] = Counter()
    hub_kinds = ("CALLS", "IMPORTS_FROM", "INHERITS", "REFERENCES")
    for row in db.execute(
        f"SELECT file_path, target_qualified, kind FROM edges WHERE kind IN {hub_kinds} AND file_path LIKE '%.py'",
    ):
        if _is_builtin_call(row):
            continue
        counts[row["file_path"]] += 1
    return counts


def _is_builtin_call(row) -> bool:
    return row["kind"] == "CALLS" and row["target_qualified"].split("::")[-1].split(".")[-1] in BUILTIN_NAMES


def _hub_file_actions(
    db,
    repo: Path,
    max_file_edges: int,
    include_tests: bool,
    file_churn: Counter[str],
    last_modified: dict[str, str],
    covered,
    graph_preferred: bool,
    stale_note: str,
) -> list[Action]:
    """Files with heavy coupling (CALLS/IMPORTS_FROM/INHERITS/REFERENCES, no test-harness edges)."""
    actions: list[Action] = []
    src_cache: dict[str, str] = {}
    counts = _hub_edge_counts(db)
    for file_path, edge_count in counts.most_common():
        if edge_count < max_file_edges:
            break
        rel = rel_path(repo, file_path)
        if not include_tests and is_test_path(rel):
            continue
        abs_file = str(repo.resolve()) + "/" + rel
        mix = concern_clusters(db, repo, source_prefix=abs_file + "::", own_module=_module_key(repo, file_path))
        message = _mix_message(
            f"{edge_count} call/import edges (>= {max_file_edges})",
            mix,
            GUIDANCE["hub-file"],
            "split into one module per concern with narrow, stable interfaces so changes stay contained.",
        )
        # Point at the file's fattest handlers: top-3 by cyclomatic complexity.
        first = db.execute(
            "SELECT MIN(line_start) FROM nodes WHERE file_path = ? AND kind IN ('Function', 'Method')",
            (file_path,),
        ).fetchone()[0]
        fat = ""
        try:
            visitor = _radon_visitor(required=False)
            if visitor is not None:
                source = _read_source(src_cache, repo, rel)
                fns = visitor.from_code(source).functions if source else []
                top = sorted(fns, key=lambda f: f.complexity, reverse=True)[:3]
                if top:
                    fat = " fattest: " + ", ".join(f"{f.name}:{f.lineno} (CC {f.complexity})" for f in top)
                    anchor = top[0].lineno
        except Exception as exc:  # code-health: ignore except fattest analysis is best-effort; the file parsed above
            log(f"fattest-handler analysis failed for {rel}: {exc}")
        message += fat
        churn = file_churn.get(rel, 0)
        actions.append(
            Action(
                kind="hub-file",
                severity="fail",
                file=rel,
                line=anchor if fat else (first or 1),
                function="",
                message=message,
                metric=edge_count,
                churn=churn,
                last_modified=last_modified.get(rel, ""),
                tested="",
                raw=_raw_score("hub-file", edge_count, churn),
            )
        )
    return actions


def _callers_text(db, row) -> Callers:
    """Distinct callers of a node from CALLS edges (qualified and bare-name targets)."""
    callers = [
        r[0].split("::")[-1]
        for r in db.execute(
            "SELECT DISTINCT source_qualified FROM edges WHERE kind = 'CALLS' "
            "AND (target_qualified = ? OR target_qualified = ? OR target_qualified LIKE ?)",
            (row["qualified_name"], row["name"], "%::" + row["name"]),
        )
    ][:8]
    if not callers:
        return Callers([], "")
    text = f", callers: {', '.join(callers)}"
    if len(callers) < row["caller_count"]:
        text += (
            f" ({len(callers)} distinct of {row['caller_count']} call sites per risk index — "
            f"count includes repeated call sites)"
        )
    return Callers(callers, text)


def _high_risk_actions(
    db,
    repo: Path,
    max_risk: float,
    include_tests: bool,
    file_churn: Counter[str],
    last_modified: dict[str, str],
    covered,
    graph_preferred: bool,
    stale_note: str,
) -> list[Action]:
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
        action = _high_risk_action(
            db,
            repo,
            row,
            max_risk,
            include_tests,
            src_cache,
            file_churn,
            last_modified,
            covered,
            graph_preferred,
            stale_note,
        )
        if action:
            actions.append(action)
    return actions


def _high_risk_action(
    db,
    repo: Path,
    row,
    max_risk: float,
    include_tests: bool,
    src_cache: dict[str, str],
    file_churn: Counter[str],
    last_modified: dict[str, str],
    covered,
    graph_preferred: bool,
    stale_note: str,
) -> Action | None:
    """One high-risk action, or None for test files."""
    rel = rel_path(repo, row["file_path"])
    if not include_tests and is_test_path(rel):
        return None
    resolved = _callers_text(db, row)
    callers = resolved.callers
    message = (
        f"graph risk {row['risk_score']:.2f} (>= {max_risk}), "
        f"{len(callers) or row['caller_count']} call site(s){resolved.text} — "
        f"{GUIDANCE['high-risk']}"
    )
    info = _info_signature(
        NodeInfo(
            name=row["name"],
            qualified_name=row["qualified_name"],
            file_path=row["file_path"],
            tested=row["test_coverage"] or "",
            params=row["params"] or "",
            return_type=row["return_type"] or "",
            line_start=row["line_start"] or 1,
            line_end=row["line_end"] or row["line_start"] or 1,
        ),
        src_cache,
        repo,
        rel,
        row["line_start"] or 1,
    )
    note = coverage_note(covered, repo, rel, info, row["test_coverage"] or "", graph_preferred, stale_note)
    churn = file_churn.get(rel, 0)
    return Action(
        kind="high-risk",
        severity="fail",
        file=rel,
        line=row["line_start"] or 1,
        function=row["name"],
        message=message,
        metric=round(row["risk_score"], 2),
        churn=churn,
        last_modified=last_modified.get(rel, ""),
        tested=final_tested(covered, rel, info, graph_preferred),
        callers=callers,
        note=note,
        raw=_raw_score("high-risk", row["risk_score"], churn, len(callers) if callers else row["caller_count"]),
    )


# --------------------------------------------------------------------------- hotspots (git history x complexity)
def _volatile_parts(conn, repo: Path, rel: str, fns, min_cc: float) -> list[VolatilePart]:
    """Complex functions in a hotspot file with their own churn (git log -L)."""
    volatile: list[VolatilePart] = []
    if conn is None:
        return volatile
    abs_file = str(repo.resolve()) + "/" + rel
    nodes = {
        r["name"]: r
        for r in conn.execute(
            "SELECT name, line_start, line_end FROM nodes WHERE file_path = ? AND kind IN ('Function', 'Method')",
            (abs_file,),
        )
    }
    for fn in fns:
        if fn.complexity < min_cc:
            continue
        node = nodes.get(fn.name)
        if node is None or node["line_start"] is None or node["line_end"] is None:
            continue
        churn = function_churn(repo, rel, node["line_start"], node["line_end"])
        volatile.append(VolatilePart(churn=churn, complexity=fn.complexity, name=fn.name, line=node["line_start"]))
    return volatile


def hotspot_actions(
    repo: Path, top_frac: float, min_cc: float, file_churn: Counter[str], last_modified: dict[str, str]
) -> list[Action]:
    """Hotspot: files that change often AND are complex.

    Change frequency from the shared `git log --name-only` pass; complexity =
    max cyclomatic complexity of the file's functions (radon). Max, not mean:
    mean dilutes when a file mixes many small functions with one monster —
    The hotspot signal is the concentration of complexity in a
    frequently-changed file.
    """
    complexity_visitor = _radon_visitor()
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
            visitor = complexity_visitor.from_code(source)
        except (
            SyntaxError,
            UnicodeDecodeError,
        ):  # code-health: ignore except an unparseable file is skipped, not a scan failure
            continue
        fns = visitor.functions
        if not fns:
            continue
        max_cc = max(f.complexity for f in fns)
        if max_cc < min_cc:
            continue
        # Name the volatile part: complex functions in this file, with their own
        # churn (git log -L over the graph's line range). Cap at 3 per file.
        volatile = sorted(
            _volatile_parts(conn, repo, rel, fns, min_cc), key=lambda v: (v.churn, v.complexity), reverse=True
        )
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
    p = argparse.ArgumentParser(
        description="deterministic code-health gate: complexity/dependency/hotspot from code-review-graph + radon + git"
    )
    p.add_argument("--repo", type=Path, default=Path.cwd(), help="repository root (default: cwd)")
    p.add_argument("--file", type=str, default=None,
                   help="scan ONE repo-relative .py file (the LSP mode): per-file findings only, "
                        "no git history / graph / coverage / repo-wide scans")
    p.add_argument(
        "--max-complexity", type=int, default=15, help="fail functions with cyclomatic complexity >= N (default 15)"
    )
    p.add_argument(
        "--max-function-lines", type=int, default=120, help="fail functions spanning >= N lines (default 120)"
    )
    p.add_argument(
        "--max-file-edges", type=int, default=150, help="fail files with >= N call/import edges (default 150)"
    )
    p.add_argument("--max-risk", type=float, default=0.8, help="fail nodes with graph risk score >= N (default 0.8)")
    p.add_argument(
        "--hotspot-top-frac",
        type=float,
        default=0.1,
        help="hotspot candidate set: top fraction of files by change count (default 0.1)",
    )
    p.add_argument(
        "--hotspot-min-cc", type=float, default=15.0, help="hotspot requires file max complexity >= N (default 15)"
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
        except subprocess.TimeoutExpired:  # code-health: ignore except a slow checkout degrades to an empty diff
            log(f"git diff against {ref} timed out — diff awareness skipped")
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
    """Current branch and short commit for report provenance."""
    branch = subprocess.run(
        ["git", "-C", str(repo), "branch", "--show-current"], capture_output=True, text=True
    ).stdout.strip()
    commit = subprocess.run(
        ["git", "-C", str(repo), "rev-parse", "--short", "HEAD"], capture_output=True, text=True
    ).stdout.strip()
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
        "thresholds: CC>="
        + str(args.max_complexity)
        + ", fn>="
        + str(args.max_function_lines)
        + " lines, file>="
        + str(args.max_file_edges)
        + " edges, risk>="
        + str(args.max_risk)
        + ", hotspot top "
        + f"{args.hotspot_top_frac:.0%}"
        + " by churn with CC>="
        + str(args.hotspot_min_cc)
        + f"; coverage: {coverage_source}"
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
        "\nre-run: uv run --with radon python3 code_health.py --repo "
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


def _latent_class_actions(
    repo: Path, include_tests: bool, file_churn: Counter[str], last_modified: dict[str, str],
    only_rel: str | None = None,
) -> list[Action]:
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
    visitor = _radon_visitor(required=False)
    actions: list[Action] = []
    files = _py_files(repo, only_rel)
    for sf in files:
        actions += _scan_file(sf.py, sf.rel, include_tests, visitor, repo, file_churn, last_modified,
                              files=files)
    return actions


def _scan_file(
    py: Path,
    rel: str,
    include_tests: bool,
    visitor_cls,
    repo: Path,
    file_churn: Counter[str],
    last_modified: dict[str, str],
    files: list[SourceFile] | None = None,
) -> list[Action]:
    """One file's latent-class / vague-name / standard findings.

    Default engine: the Rust scan core (RUST_SCAN) — one binary invocation
    per repo covers every per-file family plus the repo-wide duplicate/unused
    scans. The Python-only families stay here: the latent-class field
    partition (graph-based) and the rules that live in tests (monkeypatch,
    env-skipif, fakefs). Falls back to the pure-Python path when the binary
    is missing."""
    parsed = SOURCE_CACHE.get(py)
    tree, source = parsed.tree, parsed.source
    if tree is None:
        return []
    supps = _suppressions(source)
    file_supps = _file_suppressions(source)
    rust = RUST_SCAN.load(repo, files) if files is not None else None
    if rust is not None:
        return _rust_scan_file(
            rel, include_tests, tree, source, supps, file_supps, rust, repo, file_churn, last_modified
        )
    return _python_scan_file(rel, include_tests, visitor_cls, tree, source, supps, file_supps, repo, file_churn,
                             last_modified)


def _python_scan_file(
    rel: str,
    include_tests: bool,
    visitor_cls,
    tree,
    source: str,
    supps: dict[int, tuple[str, str]],
    file_supps,
    repo: Path,
    file_churn: Counter[str],
    last_modified: dict[str, str],
) -> list[Action]:
    """The pure-Python fallback engine: the classic per-file scan plus the
    test-only rule families and invalid-suppression findings."""
    is_test = "/test" in f"/{rel}" or rel.startswith("test")
    if is_test and not include_tests:
        # test files are excluded from the health scan, but the rules that live
        # in tests (monkeypatch, env-skipif, fakefs) are scanned for alone
        findings = _monkeypatch_findings(tree, rel) + _skipif_findings(tree, rel) + _fakefs_findings(tree, rel)
        findings += _invalid_suppressions(supps)
        findings += [
            LatentFinding(
                signal="suppression",
                function="",
                line=s.line,
                metric=1,
                detail=f"file suppression '# code-health: ignore-file {s.signal}' at line {s.line} without a why — "
                f"exemptions only apply with an explanation",
                inner=[],
            )
            for s in file_supps.invalid
        ]
        return [
            _latent_action(repo, rel, f, file_churn, last_modified)
            for f in findings
            if not _suppressed(f.signal, f.line, supps) and f.signal not in file_supps.exemptions
        ]
    fn_map = _radon_map(visitor_cls, source)
    findings = _scan_findings(tree, rel, fn_map, source)
    if is_test:
        # the test-only rule families apply whether or not --include-tests
        # flipped the general scan on — include-tests adds checks, never drops them
        findings += _monkeypatch_findings(tree, rel) + _skipif_findings(tree, rel) + _fakefs_findings(tree, rel)
    findings += _invalid_suppressions(supps)
    return [
        _latent_action(repo, rel, f, file_churn, last_modified)
        for f in findings
        if not _suppressed(f.signal, f.line, supps) and f.signal not in file_supps.exemptions
    ]


def _rust_scan_file(
    rel: str,
    include_tests: bool,
    tree,
    source: str,
    supps: dict[int, tuple[str, str]],
    file_supps,
    rust: RustFindings,
    repo: Path,
    file_churn: Counter[str],
    last_modified: dict[str, str],
) -> list[Action]:
    """The Rust path of _scan_file: findings already suppression-filtered by
    the binary; only the Python-only families are computed here."""
    is_test = "/test" in f"/{rel}" or rel.startswith("test")
    findings: list[LatentFinding] = []
    if is_test and not include_tests:
        # test files are excluded from the health scan, but the rules that
        # live in tests are scanned for alone — plus the suppression rules
        # (the binary's suppression/type-ignore findings cover invalid ones)
        findings += [f for f in rust.for_rel(rel) if f.signal in ("suppression", "type-ignore")]
    else:
        findings += rust.for_rel(rel)
        findings += _partition_findings(tree, rel)
    if is_test:
        # the test-only rule families apply whether or not --include-tests
        # flipped the general scan on — include-tests adds checks, never drops them
        findings += _monkeypatch_findings(tree, rel) + _skipif_findings(tree, rel) + _fakefs_findings(tree, rel)
    return [
        _latent_action(repo, rel, f, file_churn, last_modified)
        for f in findings
        if not _suppressed(f.signal, f.line, supps) and f.signal not in file_supps.exemptions
    ]


def _radon_map(visitor_cls, source: str) -> dict[tuple[str, int], int]:
    """function-name+line -> cyclomatic complexity, when radon is available."""
    fn_map: dict[tuple[str, int], int] = {}
    if visitor_cls is not None:
        for f in visitor_cls.from_code(source).functions:
            fn_map[(f.name, f.lineno)] = f.complexity
    return fn_map


def _scan_findings(tree, rel: str, fn_map, source: str) -> list[LatentFinding]:
    """The per-file finding families for one parsed file (one shared parents map)."""
    parents = {id(child): node for node in ast.walk(tree) for child in ast.iter_child_nodes(node)}
    return (
        _closure_findings(tree, rel, fn_map)
        + _partition_findings(tree, rel)
        + _vague_name_findings(tree, rel)
        + _standard_findings(tree, parents, rel, source)
        + _class_module_findings(tree, rel)
    )


def _invalid_suppressions(supps: dict[int, tuple[str, str]]) -> list[LatentFinding]:
    """A `# code-health: ignore <signal>` without a why is itself a finding."""
    return [
        LatentFinding(
            signal="suppression",
            function="",
            line=line,
            metric=1,
            detail=f"suppression '# code-health: ignore {sig}' at line {line} without a why — "
            f"the tool is only skipped when you explain why it is wrong",
            inner=[],
        )
        for line, (sig, why) in supps.items()
        if not why
    ]


def _closure_findings(tree: ast.Module, rel: str, fn_map: dict[tuple[str, int], int]) -> list[LatentFinding]:
    """Functions/methods with >= 2 inner function defs and size/complexity to match."""
    findings: list[LatentFinding] = []
    for fn in [n for n in ast.walk(tree) if isinstance(n, (ast.FunctionDef, ast.AsyncFunctionDef))]:
        span = (fn.end_lineno or fn.lineno) - fn.lineno
        cc = fn_map.get((fn.name, fn.lineno), 0)
        if cc < 15 and span < 60:
            continue
        inner = [n.name for n in ast.walk(fn) if n is not fn and isinstance(n, (ast.FunctionDef, ast.AsyncFunctionDef))]
        lambdas = sum(1 for n in ast.walk(fn) if isinstance(n, ast.Lambda))
        if len(inner) + lambdas < 2:
            continue
        findings.append(
            LatentFinding(
                signal="closures",
                function=fn.name,
                line=fn.lineno,
                metric=len(inner) + lambdas,
                detail=_closure_detail(inner, lambdas, cc, span),
                inner=inner[:6],
            )
        )
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
        mf[m.name] = {
            node.attr
            for node in ast.walk(m)
            if isinstance(node, ast.Attribute) and isinstance(node.value, ast.Name) and node.value.id == "self"
        }
    partition = _find_partition(list(mf), mf)
    if partition is None:
        return None
    connectors, groups = partition
    return LatentFinding(
        signal="partition",
        function=cls.name,
        line=cls.lineno,
        metric=sum(len(g) for g in groups),
        detail=_partition_detail(connectors, groups),
        inner=[],
    )


def _partition_detail(connectors: list[str], groups: NameGroups) -> str:
    """Wording: the field-disjoint groups and which connectors were removed."""
    groups_text = "/".join("{" + ",".join(g) + "}" for g in groups)
    conn_text = "{" + ",".join(connectors) + "}" if connectors else "none"
    return (
        f"methods split into {len(groups)} field-disjoint groups ({groups_text}), "
        f"connectors removed: {conn_text} — each group touches only its own fields, "
        f"so each is a latent class"
    )


def _find_partition(names: list[str], mf: MethodFields):
    """Smallest connector removal that splits the shared-field graph into
    >= 2 groups of >= 2 methods each touching >= 2 distinct fields."""
    for removal in range(3):
        for removed in combinations(names, removal):
            kept = [n for n in names if n not in removed]
            groups = _connected_groups(kept, mf)
            big = [g for g in groups if len(g) >= 2 and len({f for m in g for f in mf[m]}) >= 2]
            if len(big) >= 2:
                return list(removed), big
    return None


def _connected_groups(kept: list[str], mf: MethodFields) -> NameGroups:
    """Methods connected by sharing at least one field."""
    groups: NameGroups = []
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


def _vague_name_findings(tree: ast.Module, rel: str) -> list[LatentFinding]:
    """Classes whose role-suffix name hides load-bearing code.

    A thin framework-role class (MVC controller, event handler) that only
    delegates is communicatively named and passes; the finding is the load —
    a vague-suffix class with >= 120 lines or >= 6 methods, where the domain
    noun it operates on should be taking the name.
    """
    findings: list[LatentFinding] = []
    for cls in [n for n in tree.body if isinstance(n, ast.ClassDef)]:
        for suffix in VAGUE_SUFFIXES:
            if not cls.name.endswith(suffix):
                continue
            methods = [n for n in cls.body if isinstance(n, (ast.FunctionDef, ast.AsyncFunctionDef))]
            span = (cls.end_lineno or cls.lineno) - cls.lineno
            if span < 120 and len(methods) < 6:
                break  # thin role class — the name is the communication, not a smell
            findings.append(
                LatentFinding(
                    signal="vague-name",
                    function=cls.name,
                    line=cls.lineno,
                    metric=len(methods),
                    detail=f"'{suffix}' name carries a {span}-line class with {len(methods)} methods — "
                    f"a thin role class that only delegates is communicative; this one takes real "
                    f"weight, so the domain noun it operates on should carry the name and the logic",
                    inner=[],
                )
            )
            break
    return findings


def _standard_findings(tree: ast.Module, parents: dict[int, ast.AST], rel: str, source: str) -> list[LatentFinding]:
    """Coding-standard rules with a checkable form (Tier-1, near-zero false positives).

    ONE walk, dispatched per node type — the families used to each walk the
    whole tree (~12 walks per file; 5M walks across a repo in the profile).
    parents is the single map built in _scan_findings and shared by every
    handler that needs ancestry.
    """
    findings: list[LatentFinding] = []
    for node in ast.walk(tree):
        if isinstance(node, (ast.Import, ast.ImportFrom)):
            if _has_function_ancestor(node, parents):
                findings.append(_inline_import_finding(node))
            findings += _private_import_finding(node)
        elif isinstance(node, (ast.Try, getattr(ast, "TryStar", ast.Try))):
            findings += _except_try_findings(node, parents)
            findings += _broad_except_try_findings(node, parents)
        elif isinstance(node, (ast.FunctionDef, ast.AsyncFunctionDef)):
            findings += _shadow_args_findings(node)
        elif isinstance(node, ast.Assign) and _has_function_ancestor(node, parents):
            findings += _shadow_assign_findings(node, parents)
        elif isinstance(node, ast.Constant):
            finding = _magic_constant_finding(node, parents)
            if finding is not None:
                findings.append(finding)
        elif isinstance(node, ast.Expr):
            finding = _noop_expr_finding(node, parents)
            if finding is not None:
                findings.append(finding)
    # the families that are not tree-walk dispatches: tokenize-based,
    # module-body-only, or per-function body walks
    findings += _global_state_findings(tree, rel)
    findings += _type_ignore_findings(source, rel)
    findings += _strewing_findings(tree, rel)
    findings += _tuple_alias_findings(tree, rel)
    findings += _monkeypatch_findings(tree, rel)
    findings += _unreachable_findings(tree, rel)
    return findings


def _inline_import_finding(node: ast.AST) -> LatentFinding:
    return LatentFinding(
        signal="inline-import",
        function="",
        line=node.lineno,
        metric=1,
        detail=f"import inside function body at line {node.lineno}: '{ast.unparse(node)}' — "
        f"inline imports hide dependencies from static analysis; move every import to module top",
        inner=[],
    )


def _private_import_finding(node: ast.AST) -> list[LatentFinding]:
    """Private-symbol imports: `from pkg import _x` / `from pkg._sub import x`
    (ImportFrom) and `import pkg._internal` (plain Import with an underscore
    segment) — both are importing internals by name."""
    findings: list[LatentFinding] = []
    if isinstance(node, ast.ImportFrom):
        if node.module == "__future__":
            return []
        private_module = node.module and any(seg.startswith("_") for seg in node.module.split("."))
        for alias in node.names:
            if alias.name.startswith("_") or private_module:
                target = alias.name if alias.name.startswith("_") else f"{node.module}.{alias.name}"
                findings.append(
                    LatentFinding(
                        signal="private-import",
                        function="",
                        line=node.lineno,
                        metric=1,
                        detail=f"imports private symbol '{target}' at line {node.lineno} — "
                        f"never import underscore symbols: make the logic public and documented, "
                        f"or extract it to a shared module",
                        inner=[],
                    )
                )
        return findings
    for alias in node.names:
        if any(seg.startswith("_") for seg in alias.name.split(".")):
            findings.append(
                LatentFinding(
                    signal="private-import",
                    function="",
                    line=node.lineno,
                    metric=1,
                    detail=f"imports private path '{alias.name}' at line {node.lineno} — "
                    f"never import underscore symbols: make the logic public and documented, "
                    f"or extract it to a shared module",
                    inner=[],
                )
            )
    return findings


def _except_try_findings(node, parents) -> list[LatentFinding]:
    """A catch must fail fast: bare except, an empty body, or a body that never
    raises or surfaces a return swallows the error. Logging alone is not
    fail-fast — the only sanctioned swallow is an explicitly safe-to-ignore
    error, marked `# code-health: ignore except <why>` and logged.
    """
    fn = _enclosing_function(parents, node)
    returned = _returned_names(fn) if fn is not None else set()
    findings: list[LatentFinding] = []
    for h in node.handlers:
        if _handler_swallows(h, returned):
            kind = "bare except" if h.type is None else "except that swallows"
            findings.append(
                LatentFinding(
                    signal="except",
                    function="",
                    line=h.lineno,
                    metric=1,
                    detail=f"{kind} at line {h.lineno} — the catch never raises, returns, or surfaces "
                    f"the error, so it is invisible. Logging alone is not fail-fast: re-raise, "
                    f"surface it, or if this error is genuinely safe to ignore, mark "
                    f"`# code-health: ignore except <why>` and log with that explanation",
                    inner=[],
                )
            )
    return findings


def _broad_except_try_findings(node, parents) -> list[LatentFinding]:
    """except Exception/BaseException catches too broadly — name the specific one.

    Empty/bare handlers are already fail-tier; this warn covers handlers with
    a body that would otherwise pass.
    """
    fn = _enclosing_function(parents, node)
    returned = _returned_names(fn) if fn is not None else set()
    findings: list[LatentFinding] = []
    for h in node.handlers:
        base = _annotation_base_name(h.type) if h.type else ""
        if base in ("Exception", "BaseException") and not _handler_swallows(h, returned):
            findings.append(
                LatentFinding(
                    signal="broad-except",
                    function="",
                    line=h.lineno,
                    metric=1,
                    detail=f"broad `except {ast.unparse(h.type)}` at line {h.lineno} — catch the "
                    f"specific exception; a broad catch hides which failures are expected",
                    inner=[],
                    severity="warn",
                )
            )
    return findings


def _shadow_args_findings(fn) -> list[LatentFinding]:
    args = fn.args.args + fn.args.posonlyargs + fn.args.kwonlyargs
    if fn.args.vararg:
        args = args + [fn.args.vararg]
    if fn.args.kwarg:
        args = args + [fn.args.kwarg]
    findings: list[LatentFinding] = []
    for a in args:
        if a.arg in SHADOWED_BUILTINS:
            findings.append(_shadow_finding(a.arg, fn.lineno, fn.name, "parameter"))
    return findings


def _shadow_assign_findings(node: ast.Assign, parents) -> list[LatentFinding]:
    fn = _enclosing_function(parents, node)
    fn_name = fn.name if fn is not None else ""
    findings: list[LatentFinding] = []
    for target in node.targets:
        if isinstance(target, ast.Name) and target.id in SHADOWED_BUILTINS:
            findings.append(_shadow_finding(target.id, node.lineno, fn_name, "variable"))
    return findings


def _magic_constant_finding(node: ast.Constant, parents) -> LatentFinding | None:
    if isinstance(node.value, bool) or not isinstance(node.value, (int, float)):
        return None
    if node.value in _MAGIC_SKIP or not _has_function_ancestor(node, parents):
        return None
    parent = parents.get(id(node))
    if not isinstance(parent, (ast.BinOp, ast.Compare, ast.UnaryOp, ast.Subscript, ast.Call)):
        return None
    fn = _enclosing_function(parents, node)
    fn_name = fn.name if fn is not None else ""
    return LatentFinding(
        signal="magic-number",
        function=fn_name,
        line=node.lineno,
        metric=1,
        detail=f"magic number {node.value} at line {node.lineno} in '{fn_name}' — name it as a "
        f"constant (the name is the documentation); raw integers as operands "
        f"and indices are a finding",
        inner=[],
        severity="warn",
    )


def _noop_expr_finding(node: ast.Expr, parents) -> LatentFinding | None:
    v = node.value
    if isinstance(v, (ast.Call, ast.Await, ast.Yield, ast.YieldFrom,
                      ast.Constant, ast.Lambda, ast.NamedExpr)):
        return None
    fn = _enclosing_function(parents, node)
    expr = ast.unparse(v)
    return LatentFinding(
        signal="noop-statement",
        function=fn.name if fn is not None else "",
        line=node.lineno,
        metric=1,
        detail=f"no-op statement at line {node.lineno}: `{expr[:60]}` discards its value — dead "
        f"statement, likely a refactor leftover: assign it or delete it",
        inner=[],
    )


def _has_function_ancestor(node: ast.AST, parents: dict[int, ast.AST]) -> bool:
    cur = parents.get(id(node))
    while cur is not None:
        if isinstance(cur, (ast.FunctionDef, ast.AsyncFunctionDef)):
            return True
        cur = parents.get(id(cur))
    return False


def _handler_swallows(h, returned: set[str]) -> bool:
    """True when the handler hides the failure: bare, or a body with no
    control-flow exit (no raise, no return, no break/continue) — the error
    is invisible. An explicit return, even None or an empty literal, is the
    documented contract; a continue is retry/skip semantics; mutating an
    accumulator the enclosing function returns (issues.append, result[0] = )
    surfaces the error the same way."""
    if h.type is None:
        return True
    if any(isinstance(n, (ast.Raise, ast.Return, ast.Break, ast.Continue)) for n in ast.walk(h)):
        return False
    if any(_is_process_exit(n) for n in ast.walk(h)):
        return False  # sys.exit(...) terminates with the error surfaced in the process result
    return not _mutates_returned(h, returned)


def _is_process_exit(node: ast.AST) -> bool:
    """A sys.exit()/exit()/quit() call — the handler terminates, so the
    failure is not invisible (exit code + stderr are the surface)."""
    if isinstance(node, ast.Call) and isinstance(node.func, ast.Name):
        return node.func.id in ("exit", "quit")
    return (isinstance(node, ast.Call) and isinstance(node.func, ast.Attribute)
            and isinstance(node.func.value, ast.Name) and node.func.value.id == "sys"
            and node.func.attr == "exit")


def _mutates_returned(h, returned: set[str]) -> bool:
    """The handler stores into, rebinds, or mutates a name the enclosing
    function returns — the error rides out in the result, not a swallow."""
    if not returned:
        return False
    for node in ast.walk(h):
        if (
            isinstance(node, ast.Call)
            and isinstance(node.func, ast.Attribute)
            and isinstance(node.func.value, ast.Name)
            and node.func.value.id in returned
        ):
            return True
        if isinstance(node, ast.Name) and isinstance(node.ctx, ast.Store) and node.id in returned:
            return True
        if (
            isinstance(node, ast.Subscript)
            and isinstance(node.ctx, (ast.Store, ast.Del))
            and isinstance(node.value, ast.Name)
            and node.value.id in returned
        ):
            return True
    return False


def _returned_names(fn) -> set[str]:
    """Names the function returns at its own top level (nested functions excluded)."""
    names: set[str] = set()
    stack = [fn]
    while stack:
        node = stack.pop()
        if isinstance(node, (ast.FunctionDef, ast.AsyncFunctionDef)) and node is not fn:
            continue
        if isinstance(node, ast.Return) and isinstance(node.value, ast.Name):
            names.add(node.value.id)
        stack.extend(ast.iter_child_nodes(node))
    return names


def _enclosing_function(parents: dict[int, ast.AST], node: ast.AST):
    """The nearest enclosing function of a node, or None at module level."""
    p = parents.get(id(node))
    while p is not None and not isinstance(p, (ast.FunctionDef, ast.AsyncFunctionDef)):
        p = parents.get(id(p))
    return p


def _global_state_findings(tree: ast.Module, rel: str) -> list[LatentFinding]:
    """No module-level mutable state and no global statements. Constant lookup
    tables (all-literal values, module scope) pass — the record gate's rule."""
    findings: list[LatentFinding] = []
    for node in ast.walk(tree):
        if isinstance(node, ast.Global):
            findings.append(
                LatentFinding(
                    signal="global-state",
                    function="",
                    line=node.lineno,
                    metric=1,
                    detail=f"global statement at line {node.lineno} — no module-level mutable state. The fix: "
                    f"instantiate the object at the entry point and pass it around (parameter injection), "
                    f"or keep ONE global services object that is set at the entry point or in test setup "
                    f"and populated with whatever objects it needs — fakes in tests",
                    inner=[],
                )
            )
    flagged: set[str] = set()
    for node in tree.body:
        target = _module_assignment_target(node)
        if target is None:
            continue
        if isinstance(node.value, (ast.List, ast.Dict, ast.Set)) and not _all_constant(node.value):
            flagged.add(target)
            findings.append(
                LatentFinding(
                    signal="global-state",
                    function="",
                    line=node.lineno,
                    metric=1,
                    detail=f"module-level mutable {type(node.value).__name__} '{target}' at line {node.lineno} — "
                    f"no module-level mutable state. The fix: instantiate the object at the entry point and "
                    f"pass it around (parameter injection), or keep ONE global services object set at the "
                    f"entry point / test setup and populated with what it needs — fakes in tests",
                    inner=[],
                )
            )
    _mutation_findings(tree, flagged, findings)
    return findings


def _module_assignment_target(node: ast.AST) -> str | None:
    """A single plain-name module-level assignment target (Assign or AnnAssign)."""
    if isinstance(node, ast.Assign) and len(node.targets) == 1 and isinstance(node.targets[0], ast.Name):
        return node.targets[0].id
    if isinstance(node, ast.AnnAssign) and node.value is not None and isinstance(node.target, ast.Name):
        return node.target.id
    return None


def _mutation_findings(tree: ast.Module, flagged: set[str], findings: list[LatentFinding]) -> None:
    """Module-level collections reassigned or mutated inside functions are
    still module state, even when the literal itself looks constant
    (`_oauth_states: dict = {}` populated by login/callback)."""
    mutable = {
        _module_assignment_target(n)
        for n in tree.body
        if isinstance(getattr(n, "value", None), (ast.List, ast.Dict, ast.Set))
    } - {None}
    seen: set[str] = set()
    for fn in [n for n in ast.walk(tree) if isinstance(n, (ast.FunctionDef, ast.AsyncFunctionDef))]:
        for node in ast.walk(fn):
            container = _mutation_target(node, mutable)
            if container and container not in seen and container not in flagged:
                seen.add(container)
                findings.append(
                    LatentFinding(
                        signal="global-state",
                        function="",
                        line=node.lineno,
                        metric=1,
                        detail=f"module-level collection '{container}' is mutated at line {node.lineno} — "
                        f"no module-level mutable state. The fix: instantiate the object at the "
                        f"entry point and pass it around (parameter injection), or keep ONE global "
                        f"services object set at the entry point / test setup and populated with "
                        f"what it needs — fakes in tests",
                        inner=[],
                    )
                )


def _mutation_target(node: ast.AST, mutable: set[str]) -> str | None:
    """The module-level container this node mutates: a rebinding or an
    in-place subscript store/del, or None when the node mutates nothing."""
    if isinstance(node, ast.Name) and isinstance(node.ctx, ast.Store) and node.id in mutable:
        return node.id
    if (
        isinstance(node, ast.Subscript)
        and isinstance(node.ctx, (ast.Store, ast.Del))
        and isinstance(node.value, ast.Name)
        and node.value.id in mutable
    ):
        return node.value.id
    return None


def _all_constant(node: ast.AST) -> bool:
    """A literal subtree of only constants — a constant lookup table, not state."""
    if isinstance(node, ast.Constant):
        return True
    if isinstance(node, ast.UnaryOp) and isinstance(node.op, (ast.USub, ast.UAdd)):
        return _all_constant(node.operand)  # -4.0 parses as UnaryOp, still a literal
    return _container_all_constant(node)


def _container_all_constant(node: ast.AST) -> bool:
    """Containers whose parts are all constant literals."""
    if isinstance(node, (ast.List, ast.Set)):
        return bool(node.elts) and all(_all_constant(e) for e in node.elts)
    if isinstance(node, ast.Tuple):
        return all(_all_constant(e) for e in node.elts)
    if isinstance(node, ast.Dict):
        return (
            bool(node.keys)
            and all(k is not None and _all_constant(k) for k in node.keys)
            and all(_all_constant(v) for v in node.values)
        )
    return False


SHADOWED_BUILTINS = frozenset(
    {
        "abs",
        "all",
        "any",
        "bin",
        "bool",
        "bytes",
        "callable",
        "chr",
        "classmethod",
        "complex",
        "dict",
        "dir",
        "divmod",
        "enumerate",
        "eval",
        "exec",
        "filter",
        "float",
        "format",
        "frozenset",
        "getattr",
        "globals",
        "hasattr",
        "hash",
        "hex",
        "id",
        "input",
        "int",
        "isinstance",
        "issubclass",
        "iter",
        "len",
        "list",
        "locals",
        "map",
        "max",
        "memoryview",
        "min",
        "next",
        "object",
        "oct",
        "open",
        "ord",
        "pow",
        "print",
        "property",
        "range",
        "repr",
        "reversed",
        "round",
        "set",
        "setattr",
        "slice",
        "sorted",
        "staticmethod",
        "str",
        "sum",
        "super",
        "tuple",
        "type",
        "vars",
        "zip",
    }
)


def _unreachable_findings(tree: ast.Module, rel: str) -> list[LatentFinding]:
    """Statements after an unconditional return/raise/continue/break are dead code."""
    findings: list[LatentFinding] = []
    for fn in [n for n in ast.walk(tree) if isinstance(n, (ast.FunctionDef, ast.AsyncFunctionDef))]:
        for body in _statement_lists(fn):
            for i, stmt in enumerate(body[:-1]):
                if isinstance(stmt, (ast.Return, ast.Raise, ast.Continue, ast.Break)):
                    dead = body[i + 1]
                    findings.append(
                        LatentFinding(
                            signal="unreachable",
                            function=fn.name,
                            line=dead.lineno,
                            metric=1,
                            detail=f"unreachable statement at line {dead.lineno} in '{fn.name}' — "
                            f"it follows an unconditional {type(stmt).__name__.lower()} and can "
                            f"never run; dead code is deleted, not kept",
                            inner=[],
                        )
                    )
                    break
    return findings


def _statement_lists(fn) -> StatementBlocks:
    """All statement lists inside fn, excluding nested function bodies."""
    lists: list[list[ast.stmt]] = [fn.body]

    def walk(node: ast.AST) -> None:
        if isinstance(node, (ast.FunctionDef, ast.AsyncFunctionDef)) and node is not fn:
            return
        for child in ast.iter_child_nodes(node):
            if isinstance(child, ast.stmt) and isinstance(getattr(child, "body", None), list):
                lists.append(child.body)
            walk(child)

    walk(fn)
    return lists


_MAGIC_SKIP = (0, 1, 2, -1)


BUILTIN_NAMES = {n for n in dir(builtins) if not n.startswith("_")}


def _shadow_finding(name: str, line: int, fn: str, kind: str) -> LatentFinding:
    return LatentFinding(
        signal="builtin-shadow",
        function=fn,
        line=line,
        metric=1,
        detail=f"{kind} '{name}' at line {line} in '{fn}' shadows a builtin — rename it; a "
        f"shadowed builtin makes the code read wrong (the name needing qualification is "
        f"a failed name)",
        inner=[],
    )


def _tuple_alias_findings(tree: ast.Module, rel: str) -> list[LatentFinding]:
    """A type alias for a fixed-size tuple hides a positional record.

    Each element has a meaning the alias erases (the standard: GeoPoint, not
    LatLngPair) — 'Key = tuple[str, str]' hides which element is which. A
    variadic tuple (tuple[str, ...]) is a homogeneous sequence, not a record.
    """
    findings: list[LatentFinding] = []
    for node in tree.body:
        if not isinstance(node, ast.Assign) or len(node.targets) != 1 or not isinstance(node.targets[0], ast.Name):
            continue
        v = node.value
        if not isinstance(v, ast.Subscript) or _annotation_base_name(v.value).lower() != "tuple":
            continue
        parts = v.slice
        elts = parts.elts if isinstance(parts, ast.Tuple) else [parts]
        if len(elts) >= 2 and not any(isinstance(e, ast.Constant) and e.value is Ellipsis for e in elts):
            findings.append(
                LatentFinding(
                    signal="tuple-alias",
                    function="",
                    line=node.lineno,
                    metric=1,
                    detail=f"alias '{node.targets[0].id} = {ast.unparse(v)}' hides a positional record — "
                    f"each element has a meaning the alias erases (the standard: GeoPoint, not "
                    f"LatLngPair). Make a class with named fields so the reader sees which element "
                    f"is which",
                    inner=[],
                )
            )
    return findings


def _type_ignore_findings(source: str, rel: str) -> list[LatentFinding]:
    """A # type: ignore is itself a finding; it requires a comment explaining why.

    tokenize finds real COMMENT tokens, so '# type: ignore' inside a string
    or docstring is never a false positive.
    """
    findings: list[LatentFinding] = []
    try:
        tokens = list(tokenize.generate_tokens(io.StringIO(source).readline))
    except (IndentationError, tokenize.TokenError):
        return findings
    for tok in tokens:
        if tok.type != tokenize.COMMENT or "type: ignore" not in tok.string:
            continue
        rest = tok.string.split("type: ignore", 1)[1]
        if "#" not in rest:  # a second comment on the line is the why
            findings.append(
                LatentFinding(
                    signal="type-ignore",
                    function="",
                    line=tok.start[0],
                    metric=1,
                    detail=f"# type: ignore at line {tok.start[0]} without a why — a suppression is itself a "
                    f"finding: add a comment explaining why the checker is wrong",
                    inner=[],
                )
            )
    return findings


_MONKEYPATCH_METHODS = ("setattr", "setitem", "delattr", "setenv", "delenv")


SUPPRESSION_RE = re.compile(r"# code-health: ignore\s+(\S+)\s*(.*)")
FILE_SUPPRESSION_RE = re.compile(r"# code-health: ignore-file\s+(\S+)\s*(.*)")


def _suppressions(source: str) -> dict[int, tuple[str, str]]:
    """Lint-style exemptions: `# code-health: ignore <signal> <why>` per line.

    The why is required — a suppression without an explanation is itself a
    finding. Matches the standard's only sanctioned swallow: an explicitly
    safe-to-ignore error with an explanation of why it is safe. tokenize
    reads real COMMENT tokens, so marker text inside a string is not a
    suppression.
    """
    out: dict[int, tuple[str, str]] = {}
    try:
        tokens = list(tokenize.generate_tokens(io.StringIO(source).readline))
    except (IndentationError, tokenize.TokenError):
        return out
    for tok in tokens:
        if tok.type != tokenize.COMMENT:
            continue
        m = SUPPRESSION_RE.search(tok.string)
        if m:
            out[tok.start[0]] = (m.group(1), m.group(2).strip())
    return out


def _file_suppressions(source: str) -> FileSuppressions:
    """File-scoped exemptions: `# code-health: ignore-file <signal> <why>`.

    An ignore-file without a why is itself a finding, like the line-level
    suppression. Only real comments count."""
    out: dict[str, str] = {}
    invalid: list[InvalidFileSuppression] = []
    try:
        tokens = list(tokenize.generate_tokens(io.StringIO(source).readline))
    except (IndentationError, tokenize.TokenError):
        return FileSuppressions(out, invalid)
    for tok in tokens:
        if tok.type != tokenize.COMMENT:
            continue
        m = FILE_SUPPRESSION_RE.search(tok.string)
        if m:
            if m.group(2).strip():
                out[m.group(1)] = m.group(2).strip()
            else:
                invalid.append(InvalidFileSuppression(tok.start[0], m.group(1)))
    return FileSuppressions(out, invalid)


def _suppressed(signal: str, line: int, supps: dict[int, tuple[str, str]]) -> bool:
    """A finding is exempt when its line (or the line above) carries an
    explained suppression for that signal."""
    for ln in (line, line - 1):
        entry = supps.get(ln)
        if entry and entry[0] == signal and entry[1]:
            return True
    return False


def _class_module_findings(tree: ast.Module, rel: str) -> list[LatentFinding]:
    """Each class lives in its own module named after the class.

    A module with exactly one top-level class whose name does not match the
    file is the finding; a module grouping several closely-related models
    (the standard's own exception) passes.
    """
    if rel.endswith("__init__.py"):
        return []
    classes = [n for n in tree.body if isinstance(n, ast.ClassDef)]
    if len(classes) != 1:
        return []
    cls = classes[0]
    stem = Path(rel).stem.lower()
    if cls.name.lower() == stem or cls.name.lower() == stem.replace("_", ""):
        return []
    return [
        LatentFinding(
            signal="class-module",
            function=cls.name,
            line=cls.lineno,
            metric=1,
            detail=f"module '{rel}' holds one class '{cls.name}' — each class lives in its own module named "
            f"after the class; rename the file to {cls.name.lower()}.py (exception: a module grouping "
            f"closely related models that share one reason to change)",
            inner=[],
        )
    ]


def _skipif_findings(tree: ast.Module, rel: str) -> list[LatentFinding]:
    """Never skip a test for a missing environment — fake it instead.

    @pytest.mark.skipif keyed on environment presence is forbidden; only the
    E2E suite may skip (real external APIs that cannot be faked faithfully).
    """
    findings: list[LatentFinding] = []
    for node in ast.walk(tree):
        if not isinstance(node, ast.Call) or not isinstance(node.func, ast.Attribute):
            continue
        if node.func.attr != "skipif":
            continue
        cond = " ".join(ast.unparse(a) for a in node.args) + " " + " ".join(ast.unparse(k.value) for k in node.keywords)
        if any(needle in cond for needle in ("os.environ", "environ", "getenv", "os.path.exists", "sys.platform")):
            findings.append(
                LatentFinding(
                    signal="skipif",
                    function="",
                    line=node.lineno,
                    metric=1,
                    detail=f"@pytest.mark.skipif on environment presence at line {node.lineno} — never skip a "
                    f"test for a missing dependency: fake it (a fixture builds a stand-in) so it runs "
                    f"identically everywhere; only the E2E suite may skip",
                    inner=[],
                )
            )
    return findings


def _monkeypatch_findings(tree: ast.Module, rel: str) -> list[LatentFinding]:
    """monkeypatch/unittest.mock.patch is forbidden — inject object fakes instead.

    Fakes are objects (a class implementing the real protocol), never patched
    globals and never bare functions swapped in.
    """
    mock_imports = {
        a.name for n in tree.body if isinstance(n, ast.ImportFrom) and n.module and "mock" in n.module for a in n.names
    }
    return _mock_calls(tree, mock_imports) + _mock_decorators(tree, mock_imports)


def _mp_finding(desc: str, line: int) -> LatentFinding:
    return LatentFinding(
        signal="monkeypatch",
        function="",
        line=line,
        metric=1,
        detail=f"{desc} at line {line} — never monkeypatch global state; inject an object fake "
        f"(a class implementing the real protocol) via parameter injection or the services "
        f"container — fakes are objects, not functions",
        inner=[],
    )


def _mock_calls(tree: ast.Module, mock_imports: set[str]) -> list[LatentFinding]:
    findings: list[LatentFinding] = []
    for node in ast.walk(tree):
        if not isinstance(node, ast.Call):
            continue
        func = node.func
        if isinstance(func, ast.Attribute):
            if (
                isinstance(func.value, ast.Name)
                and func.value.id == "monkeypatch"
                and func.attr in _MONKEYPATCH_METHODS
            ):
                findings.append(_mp_finding(f"monkeypatch.{func.attr}", node.lineno))
            elif func.attr == "patch" and isinstance(func.value, ast.Attribute):
                findings.append(_mp_finding(f"{ast.unparse(func.value)}.patch", node.lineno))
        elif isinstance(func, ast.Name) and func.id in mock_imports and func.id == "patch":
            findings.append(_mp_finding("patch", node.lineno))
    return findings


def _mock_decorators(tree: ast.Module, mock_imports: set[str]) -> list[LatentFinding]:
    findings: list[LatentFinding] = []
    for fn in [n for n in ast.walk(tree) if isinstance(n, (ast.FunctionDef, ast.AsyncFunctionDef, ast.ClassDef))]:
        for dec in fn.decorator_list:
            func = dec.func if isinstance(dec, ast.Call) else dec
            if isinstance(func, ast.Name) and func.id in mock_imports:
                findings.append(_mp_finding(f"@{func.id}", getattr(dec, "lineno", 0)))
            elif isinstance(func, ast.Attribute) and func.attr == "patch":
                findings.append(_mp_finding("@patch", getattr(dec, "lineno", 0)))
    return findings


_FS_PATH_METHODS = (
    "read_text",
    "write_text",
    "read_bytes",
    "write_bytes",
    "mkdir",
    "unlink",
    "rename",
    "replace",
    "touch",
    "rmdir",
    "iterdir",
    "glob",
    "rglob",
    "exists",
    "resolve",
    "symlink_to",
    "copy",
)
_FS_OS_OPS = ("remove", "rename", "mkdir", "makedirs", "rmdir", "unlink", "symlink", "link", "replace")
_FS_SHUTIL_OPS = ("copy", "copy2", "move", "rmtree", "copytree")
_FS_TEMPFILE = ("TemporaryDirectory", "NamedTemporaryFile", "TemporaryFile", "mkdtemp", "mkstemp", "mktemp")


def _fakefs_findings(tree: ast.Module, rel: str) -> list[LatentFinding]:
    """Tests fake the filesystem (pyfakefs) — real FS access without it is a finding.

    Per the testing standard: file I/O uses the `fs` fixture or
    fake_filesystem_unittest. Real FS is sanctioned only when the code under
    test needs real semantics — subprocess interop, symlinks, C-level I/O
    like sqlite3 — and that usage is present in the test.
    """
    findings: list[LatentFinding] = []
    uses_fakefs_base = _uses_fakefs_base(tree)
    for fn in [n for n in ast.walk(tree) if isinstance(n, (ast.FunctionDef, ast.AsyncFunctionDef))]:
        finding = _fakefs_finding_for_fn(fn, tree, uses_fakefs_base)
        if finding:
            findings.append(finding)
    return findings


def _uses_fakefs_base(tree: ast.Module) -> bool:
    """pyfakefs in use: the module imports it, or a test class bases on it."""
    for n in tree.body:
        if isinstance(n, ast.ImportFrom) and n.module and "pyfakefs" in n.module:
            return True
        if isinstance(n, ast.Import) and any("pyfakefs" in a.name for a in n.names):
            return True
    for cls in [n for n in tree.body if isinstance(n, ast.ClassDef)]:
        for b in cls.bases:
            name = ast.unparse(b).lower()
            if "fakefs" in name or "fake_filesystem" in name:
                return True
    return False


def _fakefs_finding_for_fn(fn, tree: ast.Module, uses_fakefs_base: bool) -> LatentFinding | None:
    """One test function: real-FS usage without pyfakefs and without a
    sanctioned real-FS need is a finding."""
    if not _test_function(fn, tree) or _uses_fakefs(fn) or uses_fakefs_base:
        return None
    if not _real_fs_usage(fn) or _needs_real_fs(fn):
        return None
    return LatentFinding(
        signal="fakefs",
        function=fn.name,
        line=fn.lineno,
        metric=1,
        detail=f"test '{fn.name}' at line {fn.lineno} touches the real filesystem "
        f"(tmp_path/open/Path) without pyfakefs — tests fake the filesystem (the `fs` "
        f"fixture or fake_filesystem_unittest). Reach a real tmp_path only when the code "
        f"under test needs real FS semantics (subprocess interop, symlinks, C-level I/O "
        f"like sqlite3) and comment why — or mark `# code-health: ignore-file fakefs <why>`",
        inner=[],
    )


def _test_function(fn, tree: ast.Module) -> bool:
    """A test: name starts with test_, or a method of a test class."""
    if fn.name.startswith("test_"):
        return True
    return any(
        isinstance(n, ast.ClassDef) and n.name.lower().startswith("test") and any(m is fn for m in n.body)
        for n in tree.body
    )


def _uses_fakefs(fn) -> bool:
    return any(a.arg == "fs" for a in fn.args.args)


def _real_fs_usage(fn) -> bool:
    for node in ast.walk(fn):
        if isinstance(node, ast.Name) and node.id == "tmp_path":
            return True
        if isinstance(node, ast.Call):
            func = node.func
            if isinstance(func, ast.Name) and func.id in ("open", "tempfile"):
                return True
            if isinstance(func, ast.Attribute):
                if func.attr in _FS_PATH_METHODS or func.attr in _FS_OS_OPS:
                    return True
                if func.attr in _FS_SHUTIL_OPS or func.attr in _FS_TEMPFILE:
                    return True
    return False


def _needs_real_fs(fn) -> bool:
    """The standard's sanctioned real-FS cases: subprocess, symlinks, sqlite3 (C-level I/O)."""
    for node in ast.walk(fn):
        if isinstance(node, ast.Name) and node.id in ("sqlite3", "subprocess"):
            return True
        if isinstance(node, ast.Attribute) and node.attr in ("symlink", "symlink_to", "link"):
            return True
    return False


def _strewing_findings(tree: ast.Module, rel: str) -> list[LatentFinding]:
    """3+ free functions sharing a leading parameter that is a class defined in
    THIS module — a missed class. Stdlib-param strewing (Path, str) is not the
    'same record' pattern the rule means, so it is left alone."""
    findings: list[LatentFinding] = []
    class_names = {n.name for n in tree.body if isinstance(n, ast.ClassDef)}
    groups: dict[str, list[tuple[str, int]]] = defaultdict(list)
    for node in tree.body:
        if not isinstance(node, (ast.FunctionDef, ast.AsyncFunctionDef)) or not node.args.args:
            continue
        ann = node.args.args[0].annotation
        base = _annotation_base_name(ann)
        if base in class_names:
            groups[base].append((node.name, node.lineno))
    for ann_base, members in sorted(groups.items()):
        if len(members) >= 3:
            names = ", ".join(f"{n} (line {ln})" for n, ln in sorted(members, key=lambda m: m[1]))
            findings.append(
                LatentFinding(
                    signal="strewing",
                    function="",
                    line=members[0][1],
                    metric=len(members),
                    detail=f"{len(members)} free functions share leading parameter '{ann_base}' — "
                    f"a {ann_base} class is missing (function strewing is a missed class): {names}",
                    inner=[n for n, _ in members],
                )
            )
    return findings


def _annotation_base_name(ann: ast.expr | None) -> str:
    """Root name of an annotation: Subscript -> its value's name; Name -> id;
    Attribute -> the attribute (abc.ABC -> ABC)."""
    if ann is None:
        return ""
    if isinstance(ann, ast.Subscript):
        return _annotation_base_name(ann.value)
    if isinstance(ann, ast.Name):
        return ann.id
    if isinstance(ann, ast.Attribute):
        return ann.attr
    return ""


def _latent_action(
    repo: Path, rel: str, finding: LatentFinding, file_churn: Counter[str], last_modified: dict[str, str]
) -> Action:
    churn = file_churn.get(rel, 0)
    if finding.signal in ("closures", "partition", "strewing"):
        kind = "latent-class"
    elif finding.signal == "vague-name":
        kind = "vague-name"
    elif finding.signal in ("docs-link", "docs-undiscoverable"):
        kind = "docs"
    elif finding.signal == "folder-mix":
        kind = "folder-mix"
    elif finding.signal == "layer-mix":
        kind = "layer-mix"
    else:
        kind = "standard"  # including suppression and over-abstraction signals
    guidance_key = "over-abstraction" if finding.signal == "over-abstraction" else kind
    return Action(
        kind=kind,
        severity=finding.severity,
        file=rel,
        line=finding.line,
        function=finding.function,
        message=f"{finding.detail} — {GUIDANCE[guidance_key]}",
        metric=finding.metric,
        churn=churn,
        last_modified=last_modified.get(rel, ""),
        tested="",
        raw=_raw_score("latent-class", finding.metric, churn),
    )


ABSTRACT_DECORATORS = ("abstractmethod", "abstractproperty", "abstractclassmethod", "abstractstaticmethod")
MD_LINK_RE = re.compile(r"\[([^\]]*)\]\(([^)]+)\)")
MD_FENCE_RE = re.compile(r"^```", re.MULTILINE)
MD_BACKTICK_RE = re.compile(r"`([A-Za-z0-9_./*-]+\.md)`")
MD_SKIP_PREFIXES = ("http://", "https://", "#", "mailto:", "tel:", "skill://", "rule://", "agent://", "memory://", "artifact://")


def _abstraction_actions(
    repo: Path, include_tests: bool, file_churn: Counter[str], last_modified: dict[str, str]
) -> list[Action]:
    """Repo-wide: an abstract base class with exactly one concrete subclass is ceremony."""
    scan = _collect_classes(repo, include_tests)
    concrete = _concrete_counts(scan.classes, scan.imports, scan.rels)
    actions: list[Action] = []
    for ref, cls in scan.classes.items():
        if not _is_abstract(cls):
            continue
        subs = concrete.get(ref, [])
        if len(subs) == 1:
            actions.append(
                _latent_action(
                    repo,
                    ref.file,
                    LatentFinding(
                        signal="over-abstraction",
                        function=ref.name,
                        line=cls.lineno,
                        metric=1,
                        detail=f"abstract class '{ref.name}' in {ref.file} has exactly one concrete subclass "
                        f"('{subs[0]}') — an ABC with a single implementation is ceremony: fold the "
                        f"subclass into the base or drop the ABC; an abstraction earns its keep at "
                        f"two real, differing implementations",
                        inner=subs,
                    ),
                    file_churn,
                    last_modified,
                )
            )
    return actions


def _collect_classes(repo: Path, include_tests: bool) -> ClassScan:
    """One pass over the repo: every top-level class, its module's import map, and the class list."""
    classes: dict[ClassRef, ast.ClassDef] = {}
    imports: dict[str, ImportAliases] = {}
    rels: list[ClassRef] = []
    for sf in _py_files(repo):
        py, rel = sf.py, sf.rel
        if not include_tests and ("/test" in f"/{rel}" or rel.startswith("test")):
            continue
        try:
            tree = ast.parse(py.read_text(encoding="utf-8", errors="replace"))
        except (
            SyntaxError,
            UnicodeDecodeError,
        ):  # code-health: ignore except an unparseable file is skipped, not a scan failure
            continue
        imports[rel] = _import_map(tree)
        for cls in [n for n in tree.body if isinstance(n, ast.ClassDef)]:
            classes[ClassRef(rel, cls.name)] = cls
            rels.append(ClassRef(rel, cls.name))
    return ClassScan(classes, imports, rels)


def _concrete_counts(
    classes: dict[ClassRef, ast.ClassDef], imports: dict[str, ImportAliases], rels: list[ClassRef]
) -> dict[ClassRef, list[str]]:
    """Abstract class -> its concrete subclasses, resolved via imports."""
    concrete: dict[ClassRef, list[str]] = defaultdict(list)
    for ref in rels:
        for base in classes[ref].bases:
            for cand in _resolve_base(ref.file, base, imports):
                key = _class_key(classes, cand.file, cand.name)
                if key and _is_abstract(classes[key]) and key != ref:
                    concrete[key].append(ref.name)
    return concrete


def _import_map(tree: ast.Module) -> ImportAliases:
    """local name -> the ImportedSymbol it refers to, from top-level imports."""
    out: ImportAliases = {}
    for node in ast.walk(tree):
        if isinstance(node, ast.ImportFrom) and node.module:
            for a in node.names:
                out[a.asname or a.name] = ImportedSymbol(node.module, a.name)
        elif isinstance(node, ast.Import):
            for a in node.names:
                out[a.asname or a.name] = ImportedSymbol(a.name, a.name)
    return out


def _is_abstract(cls: ast.ClassDef) -> bool:
    for dec in cls.decorator_list:
        if isinstance(dec, ast.Name) and dec.id in ABSTRACT_DECORATORS:
            return True
        if isinstance(dec, ast.Attribute) and dec.attr in ABSTRACT_DECORATORS:
            return True
    return any(_annotation_base_name(b) in ("ABC", "ABCMeta") for b in cls.bases)


def _resolve_base(crel: str, base: ast.expr, imports: dict[str, ImportAliases]) -> list[ClassRef]:
    """(module, name) candidates for a base: same module, or via imports."""
    candidates: list[ClassRef] = []
    if isinstance(base, ast.Name):
        candidates.append(ClassRef(crel, base.id))
        entry = imports.get(crel, {}).get(base.id)
        if entry:
            candidates.append(ClassRef(entry.module, entry.name))
    elif isinstance(base, ast.Attribute) and isinstance(base.value, ast.Name):
        entry = imports.get(crel, {}).get(base.value.id)
        if entry:
            candidates.append(ClassRef(entry.module, base.attr))
    return candidates


def _class_key(classes: dict[ClassRef, ast.ClassDef], mrel: str, mname: str) -> ClassRef | None:
    """Resolve a module reference to a file rel: an already-file rel, or a
    dotted module to base.py / base/__init__.py."""
    if mrel.endswith(".py"):
        ref = ClassRef(mrel, mname)
        return ref if ref in classes else None
    base = mrel.replace(".", "/")
    for candidate in (f"{base}.py", f"{base}/__init__.py"):
        ref = ClassRef(candidate, mname)
        if ref in classes:
            return ref
    return None


def _md_link_targets(text: str) -> list[str]:
    """Relative markdown link targets, skipping fences and external/scheme links."""
    targets: list[str] = []
    in_fence = False
    for line in text.splitlines():
        if MD_FENCE_RE.match(line):
            in_fence = not in_fence
            continue
        if in_fence:
            continue
        for m in MD_LINK_RE.finditer(line):
            target = m.group(2).strip()
            target = target.split("#", 1)[0]  # strip the anchor — docs/PRD.md#goals is docs/PRD.md
            if target and not target.startswith(MD_SKIP_PREFIXES):
                targets.append(target)
    return targets


def _md_backtick_paths(text: str) -> list[str]:
    """Backticked .md paths OUTSIDE code fences (a fence's command examples
    are not doc references)."""
    paths: list[str] = []
    in_fence = False
    for line in text.splitlines():
        if MD_FENCE_RE.match(line):
            in_fence = not in_fence
            continue
        if in_fence:
            continue
        for m in MD_BACKTICK_RE.finditer(line):
            path = m.group(1).split("#", 1)[0]
            if path:
                paths.append(path)
    return paths


def _duplicate_actions(
    repo: Path, include_tests: bool, file_churn: Counter[str], last_modified: dict[str, str]
) -> list[Action]:
    """Copy-paste: functions with near-identical structural skeletons.

    The skeleton collapses names, constants, and arguments to placeholders,
    so copy-paste with renames or tweaked values keeps the same shape.
    Dice similarity on consecutive skeleton types; warn tier, because two
    legitimately same-shaped functions (e.g. two endpoints) can match.
    """
    fns = _collect_functions(repo)
    actions: list[Action] = []
    for i, fr in enumerate(fns):
        dup = _first_duplicate(fns, i, fr.skeleton)
        if dup:
            actions.append(
                _latent_action(
                    repo,
                    dup.rel,
                    LatentFinding(
                        signal="duplicate",
                        function=dup.name,
                        line=dup.line,
                        metric=1,
                        detail=f"function '{dup.name}' ({dup.rel}:{dup.line}) is {dup.sim:.0%} similar to "
                        f"'{fr.name}' ({fr.rel}:{fr.line}) — copy-paste; extract the shared logic "
                        f"into one function",
                        inner=[],
                        severity="warn",
                    ),
                    file_churn,
                    last_modified,
                )
            )
    return actions


def _collect_functions(repo: Path) -> list[FunctionRecord]:
    """Every function with a 12+ token skeleton, for the duplication search."""
    fns: list[FunctionRecord] = []
    for sf in _py_files(repo):
        py, rel = sf.py, sf.rel
        if "/test" in f"/{rel}" or rel.startswith("test"):
            continue
        try:
            tree = ast.parse(py.read_text(encoding="utf-8", errors="replace"))
        except (
            SyntaxError,
            UnicodeDecodeError,
        ):  # code-health: ignore except an unparseable file is skipped, not a scan failure
            continue
        for fn in [n for n in ast.walk(tree) if isinstance(n, (ast.FunctionDef, ast.AsyncFunctionDef))]:
            if _is_duplicate_candidate(fn):
                fns.append(FunctionRecord(rel, fn.name, fn.lineno, _fn_skeleton(fn)))
    return fns


def _is_duplicate_candidate(fn) -> bool:
    """A function worth comparing: at least two real statements (one-line
    accessors, stubs, and delegation wrappers are not copy-paste) and a
    12+ token skeleton."""
    if fn.name == "__init__":
        return False  # init boilerplate is conventional, not copy-paste
    stmts = [
        s
        for s in fn.body
        if not (isinstance(s, ast.Expr) and isinstance(s.value, ast.Constant) and isinstance(s.value.value, str))
    ]
    return len(stmts) >= 2 and len(_fn_skeleton(fn)) >= 12


@dataclass(frozen=True)
class DuplicateMatch:
    """A later function at least 90% structurally similar to the one before it."""

    rel: str
    name: str
    line: int
    sim: float


def _first_duplicate(fns: list[FunctionRecord], i: int, toks: list[str]) -> DuplicateMatch | None:
    """The first later function at least 90% structurally similar to fns[i]."""
    for j in range(i + 1, len(fns)):
        other = fns[j]
        if abs(len(toks) - len(other.skeleton)) > max(2, len(toks) // 5):
            continue
        sim = _dice_similarity(toks, other.skeleton)
        if sim >= 0.9:
            return DuplicateMatch(other.rel, other.name, other.line, sim)
    return None


def _fn_skeleton(fn) -> list[str]:
    """Structural fingerprint: node types, with names/constants/args collapsed."""
    toks: list[str] = []
    for node in ast.walk(fn):
        if isinstance(node, ast.Name):
            toks.append("N")
        elif isinstance(node, ast.Constant):
            toks.append("C")
        elif isinstance(node, ast.arg):
            toks.append("A")
        else:
            toks.append(type(node).__name__)
    return toks


def _dice_similarity(a: list[str], b: list[str]) -> float:
    sa = set(zip(a, a[1:], strict=False))
    sb = set(zip(b, b[1:], strict=False))
    if not sa and not sb:
        return 0.0
    return 2 * len(sa & sb) / (len(sa) + len(sb))


def _unused_actions(
    repo: Path, include_tests: bool, file_churn: Counter[str], last_modified: dict[str, str]
) -> list[Action]:
    """Module-level functions defined but never referenced — dead code.

    Referenced = any Name, any import alias, or a mention in a string
    literal (CLI commands dispatched by name). 'main' entry points pass.
    Warn tier: attribute-style calls (mod.fn()) and public API used by
    other repos can false-positive.
    """
    scan = _collect_references(repo)
    actions: list[Action] = []
    for rel, fns in scan.definitions.items():
        for name, line in fns.items():
            if name == "main" or name in scan.prod_references or any(name in s for s in scan.strings):
                continue
            if name in scan.test_references:
                detail = (
                    f"function '{name}' ({rel}:{line}) is referenced only from tests — if it is "
                    f"a deliberate test seam (isolation hook, fixture helper), document it with "
                    f"`# code-health: ignore unused <why>`; otherwise production code that "
                    f"nothing ships calls is dead — delete it"
                )
            else:
                detail = (
                    f"function '{name}' ({rel}:{line}) is defined but never referenced — dead "
                    f"code is deleted, not kept (unless it is a CLI command or public API "
                    f"entry point)"
                )
            actions.append(
                _latent_action(
                    repo,
                    rel,
                    LatentFinding(
                        signal="unused", function=name, line=line, metric=1, detail=detail, inner=[], severity="warn"
                    ),
                    file_churn,
                    last_modified,
                )
            )
    return actions


def _collect_references(repo: Path) -> ReferenceScan:
    """Module-level definitions and referenced names, split by prod vs test.

    Test references count separately from production ones: a production
    function used only by tests is either a deliberate test seam (document
    it) or dead code — it is not a live production caller.
    """
    defined: dict[str, dict[str, int]] = defaultdict(dict)
    prod_refs: set[str] = set()
    test_refs: set[str] = set()
    strings: list[str] = []
    for sf in _py_files(repo):
        is_test = "/test" in f"/{sf.rel}" or sf.rel.startswith("test")
        tree, _ = SOURCE_CACHE.get(sf.py)
        if tree is None:
            continue
        _collect_file_references(tree, sf.rel, is_test, defined, prod_refs, test_refs, strings)
    return ReferenceScan(defined, prod_refs, test_refs, strings)


def _collect_file_references(tree, rel: str, is_test: bool, defined, prod_refs, test_refs, strings) -> None:
    """One file's contribution to the reference scan: module-level definitions
    (decorated = framework-registered = referenced) and every referenced name,
    aliased import, and string literal, split by prod vs test."""
    for node in tree.body:
        if isinstance(node, (ast.FunctionDef, ast.AsyncFunctionDef)):
            if is_test:
                continue
            defined[rel][node.name] = node.lineno
            if node.decorator_list:
                prod_refs.add(node.name)
    for node in ast.walk(tree):
        if isinstance(node, ast.Name):
            (test_refs if is_test else prod_refs).add(node.id)
        elif isinstance(node, ast.alias):
            (test_refs if is_test else prod_refs).add(node.name)
        elif not is_test and isinstance(node, ast.Constant) and isinstance(node.value, str):
            strings.append(node.value)


def _cycle_actions(repo: Path, file_churn: Counter[str], last_modified: dict[str, str]) -> list[Action]:
    """Import cycles between local modules — always fixed by restructuring.

    Builds a file -> file import graph from the graph's IMPORTS_FROM edges
    (local modules only; stdlib/external skip), then finds strongly-connected
    components with >= 2 files. The fix: hoist the shared interface into its
    own module and have both sides depend on it — never bodge with lazy
    imports. No graph -> no signal.
    """
    conn = _graph_conn(repo)
    if conn is None:
        return []
    rows = conn.execute("SELECT source_qualified, target_qualified FROM edges WHERE kind = 'IMPORTS_FROM'").fetchall()
    conn.close()
    graph: ModuleGraph = defaultdict(set)
    files: set[str] = set()
    for src, tgt in rows:
        src_rel = rel_path(repo, src)
        if not src_rel.endswith(".py"):
            continue
        files.add(src_rel)
        target_rel = _module_to_file(repo, tgt)
        if target_rel and target_rel != src_rel:
            graph[src_rel].add(target_rel)
    actions: list[Action] = []
    for comp in _strongly_connected_components(graph, files):
        if len(comp) < 2:
            continue
        chain = _find_cycle(graph, comp)
        cycle_text = " -> ".join(chain) + " -> " + chain[0] if chain else ", ".join(sorted(comp))
        actions.append(
            _latent_action(
                repo,
                chain[0] if chain else sorted(comp)[0],
                LatentFinding(
                    signal="import-cycle",
                    function="",
                    line=0,
                    metric=len(comp),
                    detail=f"import cycle: {cycle_text} — circular imports are fixed by restructuring "
                    f"modules, never bodged with lazy imports: hoist the shared interface into its "
                    f"own module and have both sides depend on it",
                    inner=sorted(comp),
                ),
                file_churn,
                last_modified,
            )
        )
    return actions


def _module_to_file(repo: Path, dotted: str) -> str | None:
    """A dotted module name to its repo file rel (base.py or base/__init__.py)."""
    base = dotted.replace(".", "/")
    for candidate in (f"{base}.py", f"{base}/__init__.py"):
        if (repo / candidate).exists():
            return candidate
    return None


def _strongly_connected_components(graph: ModuleGraph, nodes: set[str]) -> NameGroups:
    """Iterative Tarjan: strongly-connected components of the module graph."""
    index = 0
    indices: dict[str, int] = {}
    low: dict[str, int] = {}
    stack: list[str] = []
    on_stack: set[str] = set()
    comps: list[list[str]] = []
    work: list[tuple[str, int, list[str]]] = []

    def strongconnect(v: str) -> None:
        nonlocal index
        indices[v] = low[v] = index
        index += 1
        stack.append(v)
        on_stack.add(v)
        work.append((v, 0, list(graph.get(v, ()))))

    for v in nodes:
        if v in indices:
            continue
        strongconnect(v)
        while work:
            v2, i, neighbors = work[-1]
            if i < len(neighbors):
                w = neighbors[i]
                work[-1] = (v2, i + 1, neighbors)
                if w not in indices:
                    strongconnect(w)
                elif w in on_stack:
                    low[v2] = min(low[v2], indices[w])
            else:
                work.pop()
                if low[v2] == indices[v2]:
                    comp = []
                    while True:
                        w = stack.pop()
                        on_stack.discard(w)
                        comp.append(w)
                        if w == v2:
                            break
                    comps.append(comp)
                if work:
                    parent = work[-1][0]
                    low[parent] = min(low[parent], low[v2])
    return comps


def _find_cycle(graph: ModuleGraph, comp: list[str]) -> list[str]:
    """One concrete cycle within an SCC: start -> ... -> back to start."""
    members = set(comp)
    start = sorted(comp)[0]
    path = [start]
    stack = [(start, [start], {start})]
    while stack:
        node, p, s = stack.pop()
        for w in graph.get(node, ()):
            if w not in members:
                continue
            if w == start:
                return p + [w]
            if w not in s:
                stack.append((w, p + [w], s | {w}))
    return path


def _folder_mix_actions(repo: Path, file_churn: Counter[str], last_modified: dict[str, str]) -> list[Action]:
    """A folder whose direct files split across graph communities is a grab bag.

    Each community is a dependency-tied group wanting its own sub-folder.
    Test dirs are excluded (they legitimately mix); the repo root is not a
    folder. No graph -> no signal, like hub-file.
    """
    conn = _graph_conn(repo)
    if conn is None:
        return []
    rows = conn.execute(
        "SELECT file_path, community_id, COUNT(*) c FROM nodes "
        "WHERE community_id IS NOT NULL AND file_path LIKE '%.py' "
        "GROUP BY file_path, community_id"
    ).fetchall()
    best: dict[str, tuple[int, int]] = {}
    for fp, cid, c in rows:
        if fp not in best or c > best[fp][1]:
            best[fp] = (cid, c)
    names = dict(conn.execute("SELECT id, name FROM communities"))
    conn.close()
    dirs: dict[str, list[DirFile]] = defaultdict(list)
    for fp, (cid, _c) in best.items():
        parts = fp.replace("\\", "/").split("/")
        dirs["/".join(parts[:-1])].append(DirFile(parts[-1], cid))
    actions: list[Action] = []
    for d, files in dirs.items():
        rel = rel_path(repo, d)
        finding = _folder_mix_for_dir(rel, files, names)
        if finding:
            actions.append(_latent_action(repo, rel, finding, file_churn, last_modified))
    return actions


def _folder_mix_for_dir(rel: str, files: list[DirFile], names: dict[int, str]) -> LatentFinding | None:
    """One directory's community split, or None when it is not a grab bag."""
    if len(files) < 5 or rel.startswith("tests") or rel in ("", "."):
        return None
    spread: dict[int, list[str]] = defaultdict(list)
    for f in files:
        spread[f.community].append(f.file)
    big = {cid: fns for cid, fns in spread.items() if len(fns) >= 2}
    if len(big) < 2:
        return None
    groups = ", ".join(f"{names.get(cid, cid)} ({', '.join(fns[:4])})" for cid, fns in list(big.items())[:3])
    return LatentFinding(
        signal="folder-mix",
        function="",
        line=0,
        metric=len(files),
        detail=f"folder '{rel}' has {len(files)} files split across {len(big)} graph communities: "
        f"{groups} — the folder mixes concerns; extract a sub-folder per community",
        inner=[],
    )


def _layer_mix_actions(repo: Path, file_churn: Counter[str], last_modified: dict[str, str]) -> list[Action]:
    """A file whose functions partition by dominant callee subsystem mixes layers.

    Each function's dominant resolved external callee module is its layer;
    groups of >= 2 functions calling distinct subsystems are latent modules.
    Functions with no resolved external callees are excluded (plumbing or
    self-contained). No graph -> no signal.
    """
    conn = _graph_conn(repo)
    if conn is None:
        return []
    actions: list[Action] = []
    for sf in _py_files(repo):
        py, rel = sf.py, sf.rel
        if "/test" in f"/{rel}" or rel.startswith("test"):
            continue
        finding = _layer_mix_for_file(conn, repo, py, rel)
        if finding:
            actions.append(_latent_action(repo, rel, finding, file_churn, last_modified))
    conn.close()
    return actions


def _layer_mix_for_file(conn, repo: Path, py: Path, rel: str) -> LatentFinding | None:
    """One file's layer partition, or None when it has no clear split."""
    fns = conn.execute(
        "SELECT qualified_name FROM nodes WHERE file_path = ? AND kind IN ('Function', 'Method')", (str(py.resolve()),)
    ).fetchall()
    if len(fns) < 6:
        return None
    layers: dict[str, list[str]] = defaultdict(list)
    for (qn,) in fns:
        layer = _dominant_callee(conn, repo, qn, rel)
        if layer:
            layers[layer].append(qn.split("::")[-1].split(".")[-1])
    big = {m: names for m, names in layers.items() if len(names) >= 2}
    if len(big) < 2:
        return None
    groups = ", ".join(f"{m} ({', '.join(names[:4])})" for m, names in list(big.items())[:3])
    return LatentFinding(
        signal="layer-mix",
        function="",
        line=0,
        metric=sum(len(n) for n in big.values()),
        detail=f"file '{rel}' mixes layers: {groups} — the call graph is the seam; extract a module per layer",
        inner=[],
    )


def _dominant_callee(conn, repo: Path, qn: str, own_rel: str) -> str:
    """The most-called external subsystem of a function, or '' when none."""
    counts: Counter[str] = Counter()
    for (target,) in conn.execute(
        "SELECT DISTINCT target_qualified FROM edges WHERE source_qualified = ? AND kind = 'CALLS'", (qn,)
    ):
        mod = _resolve_callee_module(conn, repo, target)
        if mod and mod != _module_key(repo, own_rel):
            counts[mod] += 1
    return counts.most_common(1)[0][0] if counts else ""


def _docs_actions(repo: Path, file_churn: Counter[str], last_modified: dict[str, str]) -> list[Action]:
    """Documentation standards with a checkable form: links resolve, docs discoverable."""
    actions: list[Action] = []
    mds: list[Path] = []
    if (repo / "docs").exists():
        mds += sorted((repo / "docs").rglob("*.md"))
    for root in ("README.md", "AGENTS.md"):
        p = repo / root
        if p.exists():
            mds.append(p)
    for md in mds:
        rel = md.relative_to(repo).as_posix()
        text = md.read_text(encoding="utf-8", errors="replace")
        for target in _md_link_targets(text):
            if not (md.parent / target).exists():
                actions.append(
                    _latent_action(
                        repo,
                        rel,
                        LatentFinding(
                            signal="docs-link",
                            function="",
                            line=0,
                            metric=1,
                            detail=f"link to '{target}' from {rel} does not resolve — a doc that links "
                            f"nowhere is a finding",
                            inner=[],
                        ),
                        file_churn,
                        last_modified,
                    )
                )
        for path in _md_backtick_paths(text):
            if not path.startswith(("docs/", "standards/", "./", "../")):
                continue  # a bare name (coding-standards.md) is a reference, not a path
            if not (repo / path).exists():
                actions.append(
                    _latent_action(
                        repo,
                        rel,
                        LatentFinding(
                            signal="docs-link",
                            function="",
                            line=0,
                            metric=1,
                            detail=f"backtick path '{path}' from {rel} does not resolve — a doc that links "
                            f"nowhere is a finding",
                            inner=[],
                        ),
                        file_churn,
                        last_modified,
                    )
                )
    actions += _docs_reachability_actions(repo, file_churn, last_modified)
    return actions


def _docs_reachability_actions(repo: Path, file_churn: Counter[str], last_modified: dict[str, str]) -> list[Action]:
    """Every doc in docs/ is discoverable from AGENTS.md, directly or one link deep."""
    agents = repo / "AGENTS.md"
    if not agents.exists() or not (repo / "docs").exists():
        return []
    docs = sorted((repo / "docs").rglob("*.md"))
    doc_set = {d.relative_to(repo).as_posix() for d in docs}
    links: dict[str, set[str]] = {}
    for md in [agents] + docs:
        src = md.relative_to(repo).as_posix()
        links[src] = set()
        text = md.read_text(encoding="utf-8", errors="replace")
        for target in _md_link_targets(text) + _md_backtick_paths(text):
            try:
                cand = (md.parent / target).resolve().relative_to(repo).as_posix()
            except ValueError:  # code-health: ignore except an out-of-repo link is skipped, not a crash
                continue
            if cand in doc_set:
                links[src].add(cand)
    reachable = {agents.relative_to(repo).as_posix()}
    # Any number of hops is fine — AGENTS.md links groups, not flat lists.
    while True:
        frontier = {n for src in reachable for n in links.get(src, set())}
        if frontier <= reachable:
            break
        reachable |= frontier
    actions: list[Action] = []
    for d in docs:
        rel = d.relative_to(repo).as_posix()
        if rel not in reachable:
            actions.append(
                _latent_action(
                    repo,
                    "AGENTS.md",
                    LatentFinding(
                        signal="docs-undiscoverable",
                        function="",
                        line=0,
                        metric=1,
                        detail=f"doc '{rel}' is not reachable from AGENTS.md at any hop — a doc the reader "
                        f"cannot reach from where everyone starts does not exist. Link it from its "
                        f"group's index: AGENTS.md links group indexes and stays lean — never a flat "
                        f"list of every doc, and each doc keeps one distinct purpose and audience",
                        inner=[],
                    ),
                    file_churn,
                    last_modified,
                )
            )
    return actions


def _record_actions(
    repo: Path, include_tests: bool, file_churn: Counter[str], last_modified: dict[str, str],
    only_rel: str | None = None,
) -> list[Action]:
    """Record-shaped collections (bare dicts/tuples as records) via check_records."""
    actions: list[Action] = []
    scan_root: Path | list[Path] = (repo / only_rel) if only_rel else repo
    for finding in check_records.scan([scan_root]).findings:
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
        actions.append(
            Action(
                kind="record-shape",
                severity="fail",
                file=rel,
                line=line,
                function=fn,
                message=_record_shape_message(repo, body, rel, line),
                metric=1,
                churn=churn,
                last_modified=last_modified.get(rel, ""),
                tested="",
                raw=_raw_score("record-shape", 1, churn),
            )
        )
    return actions


def _record_shape_message(repo: Path, body: str, rel: str, line: int) -> str:
    """The record-shape guidance with the evidence for THIS finding named.

    The generic paragraph read as dogma because its carve-outs seemed to
    cover the very findings it fired on (an eval reviewer flagged
    make_default_thresholds as 'a lookup table the rule exempts'). Each
    shape kind gets its concrete reason: an Any blob has no fields; a
    return re-creates the shape per call; a fixed-key literal is a record
    being built; a parameter is a boundary without a type.
    """
    evidence = ""
    m = re.search(r"'([^']*)'", body)
    annotation = m.group(1) if m else body
    if "Any" in annotation or "object" in annotation:
        evidence += (
            " 'Any'/'object' values have no fields at all — every read is a string-keyed "
            "guess at a blob; the annotation says nothing about what the record holds."
        )
    if " in parameter " in body:
        evidence += (
            " A parameter is a boundary without a type — a dataclass or pydantic model is "
            "the framework's normal shape for request bodies and injected config."
        )
    if " as return type " in body:
        evidence += (
            " A return re-creates the shape at every call site; the constant-lookup-table "
            "carve-out is for module-scope literals, not returned shapes."
        )
    if body.startswith("dict literal"):
        keys = _literal_keys(repo, rel, line)
        evidence += f" The fixed string keys ({', '.join(keys)}) are fields, not data — this is a record being built."
    return f"{body} —{evidence} {GUIDANCE['record-shape']}"


def _literal_keys(repo: Path, rel: str, line: int) -> list[str]:
    """The constant string keys of the dict literal at line — the evidence
    that a 'dict literal with constant keys' really is a record."""
    try:
        tree = ast.parse((repo / rel).read_text(encoding="utf-8", errors="replace"))
    except (
        SyntaxError,
        UnicodeDecodeError,
    ):  # code-health: ignore except the file parsed in check_records; this re-parse is best-effort
        return []
    for node in ast.walk(tree):
        if isinstance(node, ast.Dict) and node.lineno == line:
            keys = [k.value for k in node.keys if isinstance(k, ast.Constant) and isinstance(k.value, str)]
            return keys[:4] + (["..."] if len(keys) > 4 else [])
    return []


def _collect_actions(
    repo: Path, args, file_churn, last_modified, covered, graph_preferred: bool, stale_note: str,
    only_rel: str | None = None,
) -> list[Action]:
    actions: list[Action] = []
    actions += complexity_actions(
        repo, args.max_complexity, args.include_tests, file_churn, last_modified, covered, graph_preferred, stale_note,
        only_rel,
    )
    if only_rel is None:
        actions += graph_actions(
            repo,
            args.max_function_lines,
            args.max_file_edges,
            args.max_risk,
            args.include_tests,
            file_churn,
            last_modified,
            covered,
            graph_preferred,
            stale_note,
        )
    if only_rel is None:
        actions += hotspot_actions(repo, args.hotspot_top_frac, args.hotspot_min_cc, file_churn, last_modified)
    actions += _record_actions(repo, args.include_tests, file_churn, last_modified, only_rel)
    actions += _latent_class_actions(repo, args.include_tests, file_churn, last_modified, only_rel)
    if only_rel is None:
        # repo-wide families — git/graph/coverage-scoped, skipped in the
        # single-file (--file / LSP) mode
        actions += _abstraction_actions(repo, args.include_tests, file_churn, last_modified)
        actions += _docs_actions(repo, file_churn, last_modified)
        actions += _folder_mix_actions(repo, file_churn, last_modified)
        actions += _layer_mix_actions(repo, file_churn, last_modified)
        actions += _cycle_actions(repo, file_churn, last_modified)
        if not RUST_SCAN.active(repo):
            # the Rust core computes duplicate + unused in its one repo-wide run
            actions += _duplicate_actions(repo, args.include_tests, file_churn, last_modified)
            actions += _unused_actions(repo, args.include_tests, file_churn, last_modified)
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
    if not (repo / ".git").exists():
        log(f"{repo} is not a git repository")
        return 2

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
    actions = _collect_actions(repo, args, fh.churn, fh.last_modified, cr.lines, cc.graph_preferred, cc.stale_note,
                               only_rel=args.file)
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


if __name__ == "__main__":
    sys.exit(main())

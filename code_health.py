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
change is.
"""

from __future__ import annotations

import argparse
import json
import os
import re
import sqlite3
import subprocess
import sys
from collections import Counter, defaultdict
from pathlib import Path

EXCLUDED_DIRS = {".git", ".venv", "node_modules", "__pycache__", "dist", "build", ".mypy_cache", ".pytest_cache", ".ruff_cache"}

ACTION_KINDS = ("complexity", "large-function", "hub-file", "hotspot", "high-risk")

# Fix guidance per action kind. One sentence each: what to do, not just what's
# wrong. Tied to the real requirements (readability, maintainability,
# anti-fragility) via separation of concerns, domain language, encapsulation.
# Deliberately resists gaming the metric: splitting a function to lower a
# count without clarifying it is not a fix.
GUIDANCE = {
    "complexity": "Extract each decision branch into a named method that says what it decides in domain terms — one decision per method, happy path reads top-to-bottom. If the body is repeated similar blocks rather than distinct decisions, prefer a data table + loop over more methods.",
    "large-function": "Split by responsibility into named steps that read like a procedure in the domain; one job per step, each independently testable.",
    "hub-file": "Separate the concerns it mixes (HTTP, orchestration, persistence, …) into modules with narrow, stable interfaces so changes stay contained. If the file is a composition root whose job is wiring, move handler logic out to the service layer and keep the assembly thin — don't split the assembly itself.",
    "hotspot": "Make the volatile part small and data-driven behind a stable interface — frequent changes become cheap and cannot disturb the stable core.",
    "high-risk": "Pin behavior with tests, then reduce the caller surface — when many things depend on it, the simplest code is the safest.",
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
                     source_prefix: str | None = None, own_module: str | None = None) -> tuple[list[tuple[str, int, list[str]]], bool]:
    """Group a function's (or file's) cross-module callees by subsystem.

    Returns (clusters, strong). clusters always lists what resolved (even a
    single subsystem — the caller decides how to word it); strong is True
    when >= 2 distinct subsystems with >= 3 total calls — the evidence bar
    for claiming a real seam mix. Never silently empty: callers show an
    explicit "unresolved" marker instead of boilerplate.
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
    clusters = [(m, counts[m], names.get(m, [])) for m, _ in counts.most_common(4)]
    strong = len(clusters) >= 2 and sum(c for _, c, _ in clusters) >= 3
    unresolved = [r["target_qualified"].split("::")[-1].split(".")[-1] for r in rows][:6]
    return clusters, strong, unresolved


def mix_text(clusters: list[tuple[str, int, list[str]]], strong: bool) -> str:
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


def fmt_clusters(clusters: list[tuple[str, int, list[str]]]) -> str:
    parts = []
    for m, c, names in clusters:
        n = f" ({', '.join(names)})" if names else ""
        parts.append(f"{m} ({c}{n})")
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


def file_history(repo: Path) -> tuple[Counter[str], dict[str, str]]:
    """One pass over git history: per-file change counts and last-modified date.

    Returns (churn, last_modified). Cheap even on long histories; drives the
    'is this still live?' judgement and the priority ranking.
    """
    churn: Counter[str] = Counter()
    last: dict[str, str] = {}
    proc = subprocess.run(
        ["git", "-C", str(repo), "log", "--name-only", "--pretty=format:%ad", "--date=short"],
        capture_output=True, text=True, timeout=120,
    )
    if proc.returncode != 0:
        return churn, last
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
    return churn, last


def load_coverage(repo: Path) -> tuple[dict[str, set[int]] | None, str]:
    """Per-file covered line sets, preferring the repo's own coverage data.

    Sources, in order: coverage.xml (Cobertura, what CI gates on), then
    .coverage (coverage.py SQLite, line_bits format). The graph's TESTED_BY
    edges miss tests that import inside the test body — real coverage data
    does not. Returns (None, reason) when neither exists.
    """
    xml_path = repo / "coverage.xml"
    if xml_path.exists():
        import xml.etree.ElementTree as ET
        try:
            root = ET.parse(xml_path).getroot()
        except ET.ParseError:
            return None, "coverage.xml unparseable"
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
        return (covered or None), "coverage.xml"

    sqlite_path = repo / ".coverage"
    if sqlite_path.exists():
        try:
            db = sqlite3.connect(sqlite_path)
            files = dict(db.execute("SELECT id, path FROM file"))
            covered = {}
            for fid, numbits in db.execute("SELECT file_id, numbits FROM line_bits"):
                path = files.get(fid)
                if not path:
                    continue
                rel = rel_path(repo, path)
                if not rel.endswith(".py"):
                    continue
                covered.setdefault(rel, set()).update(_numbits_to_lines(numbits))
            db.close()
            return (covered or None), ".coverage"
        except sqlite3.Error:
            return None, ".coverage unreadable"
    return None, "no coverage data (no coverage.xml, no .coverage)"


def _numbits_to_lines(numbits: bytes) -> set[int]:
    """Decode coverage.py line_bits: bit (n-1)%8 of byte (n-1)//8 => line n."""
    lines: set[int] = set()
    for byte_idx, byte in enumerate(numbits):
        if byte:
            for bit in range(8):
                if byte & (1 << bit):
                    lines.add(byte_idx * 8 + bit + 1)
    return lines


def covered_span(covered: dict[str, set[int]] | None, rel: str, start: int, end: int) -> bool | None:
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


def _node_info(conn: sqlite3.Connection | None, repo: Path, rel_file: str, name: str) -> dict | None:
    """Graph node (qualified_name, file_path, tested, params, return_type, span), or None."""
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
    return {
        "name": name,
        "qualified_name": row["qualified_name"],
        "file_path": row["file_path"],
        "tested": row["test_coverage"] or "",
        "params": row["params"] or "",
        "return_type": row["return_type"] or "",
        "line_start": row["line_start"],
        "line_end": row["line_end"],
    }


def _raw_score(kind: str, metric: float, churn: int, callers: int | None = None) -> float:
    """Continuous risk score: normalized metric x churn factor (x fan-in for high-risk).

    Normalized to a 1-99 percentile ranking in main, so the list spreads
    instead of saturating at 99.
    """
    norm = {
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
                       covered: dict[str, set[int]] | None, graph_preferred: bool, stale_note: str) -> list[dict]:
    """Cyclomatic complexity per function via radon's fast pure-Python analyzer."""
    try:
        from radon.visitors import ComplexityVisitor
    except ImportError:  # pragma: no cover
        log("radon not importable — run via `uv run --with radon python3 code_health.py`")
        sys.exit(2)

    conn = _graph_conn(repo)
    actions: list[dict] = []
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
            info = _node_info(conn, repo, rel, fn.name)
            if info:
                clusters, strong, _unres = concern_clusters(conn, repo, source_qn=info["qualified_name"], own_module=_module_key(repo, info["file_path"]))
                if not info.get("params"):
                    info["def_sig"] = _def_signature(source, fn.lineno)
            else:
                clusters, strong, _unres = [], False, []
            if clusters:
                seam = mix_text(clusters, strong)
                message = (
                    f"cyclomatic complexity {fn.complexity} (>= {max_cc}) — {seam} — "
                    f"extract a class per concern; the seams are those subsystem "
                    f"boundaries, not line breaks."
                )
            else:
                snippet = "calls: " + ", ".join(_unres) if _unres else "no cross-module callees resolved"
                message = (f"cyclomatic complexity {fn.complexity} (>= {max_cc}) — {GUIDANCE['complexity']}"
                           f" [concern mix unresolved — {snippet}]")
            if info:
                message += coverage_note(covered, repo, rel, info, info["tested"], graph_preferred, stale_note)
            churn = file_churn.get(rel, 0)
            actions.append(
                {
                    "kind": "complexity",
                    "severity": "fail",
                    "file": rel,
                    "line": fn.lineno,
                    "function": fn.name,
                    "message": message,
                    "metric": fn.complexity,
                    "churn": churn,
                    "last_modified": last_modified.get(rel, ""),
                    "tested": final_tested(covered, rel, info, graph_preferred) if info else "",
                    "raw": _raw_score("complexity", fn.complexity, churn),
                }
            )
    return actions


def final_tested(covered: dict[str, set[int]] | None, rel: str, info: dict, graph_preferred: bool = False) -> str:
    """Coverage verdict: real coverage data wins, unless it's stale.

    When the snapshot predates the repo's tests (graph_preferred), the graph's
    fresher TESTED_BY signal wins — the snapshot would otherwise mislabel
    freshly-tested code as untested.
    """
    if graph_preferred and info.get("tested") == "tested":
        return "tested"
    cov = covered_span(covered, rel, info.get("line_start") or 1, info.get("line_end") or info.get("line_start") or 1)
    if cov is not None:
        return "tested" if cov else "untested"
    return info.get("tested", "")


def coverage_note(covered: dict[str, set[int]] | None, repo: Path, rel: str, info: dict, graph_tested: str,
                  graph_preferred: bool = False, stale_note: str = "") -> str:
    """Append a coverage-based instruction when the function is untested."""
    contract = contract_text(info.get("name") or "", info.get("params", ""), info.get("return_type", ""), info.get("def_sig", ""))
    tfile = _test_file_for(repo, rel) if info else ""
    extend = f" Extend {tfile}." if tfile else ""
    if graph_preferred:
        # Stale snapshot: the strongest honest claim is "verify" — current tests
        # may exercise the function through paths the graph cannot see (HTTP,
        # in-body imports), so never assert "write the failing tests first".
        if graph_tested == "tested":
            return ""
        return (f" Coverage snapshot is older than the repo's tests and the graph sees no direct "
                f"unit tests — verify with make coverage / htmlcov; if truly uncovered, pin "
                f"{contract} with tests first.{extend}")
    cov = covered_span(covered, rel, info.get("line_start") or 1, info.get("line_end") or info.get("line_start") or 1)
    if cov is False:
        return f" Not covered by the repo's coverage data — write the failing tests first. Contract to pin: {contract}.{extend}"
    if cov is None and graph_tested == "untested":
        return f" No coverage data and graph sees no unit tests — verify (htmlcov/ if present); if uncovered, pin {contract} with tests first.{extend}"
    return ""


# --------------------------------------------------------------------------- graph (code-review-graph)
def _graph_db(repo: Path) -> Path | None:
    db = repo / ".code-review-graph" / "graph.db"
    return db if db.exists() else None


def graph_actions(repo: Path, max_fn_lines: int, max_file_edges: int, max_risk: float, include_tests: bool,
                  file_churn: Counter[str], last_modified: dict[str, str],
                  covered: dict[str, set[int]] | None, graph_preferred: bool, stale_note: str) -> list[dict]:
    db_path = _graph_db(repo)
    if db_path is None:
        log(f"no graph at {db_path} — run `code-review-graph build --repo {repo}` first")
        return []
    db = sqlite3.connect(db_path)
    db.row_factory = sqlite3.Row
    actions: list[dict] = []
    src_cache: dict[str, str] = {}

    # Large functions: node line span over threshold. Skip Test nodes and non-Python.
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
        clusters, strong, unres = concern_clusters(db, repo, source_qn=row["qualified_name"], own_module=_module_key(repo, row["file_path"]))
        if clusters:
            seam = mix_text(clusters, strong)
            message = (
                f"function spans {span} lines (>= {max_fn_lines}) — {seam} — "
                f"extract a class per concern, then split each class's methods into "
                f"named domain steps."
            )
        else:
            snippet = "calls: " + ", ".join(unres) if unres else "no cross-module callees resolved"
            message = (f"function spans {span} lines (>= {max_fn_lines}) — {GUIDANCE['large-function']}"
                       f" [concern mix unresolved — {snippet}]")
        info = {"name": row["name"], "params": row["params"] or "", "return_type": row["return_type"] or "",
                "line_start": row["line_start"], "line_end": row["line_end"]}
        if not info["params"]:
            if rel not in src_cache:
                try:
                    src_cache[rel] = (repo / rel).read_text(encoding="utf-8", errors="replace")
                except OSError:
                    src_cache[rel] = ""
            info["def_sig"] = _def_signature(src_cache[rel], row["line_start"])
        message += coverage_note(covered, repo, rel, info, row["test_coverage"] or "", graph_preferred, stale_note)
        churn = file_churn.get(rel, 0)
        actions.append(
            {
                "kind": "large-function",
                "severity": "fail",
                "file": rel,
                "line": row["line_start"],
                "function": row["name"],
                "message": message,
                "metric": span,
                "churn": churn,
                "last_modified": last_modified.get(rel, ""),
                "tested": final_tested(covered, rel, info),
                "raw": _raw_score("large-function", span, churn),
            }
        )

    # Hub files: real coupling edges per Python file (fan-in + fan-out).
    # TESTED_BY/CONTAINS are test-harness noise; test files dominate raw counts.
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
        clusters, strong, unres = concern_clusters(db, repo, source_prefix=abs_file + "::", own_module=_module_key(repo, row["file_path"]))
        if clusters:
            seam = mix_text(clusters, strong)
            message = (
                f"{row['edge_count']} call/import edges (>= {max_file_edges}) — {seam} — "
                f"split into one module per concern with narrow, stable interfaces so "
                f"changes stay contained."
            )
        else:
            snippet = "calls: " + ", ".join(unres) if unres else "no cross-module callees resolved"
            message = (f"{row['edge_count']} call/import edges (>= {max_file_edges}) — {GUIDANCE['hub-file']}"
                       f" [concern mix unresolved — {snippet}]")
        # Point at the file's fattest handlers: top-3 by cyclomatic complexity.
        first = db.execute(
            "SELECT MIN(line_start) FROM nodes WHERE file_path = ? AND kind IN ('Function', 'Method')",
            (row["file_path"],),
        ).fetchone()[0]
        fat = ""
        try:
            from radon.visitors import ComplexityVisitor
            if rel not in src_cache:
                try:
                    src_cache[rel] = (repo / rel).read_text(encoding="utf-8", errors="replace")
                except OSError:
                    src_cache[rel] = ""
            if src_cache[rel]:
                fns = ComplexityVisitor.from_code(src_cache[rel]).functions
                top = sorted(fns, key=lambda f: f.complexity, reverse=True)[:3]
                if top:
                    fat = " fattest: " + ", ".join(f"{f.name}:{f.lineno} (CC {f.complexity})" for f in top)
        except ImportError:
            pass
        message += fat
        churn = file_churn.get(rel, 0)
        actions.append(
            {
                "kind": "hub-file",
                "severity": "fail",
                "file": rel,
                "line": first or 1,
                "function": "",
                "message": message,
                "metric": row["edge_count"],
                "churn": churn,
                "last_modified": last_modified.get(rel, ""),
                "tested": "",
                "raw": _raw_score("hub-file", row["edge_count"], churn),
            }
        )

    # High-risk nodes from the graph's own risk index (caller count, coverage, security).
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
        rel = rel_path(repo, row["file_path"])
        if not include_tests and is_test_path(rel):
            continue
        callers = [r[0].split("::")[-1] for r in db.execute(
            "SELECT DISTINCT source_qualified FROM edges WHERE target_qualified = ? AND kind = 'CALLS'",
            (row["qualified_name"],),
        )][:6]
        if callers:
            resolved = f", callers: {', '.join(callers)}"
            if len(callers) < row["caller_count"]:
                resolved += f" ({len(callers)} distinct of {row['caller_count']} call sites — count includes repeated call sites)"
        else:
            resolved = ""
        message = (
            f"graph risk {row['risk_score']:.2f} (>= {max_risk}), {row['caller_count']} call sites{resolved} — "
            f"{GUIDANCE['high-risk']}"
        )
        info = {"name": row["name"], "params": row["params"] or "", "return_type": row["return_type"] or "",
                "line_start": row["line_start"] or 1, "line_end": row["line_end"] or row["line_start"] or 1}
        if not info["params"]:
            if rel not in src_cache:
                try:
                    src_cache[rel] = (repo / rel).read_text(encoding="utf-8", errors="replace")
                except OSError:
                    src_cache[rel] = ""
            info["def_sig"] = _def_signature(src_cache[rel], info["line_start"])
        message += coverage_note(covered, repo, rel, info, row["test_coverage"] or "", graph_preferred, stale_note)
        churn = file_churn.get(rel, 0)
        actions.append(
            {
                "kind": "high-risk",
                "severity": "fail",
                "file": rel,
                "line": row["line_start"] or 1,
                "function": row["name"],
                "message": message,
                "metric": round(row["risk_score"], 2),
                "churn": churn,
                "last_modified": last_modified.get(rel, ""),
                "tested": final_tested(covered, rel, info),
                "callers": callers,
                "raw": _raw_score("high-risk", row["risk_score"], churn, len(callers) if callers else row["caller_count"]),
            }
        )
    db.close()
    return actions


# --------------------------------------------------------------------------- hotspots (git history x complexity)
def hotspot_actions(repo: Path, top_frac: float, min_cc: float,
                    file_churn: Counter[str], last_modified: dict[str, str]) -> list[dict]:
    """CodeScene hotspot: files that change often AND are complex.

    Change frequency from the shared `git log --name-only` pass; complexity =
    max cyclomatic complexity of the file's functions (radon). Max, not mean:
    mean dilutes when a file mixes many small functions with one monster —
    CodeScene's hotspot signal is the concentration of complexity in a
    frequently-changed file.
    """
    try:
        from radon.complexity import ComplexityVisitor
    except ImportError:  # pragma: no cover
        log("radon not importable — run via `uv run --with radon python3 code_health.py`")
        sys.exit(2)

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
        volatile: list[tuple[int, int, str, int]] = []  # (churn, cc, name, line)
        if conn is not None:
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
                volatile.append((churn, fn.complexity, fn.name, node["line_start"]))
        volatile.sort(reverse=True)
        parts = []
        for churn, cc, name, line in volatile[:3]:
            parts.append(f"{name}:{line} (CC {cc}" + (f", {churn}x churn)" if churn else ")"))
        if not parts:
            parts = [f"max CC {max_cc} in {rel}"]
        churn = file_churn.get(rel, 0)
        actions.append(
            {
                "kind": "hotspot",
                "severity": "fail",
                "file": rel,
                "line": 1,
                "function": "",
                "message": f"changed {churn}x (top {top_frac:.0%} by churn) — volatile part: "
                f"{', '.join(parts)} — {GUIDANCE['hotspot']}",
                "metric": max_cc,
                "churn": churn,
                "last_modified": last_modified.get(rel, ""),
                "tested": "",
                "raw": _raw_score("hotspot", max_cc, churn),
            }
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


def action_key(a: dict) -> str:
    return f"{a['kind']}:{a['file']}:{a['line']}:{a['function']}"


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


def main() -> int:
    args = parse_args()
    repo = args.repo.resolve()
    if not (repo / ".git").exists():
        log(f"{repo} is not a git repository")
        return 2

    file_churn, last_modified = file_history(repo)
    if args.refresh_coverage:
        log("refreshing coverage (make coverage) — this can take minutes…")
        proc = subprocess.run(["make", "coverage"], cwd=repo, capture_output=True, text=True, timeout=1800)
        log("coverage refresh exit " + str(proc.returncode))
    covered, coverage_source = load_coverage(repo)
    import time
    if coverage_source == ".coverage" and (repo / ".coverage").exists():
        coverage_source += " (mtime " + time.strftime("%Y-%m-%d %H:%M", time.localtime((repo / ".coverage").stat().st_mtime)) + ")"
    # Stale coverage: if the snapshot predates the repo's test files, prefer the
    # graph's fresher TESTED_BY verdict and say so on every affected action.
    graph_preferred = False
    stale_note = ""
    if covered is not None and (repo / ".coverage").exists():
        cov_mtime = (repo / ".coverage").stat().st_mtime
        newest_test = max((p.stat().st_mtime for p in (repo / "tests").rglob("*.py")), default=0.0)
        if newest_test > cov_mtime:
            graph_preferred = True
            stale_note = " (coverage snapshot older than the repo's tests — graph verdict used; verify against htmlcov/ if present)"
    diff = changed_files(repo, args.base)

    actions: list[dict] = []
    actions += complexity_actions(repo, args.max_complexity, args.include_tests, file_churn, last_modified, covered, graph_preferred, stale_note)
    actions += graph_actions(repo, args.max_function_lines, args.max_file_edges, args.max_risk, args.include_tests, file_churn, last_modified, covered, graph_preferred, stale_note)
    actions += hotspot_actions(repo, args.hotspot_top_frac, args.hotspot_min_cc, file_churn, last_modified)

    # Dedupe: same kind+file+function+line (graph and radon can both flag a function).
    # Keep the higher raw score when the same item fired on two axes.
    seen: dict[tuple, dict] = {}
    for a in actions:
        key = (a["kind"], a["file"], a["line"], a["function"])
        if key not in seen or a["raw"] > seen[key]["raw"]:
            seen[key] = a
    unique = list(seen.values())

    # Normalize raw risk scores to a 1-99 percentile so the list spreads and
    # ordering is meaningful (top = biggest churn x complexity x fan-in).
    raws = sorted(a["raw"] for a in unique)
    lo, hi = raws[0], raws[-1]
    for a in unique:
        a["priority"] = 99 if hi <= lo else max(1, round(1 + 98 * (a["raw"] - lo) / (hi - lo)))
        a["in_diff"] = a["file"] in diff
    unique.sort(key=lambda a: (-a["priority"], a["file"], a["line"]))

    # Baseline: acknowledged actions are reported but never fail the gate,
    # so a repo can lock today's debt and go green incrementally.
    if args.update_baseline:
        if not args.baseline:
            log("--update-baseline requires --baseline PATH")
            return 2
        keys = [action_key(a) for a in unique]
        args.baseline.write_text(json.dumps({"actions": keys}, indent=2))
        print(f"code-health: baseline written — {len(keys)} action(s) locked to {args.baseline}")
        return 0

    baseline_keys: set[str] = set()
    if args.baseline and args.baseline.exists():
        try:
            baseline_keys = set(json.loads(args.baseline.read_text()).get("actions", []))
        except (json.JSONDecodeError, AttributeError):
            log(f"baseline {args.baseline} unreadable — ignoring")
    for a in unique:
        if action_key(a) in baseline_keys:
            a["severity"] = "ack"
    fails = [a for a in unique if a["severity"] != "ack"]
    acks = [a for a in unique if a["severity"] == "ack"]

    branch = subprocess.run(["git", "-C", str(repo), "branch", "--show-current"],
                            capture_output=True, text=True).stdout.strip()
    commit = subprocess.run(["git", "-C", str(repo), "rev-parse", "--short", "HEAD"],
                            capture_output=True, text=True).stdout.strip()

    if args.json:
        import datetime
        print(json.dumps({
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
            "actions": unique,
        }, indent=2))
    else:
        if not unique:
            print("code-health: clean — no actions")
        elif fails:
            top = fails[0]
            mine = sum(1 for a in fails if a.get("in_diff"))
            mine_txt = f"; {mine} of {len(fails)} in your diff" if diff else "; diff base unresolved"
            print(f"code-health: {len(fails)} action(s) to fix (+{len(acks)} acknowledged in baseline){mine_txt}, "
                  f"top P{top['priority']} {top['file']}:{top['line']} ({top['function'] or top['kind']})")
            if graph_preferred:
                print("WARNING: coverage snapshot predates the repo's tests — 'not covered' claims are graph-based; "
                      "regenerate coverage (make coverage) for ground truth")
            print("priority = percentile of raw risk (metric norm x (1 + churn/30) x (1 + callers/5)); "
                  "thresholds: CC>=" + str(args.max_complexity) + ", fn>=" + str(args.max_function_lines) +
                  " lines, file>=" + str(args.max_file_edges) + " edges, risk>=" + str(args.max_risk) +
                  ", hotspot top " + f"{args.hotspot_top_frac:.0%}" + " by churn with CC>=" + str(args.hotspot_min_cc) +
                  f"; coverage: {coverage_source}")
            # Group by file, files by their max priority.
            by_file: dict[str, list[dict]] = {}
            for a in fails:
                by_file.setdefault(a["file"], []).append(a)
            for file, items in sorted(by_file.items(), key=lambda kv: -max(i["priority"] for i in kv[1])):
                touched = " [in your diff]" if any(i["in_diff"] for i in items) else ""
                print(f"\n{file}{touched}")
                for a in items:
                    loc = f":{a['line']}" + (f" ({a['function']})" if a["function"] else "")
                    churn = f" [churn {a['churn']}x]" if a.get("churn") else ""
                    print(f"  [P{a['priority']:02d}][{a['kind']}] {loc}{churn} — {a['message']}")
            if acks:
                print(f"\nacknowledged in baseline ({len(acks)}): " + ", ".join(f"{a['file']}:{a['line']}" for a in acks[:5]) + (" …" if len(acks) > 5 else ""))
            print("\nre-run: uv run --with radon python3 code_health.py --repo " + str(repo) +
                  (" --baseline " + str(args.baseline) if args.baseline else "") +
                  "   | thresholds and per-action data in --json output")
            print("baseline: '--update-baseline --baseline code-health.json' acknowledges today's debt so the "
                  "gate only fails on NEW actions; this report is a snapshot, not wired into CI")
        else:
            print(f"code-health: {len(acks)} action(s), all acknowledged in baseline — clean gate")

    if fails and not args.warn:
        log(f"{len(fails)} action(s) found — failing (use --warn to run informational)")
        return 1
    return 0


if __name__ == "__main__":
    sys.exit(main())

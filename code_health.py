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
                     source_prefix: str | None = None, own_module: str | None = None) -> list[tuple[str, int]]:
    """Group a function's (or file's) cross-module callees by subsystem.

    source_qn: exact function qualified name; source_prefix: file path prefix
    (all CALLS from that file). Returns [(module, count), ...] when the source
    calls into >= 2 distinct subsystems (excluding its own module and
    unresolved/builtin targets) — those are the latent seams an extract-class
    refactor should follow. Empty otherwise.
    """
    if source_qn is not None:
        rows = conn.execute(
            "SELECT DISTINCT target_qualified FROM edges WHERE source_qualified = ? AND kind = 'CALLS'",
            (source_qn,),
        )
    else:
        rows = conn.execute(
            "SELECT DISTINCT target_qualified FROM edges WHERE source_qualified LIKE ? AND kind = 'CALLS'",
            (source_prefix + "%",),
        )
    counts: Counter[str] = Counter()
    for r in rows:
        mod = _resolve_callee_module(conn, repo, r["target_qualified"])
        if mod and mod != own_module:
            counts[mod] += 1
    top = counts.most_common(4)
    # Two subsystems with one call each is weak evidence of a real seam; the
    # resolved callees understate the picture anyway (builtins/externs skip),
    # so require a minimum total before claiming a mix.
    return top if len(top) >= 2 and sum(c for _, c in top) >= 3 else []


def _resolve_callee_module(conn: sqlite3.Connection, repo: Path, target: str) -> str | None:
    """Resolve a CALLS target to its defining module's cluster key (or None)."""
    row = conn.execute("SELECT file_path FROM nodes WHERE qualified_name = ?", (target,)).fetchone()
    if row is None and "::" in target:
        name = target.split("::")[-1].split(".")[-1]
        row = conn.execute("SELECT file_path FROM nodes WHERE name = ? LIMIT 1", (name,)).fetchone()
    if row is None:
        return None
    return _module_key(repo, row["file_path"])


def fmt_clusters(clusters: list[tuple[str, int]]) -> str:
    return ", ".join(f"{m} ({c})" for m, c in clusters)


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


def _node_info(conn: sqlite3.Connection | None, repo: Path, rel_file: str, name: str) -> dict | None:
    """Graph node (qualified_name, file_path, tested) for a function, or None."""
    if conn is None:
        return None
    abs_file = str(repo.resolve()) + "/" + rel_file
    row = conn.execute(
        "SELECT n.qualified_name, n.file_path, r.test_coverage "
        "FROM nodes n LEFT JOIN risk_index r ON r.node_id = n.id "
        "WHERE n.file_path = ? AND n.name = ? AND n.kind IN ('Function', 'Method') LIMIT 1",
        (abs_file, name),
    ).fetchone()
    if row is None:
        return None
    return {"qualified_name": row["qualified_name"], "file_path": row["file_path"], "tested": row["test_coverage"] or ""}


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
                       file_churn: Counter[str], last_modified: dict[str, str]) -> list[dict]:
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
            mix = concern_clusters(conn, repo, source_qn=info["qualified_name"], own_module=_module_key(repo, info["file_path"])) if info else []
            untested = "untested" if (info and info["tested"] == "untested") else ""
            message = (
                f"cyclomatic complexity {fn.complexity} (>= {max_cc}) — mixes concerns: "
                f"{fmt_clusters(mix)} — extract a class per concern; the seams are those "
                f"subsystem boundaries, not line breaks."
                if mix
                else f"cyclomatic complexity {fn.complexity} (>= {max_cc}) — {GUIDANCE['complexity']}"
            )
            if untested:
                message += " No unit-level tests detected by the graph — verify coverage; if genuinely uncovered, write the failing tests first."
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
                    "tested": info["tested"] if info else "",
                    "raw": _raw_score("complexity", fn.complexity, churn),
                }
            )
    return actions


# --------------------------------------------------------------------------- graph (code-review-graph)
def _graph_db(repo: Path) -> Path | None:
    db = repo / ".code-review-graph" / "graph.db"
    return db if db.exists() else None


def graph_actions(repo: Path, max_fn_lines: int, max_file_edges: int, max_risk: float, include_tests: bool,
                  file_churn: Counter[str], last_modified: dict[str, str]) -> list[dict]:
    db_path = _graph_db(repo)
    if db_path is None:
        log(f"no graph at {db_path} — run `code-review-graph build --repo {repo}` first")
        return []
    db = sqlite3.connect(db_path)
    db.row_factory = sqlite3.Row
    actions: list[dict] = []

    # Large functions: node line span over threshold. Skip Test nodes and non-Python.
    test_filter = "" if include_tests else "AND kind != 'Test'"
    for row in db.execute(
        f"""
        SELECT n.name, n.qualified_name, n.file_path, n.line_start, n.line_end, n.kind,
               r.test_coverage
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
        message = (
            f"function spans {span} lines (>= {max_fn_lines}) — mixes concerns: "
            f"{fmt_clusters(mix)} — extract a class per concern, then split each class's "
            f"methods into named domain steps."
            if mix
            else f"function spans {span} lines (>= {max_fn_lines}) — {GUIDANCE['large-function']}"
        )
        if row["test_coverage"] == "untested":
            message += " No unit-level tests detected by the graph — verify coverage; if genuinely uncovered, write the failing tests first."
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
                "tested": row["test_coverage"] or "",
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
        mix = concern_clusters(db, repo, source_prefix=abs_file + "::", own_module=_module_key(repo, row["file_path"]))
        message = (
            f"{row['edge_count']} call/import edges (>= {max_file_edges}) — mixes: "
            f"{fmt_clusters(mix)} — split into one module per concern with narrow, stable "
            f"interfaces so changes stay contained."
            if mix
            else f"{row['edge_count']} call/import edges (>= {max_file_edges}) — {GUIDANCE['hub-file']}"
        )
        # Point at the file's first function instead of line 1.
        first = db.execute(
            "SELECT MIN(line_start) FROM nodes WHERE file_path = ? AND kind IN ('Function', 'Method')",
            (row["file_path"],),
        ).fetchone()[0]
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
        SELECT n.file_path, n.line_start, n.name, n.qualified_name, r.risk_score, r.caller_count,
               r.test_coverage
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
        callers_text = f", callers: {', '.join(callers)}" if callers else ""
        message = (
            f"graph risk {row['risk_score']:.2f} (>= {max_risk}), {row['caller_count']} callers{callers_text} — "
            f"{GUIDANCE['high-risk']}"
        )
        if row["test_coverage"] == "untested":
            message += " No unit-level tests detected by the graph — verify; if genuinely uncovered, write the failing tests first."
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
                "tested": row["test_coverage"] or "",
                "callers": callers,
                "raw": _raw_score("high-risk", row["risk_score"], churn, row["caller_count"]),
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
    p.add_argument("--json", action="store_true", help="emit actions as JSON array on stdout")
    p.add_argument("--warn", action="store_true", help="exit 0 even when actions exist (informational run)")
    return p.parse_args()


def main() -> int:
    args = parse_args()
    repo = args.repo.resolve()
    if not (repo / ".git").exists():
        log(f"{repo} is not a git repository")
        return 2

    file_churn, last_modified = file_history(repo)

    actions: list[dict] = []
    actions += complexity_actions(repo, args.max_complexity, args.include_tests, file_churn, last_modified)
    actions += graph_actions(repo, args.max_function_lines, args.max_file_edges, args.max_risk, args.include_tests, file_churn, last_modified)
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
    unique.sort(key=lambda a: (-a["priority"], a["file"], a["line"]))

    if args.json:
        import datetime
        branch = subprocess.run(["git", "-C", str(repo), "branch", "--show-current"],
                                capture_output=True, text=True).stdout.strip()
        commit = subprocess.run(["git", "-C", str(repo), "rev-parse", "--short", "HEAD"],
                                capture_output=True, text=True).stdout.strip()
        print(json.dumps({
            "meta": {
                "repo": str(repo),
                "branch": branch,
                "commit": commit,
                "generated_at": datetime.date.today().isoformat(),
                "thresholds": {
                    "max_complexity": args.max_complexity,
                    "max_function_lines": args.max_function_lines,
                    "max_file_edges": args.max_file_edges,
                    "max_risk": args.max_risk,
                    "hotspot_top_frac": args.hotspot_top_frac,
                    "hotspot_min_cc": args.hotspot_min_cc,
                },
            },
            "actions": unique,
        }, indent=2))
    else:
        if not unique:
            print("code-health: clean — no actions")
        else:
            top = unique[0]
            print(f"code-health: {len(unique)} actions, highest priority P{top['priority']} "
                  f"{top['file']}:{top['line']} ({top['function'] or top['kind']}) — sorted by priority (churn x complexity x fan-in)")
        for a in unique:
            loc = f"{a['file']}:{a['line']}"
            fn = f" ({a['function']})" if a["function"] else ""
            print(f"[P{a['priority']:02d}][{a['kind']}] {loc}{fn} — {a['message']}")

    if unique and not args.warn:
        log(f"{len(unique)} action(s) found — failing (use --warn to run informational)")
        return 1
    return 0


if __name__ == "__main__":
    sys.exit(main())

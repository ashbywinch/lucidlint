"""Unit tests for code_health.py — every external system is faked.

No monkeypatch: fakes are injected by direct assignment and restored in
finally blocks.

Fakes:
- radon: `code_health.radon_visitor` is set to a fake whose `from_code`
  returns a configured function list (per-file via a queue).
- subprocess: `code_health.subprocess` is replaced with a fake module whose
  `run` routes argv patterns to canned outputs.
- graph + coverage: real SQLite engines, fake data, in tmp repos.
- the repo under analysis: a tmp directory with fake source files.
"""

import json
import os
import sqlite3
import sys
import time
import types
from pathlib import Path

import pytest

import code_health as ch

SCHEMA = """
CREATE TABLE nodes (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    kind TEXT NOT NULL, name TEXT NOT NULL,
    qualified_name TEXT NOT NULL UNIQUE, file_path TEXT NOT NULL,
    line_start INTEGER, line_end INTEGER, language TEXT,
    params TEXT, return_type TEXT, is_test INTEGER DEFAULT 0
);
CREATE TABLE edges (
    id INTEGER PRIMARY KEY AUTOINCREMENT, kind TEXT NOT NULL,
    source_qualified TEXT NOT NULL, target_qualified TEXT NOT NULL,
    file_path TEXT NOT NULL, line INTEGER DEFAULT 0
);
CREATE TABLE risk_index (
    node_id INTEGER PRIMARY KEY, qualified_name TEXT NOT NULL,
    risk_score REAL DEFAULT 0, caller_count INTEGER DEFAULT 0,
    test_coverage TEXT DEFAULT 'unknown'
);
"""

APP_SRC = """def alpha(a):
    if a:
        return 1
    return 0

def beta(b):
    return b
"""


# --------------------------------------------------------------------------- fakes
class FakeProc:
    def __init__(self, returncode=0, stdout="", stderr=""):
        self.returncode = returncode
        self.stdout = stdout
        self.stderr = stderr


class FakeSubprocess:
    """Fake subprocess module: routes argv to canned stdout by predicate."""

    TimeoutExpired = TimeoutError  # referenced by except clauses

    def __init__(self, routes):
        self.routes = routes  # list of (predicate(args) -> bool, stdout, returncode)
        self.calls = []

    def run(self, args, **kwargs):
        self.calls.append(args)
        for pred, stdout, returncode in self.routes:
            if pred(args):
                return FakeProc(returncode=returncode, stdout=stdout)
        raise AssertionError(f"unexpected argv: {args}")


class FakeFn:
    def __init__(self, name, lineno, complexity):
        self.name = name
        self.lineno = lineno
        self.complexity = complexity


class FakeRadonVisitor:
    """from_code pops the next configured function list per call."""

    per_call = []

    @classmethod
    def from_code(cls, source):
        if cls.per_call:
            return types.SimpleNamespace(functions=cls.per_call.pop(0))
        return types.SimpleNamespace(functions=[])


class Env:
    """Injects fakes into code_health and restores on exit (no monkeypatch)."""

    def __init__(self, routes=None, functions=None):
        self._saved = {}
        self.routes = routes or []
        self.functions = functions

    def __enter__(self):
        self._saved["subprocess"] = ch.subprocess
        self._saved["radon_visitor"] = ch.radon_visitor
        ch.subprocess = FakeSubprocess(self.routes)
        ch.radon_visitor = FakeRadonVisitor
        FakeRadonVisitor.per_call = list(self.functions or [])
        return self

    def __exit__(self, *exc):
        ch.subprocess = self._saved["subprocess"]
        ch.radon_visitor = self._saved["radon_visitor"]
        FakeRadonVisitor.per_call = []


# --------------------------------------------------------------------------- fixtures/helpers
def make_repo(tmp_path, app_src=APP_SRC):
    repo = tmp_path / "repo"
    (repo / ".git").mkdir(parents=True)
    (repo / "houses").mkdir()
    (repo / "tests" / "unit").mkdir(parents=True)
    (repo / "scripts").mkdir()
    (repo / "houses" / "app.py").write_text(app_src)
    (repo / "scripts" / "oneoff.py").write_text("def main():\n    pass\n")
    (repo / "tests" / "unit" / "test_app.py").write_text("def test_x():\n    pass\n")
    return repo


def make_graph(repo, nodes=(), edges=(), risks=()):
    gdir = repo / ".code-review-graph"
    gdir.mkdir(exist_ok=True)
    db = sqlite3.connect(gdir / "graph.db")
    db.executescript(SCHEMA)
    for n in nodes:
        db.execute(
            "INSERT INTO nodes (kind,name,qualified_name,file_path,line_start,line_end,params,return_type,is_test) "
            "VALUES (?,?,?,?,?,?,?,?,?)", n)
    for e in edges:
        db.execute(
            "INSERT INTO edges (kind,source_qualified,target_qualified,file_path,line) VALUES (?,?,?,?,?)", e)
    for r in risks:
        db.execute(
            "INSERT INTO risk_index (node_id,qualified_name,risk_score,caller_count,test_coverage) "
            "VALUES (?,?,?,?,?)", r)
    db.commit()
    db.close()


def numbits_for(lines):
    """coverage.py encoding, inverse of ch._numbits_to_lines."""
    if not lines:
        return b""
    b = bytearray((max(lines) + 7) // 8)
    for n in lines:
        b[(n - 1) // 8] |= 1 << ((n - 1) % 8)
    return bytes(b)


def make_dot_coverage(repo, covered: dict[str, list[int]]):
    db = sqlite3.connect(repo / ".coverage")
    db.executescript("CREATE TABLE file (id INTEGER PRIMARY KEY, path TEXT);"
                     "CREATE TABLE line_bits (file_id INTEGER, context_id INTEGER, numbits BLOB);")
    for i, (rel, lines) in enumerate(covered.items(), start=1):
        db.execute("INSERT INTO file (id, path) VALUES (?,?)", (i, str(repo / rel)))
        db.execute("INSERT INTO line_bits (file_id, context_id, numbits) VALUES (?,?,?)",
                   (i, 1, numbits_for(lines)))
    db.commit()
    db.close()


def make_coverage_xml(repo, covered: dict[str, list[int]]):
    classes = []
    for rel, nums in covered.items():
        ln = "".join(f'<line number="{n}" hits="1"/>' for n in nums)
        classes.append(f'<class name="x" filename="{repo / rel}"><lines>{ln}</lines></class>')
    (repo / "coverage.xml").write_text(
        f"<coverage><packages><package><classes>{''.join(classes)}</classes></package></packages></coverage>")


def git_routes(history="", diff="", branch="test-branch", commit="abc1234", log_l="abc1234 fix\n"):
    repo = None  # filled lazily; predicates match on command shape

    def is_git(args):
        return args[:2] == ["git", "-C"] and args[2] not in (None,)

    routes = []
    routes.append((lambda a: is_git(a) and a[3:5] == ["log", "--name-only"], history, 0))
    routes.append((lambda a: is_git(a) and "-L" in a[3:], log_l, 0))
    routes.append((lambda a: is_git(a) and a[3] == "diff", diff, 0))
    routes.append((lambda a: is_git(a) and a[3] == "branch", branch, 0))
    routes.append((lambda a: is_git(a) and a[3] == "rev-parse", commit, 0))
    routes.append((lambda a: a[:2] == ["make", "coverage"], "", 0))
    return routes


# --------------------------------------------------------------------------- helpers
def test_numbits_to_lines():
    assert ch._numbits_to_lines(b"\x02") == {2}
    assert ch._numbits_to_lines(b"\x01") == {1}
    assert ch._numbits_to_lines(b"") == set()
    assert ch._numbits_to_lines(bytes([0xFF, 0x01])) == {1, 2, 3, 4, 5, 6, 7, 8, 9}


def test_numbits_roundtrip():
    lines = {3, 42, 100}
    assert ch._numbits_to_lines(numbits_for(lines)) == lines


def test_rel_path():
    repo = Path("/r")
    assert ch.rel_path(repo, "/r/houses/a.py") == "houses/a.py"
    assert ch.rel_path(repo, "houses/a.py") == "houses/a.py"
    assert ch.rel_path(repo, "/other/x.py") == "/other/x.py"


def test_is_test_path():
    assert ch.is_test_path("tests/unit/x.py")
    assert ch.is_test_path("houses/__tests__/x.ts")
    assert ch.is_test_path("houses/test_foo.py")
    assert not ch.is_test_path("houses/app.py")
    assert not ch.is_test_path("scripts/main.py")


def test_contract_text():
    assert ch.contract_text("f", "(a: int, b: str)", "int") == "f(a: int, b: str) -> int"
    assert ch.contract_text("f", "(a,\n b)", "int") == "f(a, b) -> int"
    assert ch.contract_text("f", "", "int") == "f(…) -> int"
    assert ch.contract_text("f", "", "", "def f() -> int:") == "def f() -> int:"


def test_def_signature():
    src = "def f(a):\n    pass\n"
    assert ch._def_signature(src, 1) == "def f(a):"
    src2 = "def f(\n    a: int,\n    b: str,\n):\n    pass\n"
    assert ch._def_signature(src2, 1) == "def f( a: int, b: str, ):"
    assert ch._def_signature("x", 99) == ""
    assert ch._def_signature("", 1) == ""


def test_raw_score_caps():
    assert ch._raw_score("complexity", 40, 0) == pytest.approx(1.0)  # norm capped at 1.0
    assert ch._raw_score("complexity", 400, 0) == pytest.approx(1.0)  # capped, not 10.0
    assert ch._raw_score("complexity", 20, 0) == pytest.approx(0.5)
    assert ch._raw_score("complexity", 20, 90) == pytest.approx(0.5 * 2.5)  # churn capped at 1.5
    assert ch._raw_score("high-risk", 0.5, 0) == pytest.approx(0.5)
    assert ch._raw_score("high-risk", 0.5, 0, 10) == pytest.approx(0.5 * 2.0)  # callers capped at 1.0


def test_mix_text():
    strong = [("houses/a", 3, ["x"]), ("houses/b", 1, ["y"])]
    assert ch.mix_text(strong, True) == "mixes concerns: houses/a (3 (x)), houses/b (1 (y))"
    weak = [("houses/a", 1, ["x"])]
    assert ch.mix_text(weak, False) == "possible seams (weak signal): houses/a (1 (x))"
    assert ch.mix_text([], False) == ""


# --------------------------------------------------------------------------- coverage
def test_load_coverage_none(tmp_path):
    repo = make_repo(tmp_path)
    covered, source = ch.load_coverage(repo)
    assert covered is None
    assert "no coverage" in source


def test_load_coverage_xml(tmp_path):
    repo = make_repo(tmp_path)
    make_coverage_xml(repo, {"houses/app.py": [2, 3]})
    covered, source = ch.load_coverage(repo)
    assert source == "coverage.xml"
    assert covered["houses/app.py"] == {2, 3}


def test_load_coverage_dot(tmp_path):
    repo = make_repo(tmp_path)
    make_dot_coverage(repo, {"houses/app.py": [1, 2]})
    covered, source = ch.load_coverage(repo)
    assert source == ".coverage"
    assert covered["houses/app.py"] == {1, 2}


def test_load_coverage_prefers_xml(tmp_path):
    repo = make_repo(tmp_path)
    make_coverage_xml(repo, {"houses/app.py": [2]})
    make_dot_coverage(repo, {"houses/app.py": [1]})
    covered, source = ch.load_coverage(repo)
    assert source == "coverage.xml"


def test_covered_span_semantics(tmp_path):
    repo = make_repo(tmp_path)
    make_dot_coverage(repo, {"houses/app.py": [2]})
    covered, _ = ch.load_coverage(repo)
    assert ch.covered_span(covered, "houses/app.py", 2, 2) is True
    assert ch.covered_span(covered, "houses/app.py", 5, 5) is False  # present file, no hits
    assert ch.covered_span(covered, "scripts/oneoff.py", 1, 1) is None  # absent file = unknown
    assert ch.covered_span(None, "houses/app.py", 1, 1) is None


def test_verdict_precedence(tmp_path):
    repo = make_repo(tmp_path)
    make_dot_coverage(repo, {"houses/app.py": [2]})
    covered, _ = ch.load_coverage(repo)
    info = {"line_start": 1, "line_end": 4}

    # fresh coverage wins over graph
    assert ch._verdict(covered, "houses/app.py", info, "untested", False) == "tested"
    assert ch._verdict(covered, "houses/app.py", info, "tested", False) == "tested"
    # stale: graph tested wins
    assert ch._verdict(covered, "houses/app.py", info, "tested", True) == "tested"
    # stale: graph untested but snapshot hits -> tested (hits are evidence)
    assert ch._verdict(covered, "houses/app.py", info, "untested", True) == "tested"
    # stale: graph untested, no hits in span -> unknown (never hard untested)
    assert ch._verdict(covered, "scripts/oneoff.py", info, "untested", True) == "unknown"
    # no coverage data: graph decides
    assert ch._verdict(None, "houses/app.py", info, "untested", False) == "untested"


# --------------------------------------------------------------------------- graph analysis
def test_concern_clusters(tmp_path):
    repo = make_repo(tmp_path)
    abs_app = str(repo / "houses" / "app.py")
    abs_web = str(repo / "houses" / "web" / "other.py")
    abs_nodes = str(repo / "houses" / "nodes" / "b.py")
    qn = f"{abs_app}::alpha"
    make_graph(repo, nodes=[
        ("Function", "alpha", qn, abs_app, 1, 4, None, None, 0),
        ("Function", "helper", f"{abs_app}::helper", abs_app, 10, 12, None, None, 0),
        ("Function", "web_other", f"{abs_web}::web_other", abs_web, 1, 2, None, None, 0),
        ("Function", "web_other2", f"{abs_web}::web_other2", abs_web, 4, 5, None, None, 0),
        ("Function", "nodes_b", f"{abs_nodes}::nodes_b", abs_nodes, 1, 2, None, None, 0),
    ], edges=[
        ("CALLS", qn, f"{abs_app}::helper", abs_app, 2),  # own module: excluded
        ("CALLS", qn, f"{abs_web}::web_other", abs_app, 3),
        ("CALLS", qn, f"{abs_web}::web_other2", abs_app, 4),
        ("CALLS", qn, f"{abs_nodes}::nodes_b", abs_app, 5),
        ("CALLS", qn, "get", abs_app, 6),  # builtin, unresolvable
    ])
    db = sqlite3.connect(repo / ".code-review-graph" / "graph.db")
    db.row_factory = sqlite3.Row
    clusters, strong, unresolved = ch.concern_clusters(db, repo, source_qn=qn, own_module="houses")
    db.close()
    assert strong is True  # 2 distinct subsystems, 3 distinct callees
    assert ("houses/web", 2, ["web_other", "web_other2"]) in clusters  # own module excluded, callees named
    assert ("houses/nodes", 1, ["nodes_b"]) in clusters
    assert "get" in unresolved


def test_concern_clusters_weak_returned(tmp_path):
    repo = make_repo(tmp_path)
    abs_app = str(repo / "houses" / "app.py")
    abs_web = str(repo / "houses" / "web" / "other.py")
    qn = f"{abs_app}::alpha"
    make_graph(repo, nodes=[
        ("Function", "alpha", qn, abs_app, 1, 4, None, None, 0),
        ("Function", "web_other", f"{abs_web}::web_other", abs_web, 1, 2, None, None, 0),
    ], edges=[("CALLS", qn, f"{abs_web}::web_other", abs_app, 2)])
    db = sqlite3.connect(repo / ".code-review-graph" / "graph.db")
    db.row_factory = sqlite3.Row
    clusters, strong, _ = ch.concern_clusters(db, repo, source_qn=qn, own_module="houses")
    db.close()
    assert strong is False  # single subsystem = weak, but still returned
    assert len(clusters) == 1


# --------------------------------------------------------------------------- builders
def test_complexity_actions(tmp_path):
    repo = make_repo(tmp_path)
    fake_fns = [
        [FakeFn("alpha", 1, 20), FakeFn("beta", 6, 3)],
        [FakeFn("main", 1, 40)],  # scripts/oneoff.py
    ]
    with Env(routes=git_routes(), functions=fake_fns):
        actions = ch.complexity_actions(repo, 15, False, {}, {}, None, False, "")
    names = {(a["function"], a["file"]) for a in actions}
    assert ("alpha", "houses/app.py") in names
    assert ("beta", "houses/app.py") not in names  # below threshold
    assert all(a["kind"] == "complexity" for a in actions)
    assert all(a["severity"] == "fail" for a in actions)


def test_complexity_actions_skips_tests(tmp_path):
    repo = make_repo(tmp_path)
    # Queue order matches rglob: houses/app.py, scripts/oneoff.py, tests/unit/test_app.py.
    fns = [[FakeFn("alpha", 1, 3)], [FakeFn("main", 1, 3)], [FakeFn("test_x", 1, 30)]]
    with Env(routes=git_routes(), functions=[list(x) for x in fns]) as env:
        actions = ch.complexity_actions(repo, 15, False, {}, {}, None, False, "")
        leftover = len(FakeRadonVisitor.per_call)
    assert actions == []
    assert leftover == 1  # the test file's fns were never consumed
    with Env(routes=git_routes(), functions=[list(x) for x in fns]):
        actions = ch.complexity_actions(repo, 15, True, {}, {}, None, False, "")
    assert [a["function"] for a in actions] == ["test_x"]  # include_tests scans it


def test_graph_actions_large_function(tmp_path):
    repo = make_repo(tmp_path)
    abs_app = str(repo / "houses" / "app.py")
    make_graph(repo, nodes=[
        ("Function", "big", f"{abs_app}::big", abs_app, 10, 300, None, None, 0),
        ("Function", "small", f"{abs_app}::small", abs_app, 400, 405, None, None, 0),
        ("Test", "test_big", f"{abs_app}::test_big", abs_app, 500, 700, None, None, 1),
    ], risks=[(1, f"{abs_app}::big", 0.3, 0, "untested")])
    with Env(routes=git_routes()):
        actions = ch.graph_actions(repo, 120, 150, 0.8, False, {}, {}, None, False, "")
    big = [a for a in actions if a["kind"] == "large-function"]
    assert len(big) == 1  # Test node excluded
    assert big[0]["function"] == "big"
    assert big[0]["metric"] == 291
    assert "untested" in big[0]["tested"]


def test_graph_actions_hub_file(tmp_path):
    repo = make_repo(tmp_path)
    abs_app = str(repo / "houses" / "app.py")
    abs_other = str(repo / "houses" / "other.py")
    make_graph(repo, nodes=[
        ("Function", "a", f"{abs_app}::a", abs_app, 1, 3, None, None, 0),
        ("Function", "b", f"{abs_app}::b", abs_app, 5, 7, None, None, 0),
        ("Function", "o", f"{abs_other}::o", abs_other, 1, 2, None, None, 0),
    ], edges=[
        # real coupling: 2 CALLS from app to other
        ("CALLS", f"{abs_app}::a", f"{abs_other}::o", abs_app, 2),
        ("CALLS", f"{abs_app}::b", f"{abs_other}::o", abs_app, 6),
        # TESTED_BY noise must not count toward hub edges
        ("TESTED_BY", "t", f"{abs_app}::a", abs_app, 0),
        ("CONTAINS", abs_app, f"{abs_app}::a", abs_app, 0),
    ])
    with Env(routes=git_routes(), functions=[[FakeFn("a", 1, 30)]]):
        actions = ch.graph_actions(repo, 120, 2, 0.8, False, {}, {}, None, False, "")
    hub = [a for a in actions if a["kind"] == "hub-file"]
    assert len(hub) == 1
    assert hub[0]["file"] == "houses/app.py"
    assert hub[0]["metric"] == 2  # TESTED_BY/CONTAINS excluded
    assert "fattest: a:1 (CC 30)" in hub[0]["message"]
    assert hub[0]["line"] == 1  # anchored at the fattest function


def test_graph_actions_high_risk(tmp_path):
    repo = make_repo(tmp_path)
    abs_app = str(repo / "houses" / "app.py")
    qn = f"{abs_app}::alpha"
    make_graph(repo, nodes=[
        ("Function", "alpha", qn, abs_app, 1, 4, "(x: int)", "int", 0),
        ("Function", "caller", f"{abs_app}::caller", abs_app, 20, 22, None, None, 0),
    ], edges=[
        ("CALLS", f"{abs_app}::caller", qn, abs_app, 21),
        ("CALLS", f"{abs_app}::caller", "alpha", abs_app, 22),  # bare-name target also resolves
    ], risks=[(1, qn, 0.9, 2, "tested")])
    with Env(routes=git_routes()):
        actions = ch.graph_actions(repo, 120, 150, 0.8, False, {}, {}, None, False, "")
    hr = [a for a in actions if a["kind"] == "high-risk"]
    assert len(hr) == 1
    assert "callers: caller" in hr[0]["message"]
    assert "7 call site(s)" not in hr[0]["message"]  # 1 distinct caller
    assert hr[0]["tested"] == "tested"  # graph says tested, no coverage data


def test_hotspot_actions(tmp_path):
    repo = make_repo(tmp_path)
    abs_app = str(repo / "houses" / "app.py")
    make_graph(repo, nodes=[
        ("Function", "alpha", f"{abs_app}::alpha", abs_app, 1, 4, None, None, 0),
    ])
    # app.py changed 5x (top of history); scripts/oneoff.py 1x
    history = ("2026-08-01\nhouses/app.py\n2026-08-02\nhouses/app.py\n"
               "2026-08-03\nhouses/app.py\n2026-08-04\nhouses/app.py\n"
               "2026-08-05\nhouses/app.py\n2026-08-01\nscripts/oneoff.py\n")
    with Env(routes=git_routes(history=history, log_l="abc1234 fix\n"), functions=[[FakeFn("alpha", 1, 20)]]):
        churn, last = ch.file_history(repo)
        actions = ch.hotspot_actions(repo, 0.5, 15, churn, last)
    assert len(actions) == 1
    assert actions[0]["file"] == "houses/app.py"
    assert "alpha:1" in actions[0]["message"]  # volatile part named
    assert actions[0]["churn"] == 5


def test_file_history(tmp_path):
    repo = make_repo(tmp_path)
    history = ("2026-08-01\nhouses/app.py\n2026-08-02\nhouses/app.py\n"
               "2026-08-03\nnotpy.txt\n2026-08-04\n.venv/x.py\n")
    with Env(routes=git_routes(history=history)):
        churn, last = ch.file_history(repo)
    assert churn["houses/app.py"] == 2
    assert "notpy.txt" not in churn  # non-py ignored
    assert ".venv/x.py" not in churn
    assert last["houses/app.py"] == "2026-08-02"


# --------------------------------------------------------------------------- main
def run_main(repo, *extra, routes=None, functions=None):
    saved_argv = sys.argv
    sys.argv = ["code_health.py", "--repo", str(repo), *extra]
    try:
        with Env(routes=routes or git_routes(), functions=functions):
            return ch.main()
    finally:
        sys.argv = saved_argv


def test_main_exit_codes(tmp_path, capsys):
    repo = make_repo(tmp_path)
    assert run_main(repo, functions=[[FakeFn("alpha", 1, 20)]]) == 1
    assert "GATE: FAIL" in capsys.readouterr().out

    assert run_main(repo, functions=[[FakeFn("alpha", 1, 3)]]) == 0
    assert "GATE: PASS" in capsys.readouterr().out

    assert run_main(repo, functions=[[FakeFn("alpha", 1, 20)]], routes=git_routes()) == 1


def test_main_warn(tmp_path, capsys):
    repo = make_repo(tmp_path)
    assert run_main(repo, "--warn", functions=[[FakeFn("alpha", 1, 20)]]) == 0
    assert "GATE: INFORMATIONAL" in capsys.readouterr().out


def test_main_not_git_repo(tmp_path):
    assert run_main(tmp_path) == 2


def test_main_merges_function_targets(tmp_path, capsys):
    """complexity + large-function on the same function merge into one action."""
    repo = make_repo(tmp_path)
    abs_app = str(repo / "houses" / "app.py")
    make_graph(repo, nodes=[
        ("Function", "alpha", f"{abs_app}::alpha", abs_app, 1, 400, None, None, 0),
    ], risks=[(1, f"{abs_app}::alpha", 0.3, 0, "untested")])
    rc = run_main(repo, functions=[
        [FakeFn("alpha", 1, 44)],  # complexity for app.py
        [FakeFn("main", 1, 3)],    # oneoff.py below threshold
    ])
    assert rc == 1
    out = capsys.readouterr().out
    assert "across 1 distinct targets" in out  # alpha's complexity+large-function merged into one
    assert "[complexity,large-function]" in out


def test_main_baseline_ack(tmp_path, capsys):
    repo = make_repo(tmp_path)
    baseline = tmp_path / "baseline.json"
    # write a baseline covering the alpha complexity action
    abs_app = str(repo / "houses" / "app.py")
    baseline.write_text('{"actions": ["complexity:houses/app.py:1:alpha"]}')
    rc = run_main(repo, "--baseline", str(baseline), functions=[[FakeFn("alpha", 1, 20)]])
    assert rc == 0
    assert "GATE: PASS" in capsys.readouterr().out


def test_main_update_baseline(tmp_path, capsys):
    repo = make_repo(tmp_path)
    baseline = tmp_path / "baseline.json"
    rc = run_main(repo, "--update-baseline", "--baseline", str(baseline),
                  functions=[[FakeFn("alpha", 1, 20)]])
    assert rc == 0
    data = json.loads(baseline.read_text())
    assert "actions" in data and len(data["actions"]) >= 1


def test_main_json_meta(tmp_path, capsys):
    repo = make_repo(tmp_path)
    run_main(repo, "--json", functions=[[FakeFn("alpha", 1, 20)]])
    data = json.loads(capsys.readouterr().out)
    assert data["meta"]["repo"] == str(repo.resolve())
    assert data["meta"]["branch"] == "test-branch"
    assert data["meta"]["commit"] == "abc1234"
    assert data["meta"]["thresholds"]["max_complexity"] == 15
    assert data["meta"]["coverage_source"]  # non-empty
    assert all("priority" in a and "raw" in a and "churn" in a for a in data["actions"])


def test_main_lifecycle_note(tmp_path, capsys):
    repo = make_repo(tmp_path)
    history = "2026-08-01\nscripts/oneoff.py\n2026-08-02\nhouses/app.py\n"
    rc = run_main(repo, routes=git_routes(history=history),
                  functions=[[FakeFn("alpha", 1, 20)], [FakeFn("main", 1, 40)]])
    assert rc == 1
    out = capsys.readouterr().out
    assert "Lifecycle:" in out
    assert "scripts/oneoff.py" in out


def test_main_priority_percentile(tmp_path, capsys):
    repo = make_repo(tmp_path)
    # two flagged functions, one far bigger -> spread priorities
    rc = run_main(repo, functions=[
        [FakeFn("alpha", 1, 20), FakeFn("beta", 10, 200)],
        [FakeFn("main", 1, 3)],
    ])
    assert rc == 1
    out = capsys.readouterr().out
    assert "P99" in out or "P98" in out or "P97" in out

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
        self._saved["radon"] = ch.RADON.visitor
        ch.subprocess = FakeSubprocess(self.routes)
        ch.RADON.visitor = FakeRadonVisitor
        FakeRadonVisitor.per_call = list(self.functions or [])
        return self

    def __exit__(self, *exc):
        ch.subprocess = self._saved["subprocess"]
        ch.RADON.visitor = self._saved["radon"]
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
    strong = [ch.Cluster("houses/a", 3, ["x"]), ch.Cluster("houses/b", 1, ["y"])]
    assert ch.mix_text(strong, True) == "mixes concerns: houses/a (3 (x)), houses/b (1 (y))"
    weak = [ch.Cluster("houses/a", 1, ["x"])]
    assert ch.mix_text(weak, False) == "possible seams (weak signal): houses/a (1 (x))"
    assert ch.mix_text([], False) == ""


# --------------------------------------------------------------------------- coverage
def test_load_coverage_none(tmp_path):
    repo = make_repo(tmp_path)
    cr = ch.load_coverage(repo)
    assert cr.lines is None
    assert "no coverage" in cr.source


def test_load_coverage_xml(tmp_path):
    repo = make_repo(tmp_path)
    make_coverage_xml(repo, {"houses/app.py": [2, 3]})
    cr = ch.load_coverage(repo)
    assert cr.source == "coverage.xml"
    assert cr.lines["houses/app.py"] == {2, 3}


def test_load_coverage_dot(tmp_path):
    repo = make_repo(tmp_path)
    make_dot_coverage(repo, {"houses/app.py": [1, 2]})
    cr = ch.load_coverage(repo)
    assert cr.source == ".coverage"
    assert cr.lines["houses/app.py"] == {1, 2}


def test_load_coverage_prefers_xml(tmp_path):
    repo = make_repo(tmp_path)
    make_coverage_xml(repo, {"houses/app.py": [2]})
    make_dot_coverage(repo, {"houses/app.py": [1]})
    cr = ch.load_coverage(repo)
    assert cr.source == "coverage.xml"


def test_covered_span_semantics(tmp_path):
    repo = make_repo(tmp_path)
    make_dot_coverage(repo, {"houses/app.py": [2]})
    covered = ch.load_coverage(repo).lines
    assert ch.covered_span(covered, "houses/app.py", 2, 2) is True
    assert ch.covered_span(covered, "houses/app.py", 5, 5) is False  # present file, no hits
    assert ch.covered_span(covered, "scripts/oneoff.py", 1, 1) is None  # absent file = unknown
    assert ch.covered_span(None, "houses/app.py", 1, 1) is None


def test_verdict_precedence(tmp_path):
    repo = make_repo(tmp_path)
    make_dot_coverage(repo, {"houses/app.py": [2]})
    covered = ch.load_coverage(repo).lines
    info = ch.NodeInfo("f", "", "", "untested", "", "", 1, 4)

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
    res = ch.concern_clusters(db, repo, source_qn=qn, own_module="houses")
    db.close()
    assert res.strong is True  # 2 distinct subsystems, 3 distinct callees
    assert any(c.name == "houses/web" and c.count == 2 and c.callees == ["web_other", "web_other2"] for c in res.clusters)
    assert any(c.name == "houses/nodes" and c.count == 1 and c.callees == ["nodes_b"] for c in res.clusters)
    assert "get" in res.unresolved


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
    res = ch.concern_clusters(db, repo, source_qn=qn, own_module="houses")
    db.close()
    assert res.strong is False  # single subsystem = weak, but still returned
    assert len(res.clusters) == 1


# --------------------------------------------------------------------------- builders
def test_complexity_actions(tmp_path):
    repo = make_repo(tmp_path)
    fake_fns = [
        [FakeFn("alpha", 1, 20), FakeFn("beta", 6, 3)],
        [FakeFn("main", 1, 40)],  # scripts/oneoff.py
    ]
    with Env(routes=git_routes(), functions=fake_fns):
        actions = ch.complexity_actions(repo, 15, False, {}, {}, None, False, "")
    names = {(a.function, a.file) for a in actions}
    assert ("alpha", "houses/app.py") in names
    assert ("beta", "houses/app.py") not in names  # below threshold
    assert all(a.kind == "complexity" for a in actions)
    assert all(a.severity == "fail" for a in actions)


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
    assert [a.function for a in actions] == ["test_x"]  # include_tests scans it


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
    big = [a for a in actions if a.kind == "large-function"]
    assert len(big) == 1  # Test node excluded
    assert big[0].function == "big"
    assert big[0].metric == 291
    assert "untested" in big[0].tested


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
    hub = [a for a in actions if a.kind == "hub-file"]
    assert len(hub) == 1
    assert hub[0].file == "houses/app.py"
    assert hub[0].metric == 2  # TESTED_BY/CONTAINS excluded
    assert "fattest: a:1 (CC 30)" in hub[0].message
    assert hub[0].line == 1  # anchored at the fattest function


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
    hr = [a for a in actions if a.kind == "high-risk"]
    assert len(hr) == 1
    assert "callers: caller" in hr[0].message
    assert "7 call site(s)" not in hr[0].message  # 1 distinct caller
    assert hr[0].tested == "tested"  # graph says tested, no coverage data


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
        fh = ch.file_history(repo)
        actions = ch.hotspot_actions(repo, 0.5, 15, fh.churn, fh.last_modified)
    assert len(actions) == 1
    assert actions[0].file == "houses/app.py"
    assert "alpha:1" in actions[0].message  # volatile part named
    assert actions[0].churn == 5


def test_file_history(tmp_path):
    repo = make_repo(tmp_path)
    history = ("2026-08-01\nhouses/app.py\n2026-08-02\nhouses/app.py\n"
               "2026-08-03\nnotpy.txt\n2026-08-04\n.venv/x.py\n")
    with Env(routes=git_routes(history=history)):
        fh = ch.file_history(repo)
    assert fh.churn["houses/app.py"] == 2
    assert "notpy.txt" not in fh.churn  # non-py ignored
    assert ".venv/x.py" not in fh.churn
    assert fh.last_modified["houses/app.py"] == "2026-08-02"


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


# --------------------------------------------------------------------------- record-shape integration
def test_record_actions(tmp_path):
    repo = make_repo(tmp_path)
    (repo / "houses" / "app.py").write_text(
        "def load(x: dict[str, Any]) -> dict[str, Any]:\n"
        "    return {\"a\": 1, \"b\": x}\n"
    )
    with Env(routes=git_routes()):
        actions = ch._record_actions(repo, False, {}, {})
    kinds = [a for a in actions if a.kind == "record-shape"]
    # grab-bag param (line 1), grab-bag return (line 1), return-position dict literal (line 2)
    assert len(kinds) == 3
    assert all(a.file == "houses/app.py" for a in kinds)
    assert "record-shape" in ch.ACTION_KINDS


def test_record_actions_skips_tests(tmp_path):
    repo = make_repo(tmp_path)
    (repo / "tests" / "unit" / "test_app.py").write_text(
        "def helper() -> dict[str, Any]:\n    return {}\n")
    with Env(routes=git_routes()):
        actions = ch._record_actions(repo, False, {}, {})
    assert actions == []
    with Env(routes=git_routes()):
        actions = ch._record_actions(repo, True, {}, {})
    assert len(actions) >= 1


def test_merge_keeps_file_level_kinds(tmp_path, capsys):
    """Hotspot + hub-file on the same file are different problems — both survive the merge."""
    repo = make_repo(tmp_path)
    abs_app = str(repo / "houses" / "app.py")
    make_graph(repo, nodes=[("Function", "a", f"{abs_app}::a", abs_app, 1, 3, None, None, 0)],
               edges=[("CALLS", f"{abs_app}::a", "x", abs_app, 2)],
               risks=[(1, f"{abs_app}::a", 0.9, 1, "tested")])
    # one file-level hotspot + one hub-file + one high-risk on the same file
    history = "2026-08-01\nhouses/app.py\n2026-08-02\nhouses/app.py\n"
    with Env(routes=git_routes(history=history), functions=[[FakeFn("a", 1, 20)], [FakeFn("a", 1, 20)]]):
        fh = ch.file_history(repo)
        actions = ch.graph_actions(repo, 120, 1, 0.8, False, fh.churn, fh.last_modified, None, False, "")
        actions += ch.hotspot_actions(repo, 1.0, 15, fh.churn, fh.last_modified)
        merged = ch._merge_targets(ch._dedupe(actions))
    kinds = {a.kind for a in merged}
    assert kinds == {"hub-file", "high-risk", "hotspot"}  # all three survive


def test_tool_passes_its_own_record_check():
    """The record-shape kind must never fire on the tool's own code."""
    repo = Path(__file__).resolve().parent.parent
    actions = ch._record_actions(repo, False, {}, {})
    self_findings = [a for a in actions if a.file in ("code_health.py", "check_records.py")]
    assert self_findings == []


def test_record_shape_guidance_teaches_naming_not_just_classes():
    """The fix guidance must teach the communication theme: records want
    classes; genuine maps are named by their MEANING (never SomethingDict —
    that renames the smell); boundary-crossing data is ingested into a domain
    class at the boundary. (Regression: it used to say 'maps stay dicts'.)"""
    g = ch.GUIDANCE["record-shape"]
    assert "class/dataclass" in g
    assert "stay dicts" not in g
    assert "SomethingDict" in g and "renames the smell" in g  # anti-*Dict lesson
    assert "boundary" in g and "ingest" in g  # boundary-ingestion lesson
    # the philosophy docstring carries the same principles
    assert "cheapest form of encapsulation" in ch.__doc__
    assert "JsonDict is the smell renamed" in ch.__doc__
    # the tool models the lesson: no JsonDict alias, no to_dict claiming a
    # type for the wire format — serialization happens at the render boundary
    assert not hasattr(ch, "JsonDict")
    assert not hasattr(ch.Action, "to_dict")


# --------------------------------------------------------------------------- latent-class detector
def test_latent_class_closure_signal(tmp_path):
    repo = make_repo(tmp_path)
    src = ("def big():\n"
           "    def inner_a(x):\n"
           "        return x + 1\n"
           "    def inner_b(x):\n"
           "        return x * 2\n"
           "    return inner_a(1) + inner_b(2)\n"
           "\n"
           "def small():\n"
           "    def only_one():\n"
           "        return 1\n"
           "    return only_one()\n")
    (repo / "houses" / "app.py").write_text(src)
    # big() at line 1 with CC 20 (fake radon); small() line 9 below gate
    with Env(routes=git_routes(), functions=[[FakeFn("big", 1, 20), FakeFn("small", 9, 3)]]):
        actions = ch._latent_class_actions(repo, False, {}, {})
    latent = [a for a in actions if a.kind == "latent-class"]
    assert len(latent) == 1
    assert latent[0].function == "big"
    assert "inner_a" in latent[0].message and "inner_b" in latent[0].message
    assert "class in disguise" in latent[0].message


def test_latent_class_partition_signal(tmp_path):
    repo = make_repo(tmp_path)
    pad = "# pad\n" * 160
    src = ("class Big:\n" + "    # pad\n" * 160 + "\n"
           "    def __init__(self):\n"
           "        self.a = self.b = self.c = self.d = 0\n"
           "    def m1(self):\n"
           "        return self.a + self.b\n"
           "    def m2(self):\n"
           "        return self.a - self.b\n"
           "    def m3(self):\n"
           "        return self.c * self.d\n"
           "    def m4(self):\n"
           "        return self.c / self.d\n"
           "    def m5(self):\n"
           "        return self.a + self.b\n")
    (repo / "houses" / "app.py").write_text(src)
    with Env(routes=git_routes(), functions=[[]]):
        actions = ch._latent_class_actions(repo, False, {}, {})
    latent = [a for a in actions if a.kind == "latent-class"]
    assert len(latent) == 1
    assert latent[0].function == "Big"
    assert "m1" in latent[0].message and "m3" in latent[0].message  # both groups named
    assert "connectors removed" in latent[0].message


def test_latent_class_no_false_positive_on_shared_fields(tmp_path):
    """Two methods sharing one field is not a latent class — needs >= 2 fields per group."""
    repo = make_repo(tmp_path)
    pad = "# pad\n" * 160
    src = ("class NotFat:\n" + "    # pad\n" * 160 + "\n"
           "    def __init__(self):\n"
           "        self.flag = False\n"
           "    def m1(self):\n"
           "        return self.flag\n"
           "    def m2(self):\n"
           "        return not self.flag\n"
           "    def m3(self):\n"
           "        self.flag = True\n"
           "    def m4(self):\n"
           "        return self.flag\n"
           "    def m5(self):\n"
           "        return self.flag\n"
           "    def m6(self):\n"
           "        return self.flag\n")
    (repo / "houses" / "app.py").write_text(src)
    with Env(routes=git_routes(), functions=[[]]):
        actions = ch._latent_class_actions(repo, False, {}, {})
    assert [a for a in actions if a.kind == "latent-class"] == []


def test_latent_class_guidance_is_conditional():
    """The lesson is offered on the evidence, and coincidental grouping is left alone."""
    g = ch.GUIDANCE["latent-class"]
    assert "extract a class per group" in g
    assert "If the grouping is incidental" in g and "leave it" in g


def test_vague_name_thin_role_class_passes(tmp_path):
    """An MVC controller / event handler that only delegates is communicative."""
    repo = make_repo(tmp_path)
    (repo / "houses" / "app.py").write_text(
        "class PropertyController:\n"
        "    def __init__(self, service):\n"
        "        self.service = service\n"
        "    def get(self, rid):\n"
        "        return self.service.get(rid)\n"
        "    def post(self, body):\n"
        "        return self.service.save(body)\n")
    with Env(routes=git_routes(), functions=[[]]):
        actions = ch._latent_class_actions(repo, False, {}, {})
    assert [a for a in actions if a.kind == "vague-name"] == []


def test_vague_name_load_bearing_class_is_found(tmp_path):
    """A fat class hiding behind a role suffix is a finding — the domain noun should carry it."""
    repo = make_repo(tmp_path)
    pad = "    # pad\n" * 130
    src = ("class PropertyManager:\n" + pad +
           "    def __init__(self):\n"
           "        self.props = []\n"
           "    def add(self, p):\n"
           "        self.props.append(p)\n"
           "    def total(self):\n"
           "        return sum(p.price for p in self.props)\n")
    (repo / "houses" / "app.py").write_text(src)
    with Env(routes=git_routes(), functions=[[]]):
        actions = ch._latent_class_actions(repo, False, {}, {})
    vague = [a for a in actions if a.kind == "vague-name"]
    assert len(vague) == 1
    assert vague[0].function == "PropertyManager"
    assert "thin role class" in vague[0].message  # the exemption is stated
    assert "domain noun" in vague[0].message


def test_vague_name_guidance_states_the_principle():
    g = ch.GUIDANCE["vague-name"]
    assert "thin framework-role class" in g and "delegates" in g
    assert "domain noun" in g


def test_standard_inline_import_finding(tmp_path):
    repo = make_repo(tmp_path)
    (repo / "houses" / "app.py").write_text(
        "def load():\n    import json\n    return json.dumps({})\n")
    with Env(routes=git_routes(), functions=[[]]):
        actions = ch._latent_class_actions(repo, False, {}, {})
    std = [a for a in actions if a.kind == "standard"]
    assert len(std) == 1
    assert std[0].signal == "inline-import" if hasattr(std[0], "signal") else True
    assert "module top" in std[0].message


def test_standard_private_import_finding(tmp_path):
    repo = make_repo(tmp_path)
    (repo / "houses" / "app.py").write_text(
        "from houses._internal import secret\n"
        "def f():\n    return secret\n")
    with Env(routes=git_routes(), functions=[[]]):
        actions = ch._latent_class_actions(repo, False, {}, {})
    assert any(a.kind == "standard" and "private symbol" in a.message for a in actions)


def test_standard_bare_except_finding(tmp_path):
    repo = make_repo(tmp_path)
    (repo / "houses" / "app.py").write_text(
        "def f():\n"
        "    try:\n"
        "        g()\n"
        "    except Exception:\n"
        "        pass\n")
    with Env(routes=git_routes(), functions=[[]]):
        actions = ch._latent_class_actions(repo, False, {}, {})
    assert any(a.kind == "standard" and "swallows" in a.message for a in actions)


def test_standard_global_state_message_teaches_the_fix(tmp_path):
    repo = make_repo(tmp_path)
    (repo / "houses" / "app.py").write_text(
        "state = []\n"
        "def f():\n    global state\n    state.append(1)\n")
    with Env(routes=git_routes(), functions=[[]]):
        actions = ch._latent_class_actions(repo, False, {}, {})
    std = [a for a in actions if a.kind == "standard"]
    assert len(std) == 2  # module-level mutable list + global statement
    msg = " ".join(a.message for a in std)
    assert "entry point" in msg and "pass it around" in msg
    assert "services object" in msg and "fakes in tests" in msg


def test_standard_type_ignore_requires_a_why(tmp_path):
    repo = make_repo(tmp_path)
    (repo / "houses" / "app.py").write_text(
        "x: int = 1  # type: ignore\n"
        "y: int = 2  # type: ignore # pyright cannot see the kwarg type\n")
    with Env(routes=git_routes(), functions=[[]]):
        actions = ch._latent_class_actions(repo, False, {}, {})
    found = [a for a in actions if a.kind == "standard" and "type: ignore" in a.message]
    assert len(found) == 1  # only the un-justified one
    assert found[0].line == 1


def test_monkeypatch_finding_in_test_files(tmp_path):
    """monkeypatch is forbidden even in tests, which the health scan excludes."""
    repo = make_repo(tmp_path)
    (repo / "tests" / "unit" / "test_app.py").write_text(
        "def test_x(monkeypatch):\n"
        "    monkeypatch.setattr('m', 'a', 1)\n")
    (repo / "houses" / "app.py").write_text("def f():\n    return 1\n")
    with Env(routes=git_routes(), functions=[[]]):
        actions = ch._latent_class_actions(repo, False, {}, {})
    mp = [a for a in actions if a.kind == "standard" and "monkeypatch" in a.message]
    assert len(mp) == 1
    assert "fakes are objects, not functions" in mp[0].message


def test_monkeypatch_unittest_mock_finding(tmp_path):
    repo = make_repo(tmp_path)
    (repo / "tests" / "unit" / "test_app.py").write_text(
        "from unittest.mock import patch\n"
        "@patch('houses.app.f')\n"
        "def test_x(mock_f):\n"
        "    return mock_f()\n")
    with Env(routes=git_routes(), functions=[[]]):
        actions = ch._latent_class_actions(repo, False, {}, {})
    mp = [a for a in actions if a.kind == "standard" and "monkeypatch" in a.message]
    assert len(mp) >= 1  # the @patch decorator
    assert "inject an object fake" in mp[0].message


# --------------------------------------------------------------------------- suppression mechanism
def _standard_for(tmp_path, src):
    repo = make_repo(tmp_path)
    (repo / "houses" / "app.py").write_text(src)
    with Env(routes=git_routes(), functions=[[]]):
        return ch._latent_class_actions(repo, False, {}, {})


def test_suppression_on_same_line_exempts(tmp_path):
    actions = _standard_for(tmp_path, (
        "def f():\n"
        "    try:\n"
        "        g()\n"
        "    except ValueError:  # code-health: ignore except this error is safe to skip, logged\n"
        "        log('skipping')\n"))
    assert [a for a in actions if a.kind == "standard" and "swallows" in a.message] == []


def test_suppression_on_line_above_exempts(tmp_path):
    actions = _standard_for(tmp_path, (
        "def f():\n"
        "    try:\n"
        "        g()\n"
        "    # code-health: ignore except safe to skip, logged\n"
        "    except ValueError:\n"
        "        log('skipping')\n"))
    assert [a for a in actions if a.kind == "standard" and "swallows" in a.message] == []


def test_suppression_without_why_is_a_finding(tmp_path):
    actions = _standard_for(tmp_path, (
        "def f():\n"
        "    try:\n"
        "        g()\n"
        "    except ValueError:  # code-health: ignore except\n"
        "        log('skipping')\n"))
    std = [a for a in actions if a.kind == "standard"]
    # the un-explained suppression exempts nothing: the original except finding
    # fires AND the suppression-without-a-why finding fires
    assert len(std) == 2
    assert any("without a why" in a.message for a in std)
    assert any("swallows" in a.message for a in std)


def test_suppression_wrong_signal_does_not_exempt(tmp_path):
    actions = _standard_for(tmp_path, (
        "def f():\n"
        "    try:\n"
        "        g()\n"
        "    except ValueError:  # code-health: ignore inline-import not the right signal\n"
        "        log('skipping')\n"))
    assert len([a for a in actions if a.kind == "standard" and "swallows" in a.message]) == 1


def test_suppression_scoped_to_its_line(tmp_path):
    """A suppression on one except does not exempt a second except elsewhere."""
    actions = _standard_for(tmp_path, (
        "def f():\n"
        "    try:\n"
        "        g()\n"
        "    except ValueError:  # code-health: ignore except this one is safe, logged\n"
        "        log('a')\n"
        "    try:\n"
        "        h()\n"
        "    except ValueError:\n"
        "        log('b')\n"))
    remaining = [a for a in actions if a.kind == "standard" and "swallows" in a.message]
    assert len(remaining) == 1
    assert remaining[0].line == 8  # the second, unmarked except


def test_except_with_raise_is_not_a_finding(tmp_path):
    actions = _standard_for(tmp_path, (
        "def f():\n"
        "    try:\n"
        "        g()\n"
        "    except ValueError as e:\n"
        "        log('failed')\n"
        "        raise\n"))
    assert [a for a in actions if a.kind == "standard" and "swallows" in a.message] == []


def test_except_with_surfaced_return_is_not_a_finding(tmp_path):
    actions = _standard_for(tmp_path, (
        "def f():\n"
        "    try:\n"
        "        g()\n"
        "    except ValueError:\n"
        "        return 'failed: missing value'\n"))
    assert [a for a in actions if a.kind == "standard" and "swallows" in a.message] == []


def test_except_logging_only_is_a_finding(tmp_path):
    """The user's rule: logging alone is not fail-fast."""
    actions = _standard_for(tmp_path, (
        "def f():\n"
        "    try:\n"
        "        g()\n"
        "    except ValueError:\n"
        "        log('failed but invisible')\n"))
    std = [a for a in actions if a.kind == "standard" and "swallows" in a.message]
    assert len(std) == 1
    assert "Logging alone is not fail-fast" in std[0].message


def test_type_ignore_in_docstring_is_not_a_finding(tmp_path):
    actions = _standard_for(tmp_path, (
        '"""Example: # type: ignore inside a docstring is not a comment."""\n'
        "def f():\n"
        "    return 1\n"))
    assert [a for a in actions if a.kind == "standard" and "type: ignore" in a.message] == []


def test_type_ignore_real_comment_requires_why(tmp_path):
    actions = _standard_for(tmp_path, (
        "x: int = 1  # type: ignore\n"
        "y: int = 2  # type: ignore # pyright cannot see the kwarg type\n"))
    found = [a for a in actions if a.kind == "standard" and "type: ignore" in a.message]
    assert len(found) == 1
    assert found[0].line == 1


def test_except_returning_empty_dict_is_a_finding(tmp_path):
    """return {} on exception is silent degradation (fail-fast names it), not surfacing."""
    actions = _standard_for(tmp_path, (
        "def f():\n"
        "    try:\n"
        "        g()\n"
        "    except ValueError:\n"
        "        return {}\n"))
    assert len([a for a in actions if a.kind == "standard" and "swallows" in a.message]) == 1


# --------------------------------------------------------------------------- ABC / class-module / skipif / tuple-alias
def test_abc_single_concrete_is_a_finding(tmp_path):
    repo = make_repo(tmp_path)
    (repo / "houses" / "app.py").write_text(
        "from abc import ABC, abstractmethod\n"
        "class Base(ABC):\n"
        "    @abstractmethod\n"
        "    def run(self):\n"
        "        pass\n"
        "class Only(Base):\n"
        "    def run(self):\n"
        "        return 1\n")
    with Env(routes=git_routes(), functions=[[]]):
        actions = ch._abstraction_actions(repo, False, {}, {})
    over = [a for a in actions if "ceremony" in a.message]
    assert len(over) == 1
    assert "Base" in over[0].message and "Only" in over[0].message
    assert "ceremony" in over[0].message


def test_abc_with_two_subclasses_is_fine(tmp_path):
    repo = make_repo(tmp_path)
    (repo / "houses" / "app.py").write_text(
        "from abc import ABC, abstractmethod\n"
        "class Base(ABC):\n"
        "    @abstractmethod\n"
        "    def run(self):\n"
        "        pass\n"
        "class A(Base):\n"
        "    def run(self):\n"
        "        return 1\n"
        "class B(Base):\n"
        "    def run(self):\n"
        "        return 2\n")
    with Env(routes=git_routes(), functions=[[]]):
        actions = ch._abstraction_actions(repo, False, {}, {})
    assert [a for a in actions if "over-abstraction" in a.message] == []


def test_abc_cross_file_single_concrete(tmp_path):
    repo = make_repo(tmp_path)
    (repo / "houses" / "base.py").write_text(
        "from abc import ABC, abstractmethod\n"
        "class Strategy(ABC):\n"
        "    @abstractmethod\n"
        "    def run(self):\n"
        "        pass\n")
    (repo / "houses" / "impl.py").write_text(
        "from houses.base import Strategy\n"
        "class Fast(Strategy):\n"
        "    def run(self):\n"
        "        return 1\n")
    with Env(routes=git_routes(), functions=[[]]):
        actions = ch._abstraction_actions(repo, False, {}, {})
    over = [a for a in actions if "ceremony" in a.message]
    assert len(over) == 1
    assert "Fast" in over[0].message


def test_tuple_alias_hides_positional_record(tmp_path):
    repo = make_repo(tmp_path)
    (repo / "houses" / "app.py").write_text(
        "Key = tuple[str, str]\n"
        "Seq = tuple[str, ...]\n"
        "Lookup = dict[str, int]\n"
        "def f():\n    return 1\n")
    with Env(routes=git_routes(), functions=[[]]):
        actions = ch._latent_class_actions(repo, False, {}, {})
    aliases = [a for a in actions if "tuple-alias" in a.message or "positional record" in a.message]
    assert len(aliases) == 1
    assert "Key" in aliases[0].message and "LatLngPair" in aliases[0].message


def test_class_module_mismatch_is_a_finding(tmp_path):
    repo = make_repo(tmp_path)
    (repo / "houses" / "helpers.py").write_text(
        "class PropertyService:\n"
        "    def get(self):\n"
        "        return 1\n")
    with Env(routes=git_routes(), functions=[[]]):
        actions = ch._latent_class_actions(repo, False, {}, {})
    cm = [a for a in actions if "holds one class" in a.message]
    assert len(cm) == 1
    assert "helpers.py" in cm[0].message and "PropertyService" in cm[0].message


def test_class_module_matching_name_and_multi_class_pass(tmp_path):
    repo = make_repo(tmp_path)
    (repo / "houses" / "property_service.py").write_text(
        "class PropertyService:\n"
        "    def get(self):\n"
        "        return 1\n")
    (repo / "houses" / "models.py").write_text(
        "class A:\n    pass\n"
        "class B:\n    pass\n")
    with Env(routes=git_routes(), functions=[[]]):
        actions = ch._latent_class_actions(repo, False, {}, {})
    assert [a for a in actions if "holds one class" in a.message] == []


def test_skipif_on_environment_is_a_finding(tmp_path):
    repo = make_repo(tmp_path)
    (repo / "tests" / "unit" / "test_app.py").write_text(
        "import os\n"
        "import pytest\n"
        "@pytest.mark.skipif(os.environ.get('API_KEY') is None)\n"
        "def test_x():\n"
        "    pass\n")
    with Env(routes=git_routes(), functions=[[]]):
        actions = ch._latent_class_actions(repo, False, {}, {})
    sk = [a for a in actions if "skipif" in a.message]
    assert len(sk) == 1
    assert "fake it" in sk[0].message


def test_skipif_on_version_is_fine(tmp_path):
    repo = make_repo(tmp_path)
    (repo / "tests" / "unit" / "test_app.py").write_text(
        "import sys\n"
        "import pytest\n"
        "@pytest.mark.skipif(sys.version_info < (3, 11))\n"
        "def test_x():\n"
        "    pass\n")
    with Env(routes=git_routes(), functions=[[]]):
        actions = ch._latent_class_actions(repo, False, {}, {})
    assert [a for a in actions if "skipif" in a.message] == []

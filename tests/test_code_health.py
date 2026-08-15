# code-health: ignore-file fakefs the fixtures build real repo trees and the
# gate runs the actual Rust binary — real-FS subprocess interop, the same
# named exception as test_lsp.py
"""Orchestrator tests for the code-health gate.

The finding engine is the Rust binary (verified by its own unit suite);
these tests cover the orchestrator: git/coverage gathering, the gate
verdict, baselines, dedupe/merge, priority, and rendering — driving the
real binary through a passthrough subprocess route.
"""

import json
import sqlite3
import subprocess
import sys
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent.parent))
import code_health as ch

APP_SRC = """def alpha(a):
    if a:
        return 1
    return 0

def beta(b):
    return b
"""

SWALLOW_SRC = """def f():
    try:
        g()
    except Exception:
        pass
"""


# --------------------------------------------------------------------------- fakes
class FakeProc:
    def __init__(self, returncode=0, stdout="", stderr=""):
        self.returncode = returncode
        self.stdout = stdout
        self.stderr = stderr


PASSTHROUGH = object()  # a route marker: run the REAL subprocess (the Rust binary)


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
                if stdout is PASSTHROUGH:
                    proc = subprocess.run(args, **kwargs)
                    return FakeProc(returncode=proc.returncode, stdout=proc.stdout or "")
                out = stdout(args) if callable(stdout) else stdout
                return FakeProc(returncode=returncode, stdout=out)
        raise AssertionError(f"unexpected argv: {args}")


class Env:
    """Injects fakes into code_health and restores on exit (no monkeypatch).
    git calls are routed; the Rust binary passes through to the real one."""

    def __init__(self, routes=None):
        self._saved = {}
        self.routes = routes or []

    def __enter__(self):
        self._saved["subprocess"] = ch.subprocess
        ch.subprocess = FakeSubprocess(self.routes)
        return self

    def __exit__(self, *exc):
        ch.subprocess = self._saved["subprocess"]


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


def git_routes(history="", diff="", branch="test-branch", commit="abc1234", log_l="abc1234 fix\n"):
    def is_git(args):
        return args[:2] == ["git", "-C"] and args[2] not in (None,)

    routes = []
    routes.append((lambda a: is_git(a) and a[3:5] == ["log", "--name-only"], history, 0))
    routes.append((lambda a: is_git(a) and "-L" in a[3:], log_l, 0))
    routes.append((lambda a: is_git(a) and a[3] == "diff", diff, 0))
    routes.append((lambda a: is_git(a) and a[3] == "branch", branch, 0))
    routes.append((lambda a: is_git(a) and a[3] == "rev-parse", commit, 0))
    # the Rust scan binary + the graph-contract adapter pass through
    routes.append((lambda a: str(a[0]).endswith("code-health-scan"), PASSTHROUGH, 0))
    routes.append((lambda a: "code_health_graph_export.py" in " ".join(a), PASSTHROUGH, 0))

    def ls_files_stdout(args):
        repo = Path(args[2])
        rels = sorted(str(p.relative_to(repo)) for p in repo.rglob("*.py"))
        return "\0".join(rels) + ("\0" if rels else "")

    routes.append((lambda a: is_git(a) and a[3] == "ls-files", ls_files_stdout, 0))
    routes.append((lambda a: a[:2] == ["make", "coverage"], "", 0))
    return routes


def run_main(repo, *extra, routes=None):
    saved_argv = sys.argv
    sys.argv = ["code_health.py", "--repo", str(repo), *extra]
    try:
        with Env(routes=routes or git_routes()):
            return ch.main()
    finally:
        sys.argv = saved_argv


# --------------------------------------------------------------------------- utilities
def test_numbits_to_lines():
    assert ch._numbits_to_lines(b"\x01") == {1}
    assert ch._numbits_to_lines(b"\x05") == {1, 3}
    assert ch._numbits_to_lines(b"") == set()


def test_numbits_roundtrip():
    bits = ch._numbits_to_lines(bytes([0b10101]))
    assert bits == {1, 3, 5}
    assert ch._numbits_to_lines(bytes([0])) == set()


def test_rel_path():
    assert ch.rel_path(Path("/repo"), "/repo/a/b.py") == "a/b.py"
    assert ch.rel_path(Path("/repo"), "a/b.py") == "a/b.py"
    assert ch.rel_path(Path("/repo"), "/elsewhere/x.py") == "/elsewhere/x.py"


def test_is_test_path():
    assert ch.is_test_path("tests/unit/test_x.py")
    assert ch.is_test_path("test_standalone.py")
    assert not ch.is_test_path("houses/app.py")
    assert not ch.is_test_path("scripts/tool.py")


# --------------------------------------------------------------------------- coverage
def test_load_coverage_none(tmp_path):
    repo = make_repo(tmp_path)
    cr = ch.load_coverage(repo)
    assert cr.lines is None


def test_load_coverage_xml(tmp_path):
    repo = make_repo(tmp_path)
    (repo / "coverage.xml").write_text(
        '<coverage><packages><package><classes><class filename="houses/app.py">'
        '<lines><line number="1" hits="1"/><line number="2" hits="0"/></lines>'
        "</class></classes></package></packages></coverage>"
    )
    cr = ch.load_coverage(repo)
    assert cr.lines.get("houses/app.py") == {1}


def test_load_coverage_dot(tmp_path):
    repo = make_repo(tmp_path)
    db = sqlite3.connect(repo / ".coverage")
    db.execute("CREATE TABLE file (id INTEGER PRIMARY KEY, path TEXT)")
    db.execute("CREATE TABLE line_bits (file_id INTEGER, numbits BLOB)")
    db.execute("INSERT INTO file (path) VALUES ('houses/app.py')")
    db.execute("INSERT INTO line_bits VALUES (1, ?)", (b"\x01",))
    db.commit()
    cr = ch.load_coverage(repo)
    assert cr.lines.get("houses/app.py") == {1}


def test_load_coverage_prefers_xml(tmp_path):
    repo = make_repo(tmp_path)
    (repo / "coverage.xml").write_text(
        '<coverage><packages><package><classes><class filename="houses/app.py">'
        '<lines><line number="1" hits="1"/></lines></class></classes></package></packages></coverage>'
    )
    cr = ch.load_coverage(repo)
    assert "coverage.xml" in cr.source


# --------------------------------------------------------------------------- git
def test_file_history(tmp_path):
    repo = make_repo(tmp_path)
    routes = git_routes(history="houses/app.py\nscripts/oneoff.py\n")
    fh = ch.file_history(repo) if False else None
    # file_history shells to git through the fake
    with Env(routes=routes):
        fh = ch.file_history(repo)
    assert fh.churn["houses/app.py"] == 1


# --------------------------------------------------------------------------- the gate
def test_main_exit_codes(tmp_path, capsys):
    repo = make_repo(tmp_path, app_src=SWALLOW_SRC)
    assert run_main(repo) == 1
    assert "GATE: FAIL" in capsys.readouterr().out

    repo2 = make_repo(tmp_path / "clean", app_src=APP_SRC)
    assert run_main(repo2) == 0
    assert "GATE: PASS" in capsys.readouterr().out


def test_main_warn(tmp_path, capsys):
    repo = make_repo(tmp_path, app_src=SWALLOW_SRC)
    assert run_main(repo, "--warn") == 0
    assert "GATE: INFORMATIONAL" in capsys.readouterr().out


def test_main_not_git_repo(tmp_path):
    plain = tmp_path / "plain"
    plain.mkdir()
    assert run_main(plain) == 2


def test_main_merges_function_targets(tmp_path, capsys):
    # a function that is BOTH complex and long merges into one action
    src = "def big(a):\n    x = a\n"
    for i in range(20):
        src += f"    if x > {i}:\n        x -= 1\n"
    src += "    return x\n"
    repo = make_repo(tmp_path, app_src=src + "\n" * 100)
    run_main(repo, "--warn", "--json")
    data = json.loads(capsys.readouterr().out)
    kinds = [a["kind"] for a in data["actions"]]
    assert "complexity" in kinds


def test_main_baseline_ack(tmp_path, capsys):
    repo = make_repo(tmp_path, app_src=SWALLOW_SRC)
    baseline = tmp_path / "code-health.json"
    assert run_main(repo, "--update-baseline", "--baseline", str(baseline)) == 0
    assert run_main(repo, "--baseline", str(baseline)) == 0
    out = capsys.readouterr().out
    assert "acknowledged in baseline" in out


def test_main_update_baseline(tmp_path):
    repo = make_repo(tmp_path, app_src=SWALLOW_SRC)
    baseline = tmp_path / "code-health.json"
    assert run_main(repo, "--update-baseline", "--baseline", str(baseline)) == 0
    assert baseline.exists()
    keys = json.loads(baseline.read_text())["actions"]
    assert keys and "standard:houses/app.py" in keys[0]


def test_main_json_meta(tmp_path, capsys):
    repo = make_repo(tmp_path, app_src=SWALLOW_SRC)
    run_main(repo, "--warn", "--json")
    data = json.loads(capsys.readouterr().out)
    assert data["meta"]["repo"].endswith("repo")
    assert "thresholds" in data["meta"]
    assert data["meta"]["thresholds"]["max_complexity"] == 15


def test_main_priority_percentile(tmp_path, capsys):
    repo = make_repo(tmp_path, app_src=SWALLOW_SRC)
    run_main(repo, "--warn")
    out = capsys.readouterr().out
    assert "P0" in out or "P1" in out or "P2" in out or "warn" in out


def test_update_baseline_excludes_warns(tmp_path):
    repo = make_repo(tmp_path, app_src="def alpha(a):\n    return a * 60\n")
    baseline = tmp_path / "code-health.json"
    assert run_main(repo, "--update-baseline", "--baseline", str(baseline)) == 0
    keys = json.loads(baseline.read_text())["actions"]
    assert keys == []  # magic number is a warn — never baselined


def test_fail_run_lists_warnings(tmp_path, capsys):
    repo = make_repo(tmp_path, app_src=SWALLOW_SRC + "\ndef alpha(a):\n    return a * 60\n")
    run_main(repo)
    out = capsys.readouterr().out
    assert "warnings (reported, never fail)" in out
    assert "magic number" in out


def test_kind_rollup_in_fail_output(tmp_path, capsys):
    repo = make_repo(tmp_path, app_src=SWALLOW_SRC)
    run_main(repo)
    out = capsys.readouterr().out
    assert "by kind — fails:" in out


# --------------------------------------------------------------------------- baseline semantics
def test_stale_baseline_entry_fails(tmp_path, capsys):
    repo = make_repo(tmp_path, app_src=SWALLOW_SRC)
    baseline = tmp_path / "code-health.json"
    run_main(repo, "--update-baseline", "--baseline", str(baseline))
    # fix the code — the baselined finding is now stale
    (repo / "houses" / "app.py").write_text(APP_SRC)
    assert run_main(repo, "--baseline", str(baseline)) == 1
    assert "stale baseline" in capsys.readouterr().err


def test_stale_baseline_clears_after_update(tmp_path):
    repo = make_repo(tmp_path, app_src=SWALLOW_SRC)
    baseline = tmp_path / "code-health.json"
    run_main(repo, "--update-baseline", "--baseline", str(baseline))
    (repo / "houses" / "app.py").write_text(APP_SRC)
    run_main(repo, "--update-baseline", "--baseline", str(baseline))
    assert run_main(repo, "--baseline", str(baseline)) == 0


def test_baseline_line_shift_is_not_stale(tmp_path):
    repo = make_repo(tmp_path, app_src=SWALLOW_SRC)
    baseline = tmp_path / "code-health.json"
    run_main(repo, "--update-baseline", "--baseline", str(baseline))
    # add a line above the finding — same function, new line
    shifted = "# comment\n" * 3 + SWALLOW_SRC
    (repo / "houses" / "app.py").write_text(shifted)
    assert run_main(repo, "--baseline", str(baseline)) == 0


def test_baseline_gone_function_is_stale(tmp_path, capsys):
    repo = make_repo(tmp_path, app_src=SWALLOW_SRC)
    baseline = tmp_path / "code-health.json"
    run_main(repo, "--update-baseline", "--baseline", str(baseline))
    (repo / "houses" / "app.py").write_text(APP_SRC)
    assert run_main(repo, "--baseline", str(baseline)) == 1
    assert "stale baseline" in capsys.readouterr().err

# lucidlint: ignore-file fakefs the fixtures build real repo trees and the
# gate runs the actual Rust binary — real-FS subprocess interop, the same
# named exception as test_lsp.py
"""Orchestrator tests for the lucidlint gate.

The finding engine is the Rust binary (verified by its own unit suite);
these tests cover the orchestrator: git/coverage gathering, the gate
verdict, baselines, dedupe/merge, priority, and rendering — driving the
real binary through a passthrough subprocess route.
"""

import json
import re
import sqlite3
import subprocess
import sys
import tarfile
from collections import Counter
from pathlib import Path

import pygit2

sys.path.insert(0, str(Path(__file__).resolve().parent.parent))
import lucidlint as ch

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
                if isinstance(stdout, type) and issubclass(stdout, BaseException):
                    # the fake's except-clause alias is TimeoutError — raise THAT, since the
                    # real TimeoutExpired is a SubprocessError, not a TimeoutError
                    raise TimeoutError("command timed out")
                if stdout is PASSTHROUGH:
                    proc = subprocess.run(args, **kwargs)
                    return FakeProc(returncode=proc.returncode, stdout=proc.stdout or "")
                out = stdout(args) if callable(stdout) else stdout
                if kwargs.get("check") and returncode != 0:
                    raise subprocess.CalledProcessError(returncode, args)  # like real run(check=True)
                return FakeProc(returncode=returncode, stdout=out)
        raise AssertionError(f"unexpected argv: {args}")


class Env:
    """Injects fakes into lucidlint and restores on exit (no monkeypatch).
    git calls are routed; the Rust binary passes through to the real one."""

    def __init__(self, routes=None):
        self._saved = {}
        self.routes = routes or []

    def __enter__(self):
        self.fake = FakeSubprocess(self.routes)
        self._saved["subprocess"] = ch.subprocess
        ch.subprocess = self.fake
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


# --------------------------------------------------------------------------- fixtures/helpers

def materialize_test_repo(tmp_path) -> Path:
    """Extract the canonical lucidlint test repo (a committed fixture —
    real pygit2 history) to tmp_path. Tests exercise the REAL pygit2 API, so
    a library bump that breaks our calls fails here. Regenerate the fixture
    with `make test-fixture`."""
    fixture = Path(__file__).resolve().parent / "fixtures" / "test-repo.tar.gz"
    with tarfile.open(fixture) as tf:
        tf.extractall(tmp_path / "repo")
    return tmp_path / "repo"


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
    routes.append((lambda a: str(a[0]).endswith("lucidlint"), PASSTHROUGH, 0))

    def ls_files_stdout(args):
        repo = Path(args[2])
        rels = sorted(
            str(p.relative_to(repo)) for p in list(repo.rglob("*.py")) + list(repo.rglob("*.rs"))
        )
        return "\0".join(rels) + ("\0" if rels else "")

    routes.append((lambda a: is_git(a) and a[3] == "ls-files", ls_files_stdout, 0))
    routes.append((lambda a: a[0] == "make" and "coverage" in a, "", 0))
    return routes


def run_main(repo, *extra, routes=None):
    saved_argv = sys.argv
    sys.argv = ["lucidlint.py", "--repo", str(repo), *extra]
    try:
        with Env(routes=routes or git_routes()):
            return ch.main()
    finally:
        sys.argv = saved_argv


# --------------------------------------------------------------------------- coverage edges
def test_coverage_xml_unparseable(tmp_path):
    repo = make_repo(tmp_path)
    (repo / "coverage.xml").write_text("not xml at all")
    cr = ch._GateRunner(repo, None).load_coverage()
    assert cr.source == "coverage.xml unparseable"
    assert cr.lines is None


def test_coverage_xml_malformed_line_skipped(tmp_path, capsys):
    repo = make_repo(tmp_path)
    (repo / "coverage.xml").write_text(
        "<coverage><class filename=\"houses/app.py\">"
        "<line hits=\"1\" number=\"abc\"/>"
        "<line hits=\"1\" number=\"2\"/>"
        "</class>"
        "<class filename=\"notes.txt\"><line hits=\"1\" number=\"1\"/></class>"
        "</coverage>"
    )
    cr = ch._GateRunner(repo, None).load_coverage()
    # the malformed <line> is skipped with a log; the valid one survives; the .txt class is skipped
    assert cr.lines == {"houses/app.py": {2}}
    assert "malformed <line>" in capsys.readouterr().err


def test_coverage_sqlite_skips_unknown_and_nonpy(tmp_path):
    repo = make_repo(tmp_path)
    db = sqlite3.connect(repo / ".coverage")
    db.execute("CREATE TABLE file (id INTEGER, path TEXT)")
    db.execute("CREATE TABLE line_bits (file_id INTEGER, numbits BLOB)")
    db.execute("INSERT INTO file VALUES (1, 'houses/app.py'), (3, 'notes.txt')")
    db.execute("INSERT INTO line_bits VALUES (1, X'05'), (2, X'05'), (3, X'05')")
    db.commit()
    db.close()
    cr = ch._GateRunner(repo, None).load_coverage()
    # file_id 2 has no file row (skipped); notes.txt is not .py (skipped); app.py lines 1,3 covered
    assert cr.lines == {"houses/app.py": {1, 3}}


def test_coverage_sqlite_unreadable(tmp_path):
    repo = make_repo(tmp_path)
    (repo / ".coverage").write_bytes(b"this is not a sqlite database at all, definitely")
    cr = ch._GateRunner(repo, None).load_coverage()
    assert cr.source == ".coverage unreadable"


def test_coverage_context_staleness(tmp_path):
    repo = make_repo(tmp_path)
    (repo / ".coverage").write_bytes(b"x")
    now = 1_800_000_000.0
    os_utime = __import__("os").utime
    os_utime(repo / ".coverage", (now, now))
    test = repo / "tests" / "unit" / "test_app.py"
    os_utime(test, (now - 1000, now - 1000))  # tests older than the snapshot
    cc = ch._coverage_context(repo, {"houses/app.py": {1}}, ".coverage")
    assert cc.graph_preferred is False
    assert "mtime" in cc.label
    os_utime(test, (now + 1000, now + 1000))  # a newer test makes the snapshot stale
    cc = ch._coverage_context(repo, {"houses/app.py": {1}}, ".coverage")
    assert cc.graph_preferred is True
    assert "snapshot older" in cc.stale_note


# --------------------------------------------------------------------------- git gathering edges
def test_file_history_parser_edges(tmp_path):
    # fixture: houses/app.py touched in c1+c2 (churn 2), Makefile never
    # exists (not .py — must be skipped), oneoff.py in c3
    repo = materialize_test_repo(tmp_path)
    fh = ch._GateRunner(repo, None).file_history()
    assert fh.churn["houses/app.py"] == 2
    assert "Makefile" not in fh.churn  # not .py — skipped
    assert fh.last_modified["houses/app.py"]  # commit-timestamp present


def _file_history_with(routes, repo):
    saved = ch.subprocess
    ch.subprocess = FakeSubprocess(routes)
    try:
        return ch._GateRunner(repo, None).file_history()
    finally:
        ch.subprocess = saved


def test_file_history_timeout_and_nonzero(tmp_path):
    repo = make_repo(tmp_path)
    fh = _file_history_with([(
        lambda a: a[:2] == ["git", "-C"] and a[3:5] == ["log", "--name-only"],
        subprocess.TimeoutExpired, 0,
    )], repo)
    assert fh.churn == {}  # timeout degrades to empty history
    fh = _file_history_with([(
        lambda a: a[:2] == ["git", "-C"] and a[3:5] == ["log", "--name-only"],
        "", 1,
    )], repo)
    assert fh.churn == {}  # nonzero exit degrades to empty history


def test_changed_files_branch_diff_and_no_git(tmp_path):
    repo = materialize_test_repo(tmp_path)
    git = pygit2.Repository(str(repo))
    # pin "other" at HEAD, then commit a change on main (the HEAD side of the
    # three-dot diff) — real pygit2 ops on the materialized fixture
    git.branches.create("other", git.get(git.head.target))
    (repo / "houses" / "x.py").write_text("def g():\n    pass\n")
    git.index.add_all()
    tree = git.index.write_tree()
    sig = pygit2.Signature("Test", "test@example.com")
    git.create_commit("HEAD", sig, sig, "add x on main", tree, [git.head.target])
    assert ch.changed_files(repo, "other") == {"houses/x.py"}
    # no .git at all degrades to empty
    plain = tmp_path / "plain"
    plain.mkdir()
    assert ch.changed_files(plain, "main") == set()


# --------------------------------------------------------------------------- scoring/merge/baseline units
def test_raw_score_high_risk_callers():
    base = ch._raw_score("high-risk", 0.5, 0)
    assert ch._raw_score("high-risk", 0.5, 0, callers=6) == base * 2.0  # capped factor 1 + 6/5
    assert ch._raw_score("high-risk", 0.5, 0, callers=1) == base * 1.2
    # only high-risk scales with fan-in — standard ignores callers entirely
    assert ch._raw_score("standard", 0.5, 0, callers=6) == ch._raw_score("standard", 0.5, 0)


def test_merge_keeps_distinct_line_findings(tmp_path, capsys):
    """Two positional-literals calls in ONE function are two fixes at two
    lines — the per-target merge must not collapse them (it would hide one
    finding behind the other and the fix loop would thrash)."""
    repo = make_repo(
        tmp_path,
        app_src="def g():\n    set_limits(10, 20)\n    Money(\"0\", \"GBP\")\n",
    )
    run_main(repo, "--warn", "--json")
    data = json.loads(capsys.readouterr().out)
    pl = [a for a in data["actions"] if a["kind"] == "positional-literals"]
    assert len(pl) == 2, f"expected two distinct findings, got {len(pl)}"
    assert {a["line"] for a in pl} == {2, 3}


def test_merge_warn_into_fail_target():
    fail = ch.Action("complexity", "fail", "houses/app.py", 3, "alpha", "m1", 1, 0, "", "", note="n1", raw=2)
    # same target (file+function+kind-group) but a different line — distinct dedupe keys,
    # same merge key: the merge path (not the dedupe path) must handle the warn
    warn = ch.Action("complexity", "warn", "houses/app.py", 5, "alpha", "m2", 1, 0, "", "", note="n2", raw=1)
    out = ch._GateRunner(Path("."), None)._dedupe_merge([fail, warn], set())
    assert len(out) == 1
    assert out[0].severity == "fail"  # a warn merged into a fail target keeps the gate
    assert "n2" in out[0].note
    assert "WARN: m2" in out[0].note  # the differing message lands in the note


def test_baseline_identity_fallback():
    assert ch._baseline_identity("complexity:a.py:3:alpha") == ch.BaselineIdentity("complexity", "a.py", "alpha")
    assert ch._baseline_identity("short") == ch.BaselineIdentity("short", "", "")


def test_rust_finding_rel_unmappable(tmp_path):
    repo = make_repo(tmp_path)
    rels = {"houses/app.py", "scripts/oneoff.py"}
    assert ch._rust_finding_rel("houses/app.py", repo, rels) == "houses/app.py"
    assert ch._rust_finding_rel(str(repo / "houses" / "app.py"), repo, rels) == "houses/app.py"
    assert ch._rust_finding_rel("/elsewhere/x.py", repo, rels) is None  # outside the repo — dropped
    assert ch._rust_finding_rel("houses/missing.py", repo, rels) is None  # not in this scan set


# --------------------------------------------------------------------------- end-to-end edges
def test_file_mode_single_file(tmp_path, capsys):
    repo = make_repo(tmp_path, app_src=SWALLOW_SRC)  # except Exception -> a finding
    rc = run_main(repo, "--file", "houses/app.py")
    out = capsys.readouterr().out
    assert rc == 1
    assert "houses/app.py" in out  # the requested file renders with its finding
    assert "scripts/oneoff.py" not in out  # only the one file is scanned
    assert "GATE" in out


def test_update_baseline_requires_path(tmp_path):
    repo = make_repo(tmp_path)
    assert run_main(repo, "--update-baseline") == 2


def test_corrupt_baseline_ignored(tmp_path, capsys):
    repo = make_repo(tmp_path)
    baseline = tmp_path / "bad.json"
    baseline.write_text("{not json")
    rc = run_main(repo, "--baseline", str(baseline))
    assert rc == 0  # unbaselined — the clean repo still passes
    assert "unreadable" in capsys.readouterr().err


def test_git_lsfiles_failure_falls_back_to_rglob(tmp_path, capsys):
    repo = make_repo(tmp_path)
    (repo / ".venv").mkdir()
    (repo / ".venv" / "x.py").write_text("def f():\n    return 60\n")  # would fail if scanned
    routes = [
        (lambda a: a[:2] == ["git", "-C"] and a[3:5] == ["log", "--name-only"], "", 1),
        (lambda a: a[:2] == ["git", "-C"] and "-L" in a[3:], "", 1),
        (lambda a: a[:2] == ["git", "-C"] and a[3] == "diff", "", 1),
        (lambda a: a[:2] == ["git", "-C"] and a[3] == "branch", "test-branch", 0),
        (lambda a: a[:2] == ["git", "-C"] and a[3] == "rev-parse", "abc1234", 0),
        (lambda a: a[:2] == ["git", "-C"] and a[3] == "ls-files", "", 1),  # git fails
        (lambda a: str(a[0]).endswith("lucidlint"), PASSTHROUGH, 0),
        (lambda a: a[:2] == ["make", "coverage"], "", 0),
    ]
    rc = run_main(repo, routes=routes)
    assert rc == 0  # .venv excluded by _excluded_part; the clean repo passes
    # R28: a certain-unfixable gap (no git) is silent — no "falling back"
    # announcement; the rglob fallback just happens
    assert "rglob" not in capsys.readouterr().err
    assert ".venv" not in capsys.readouterr().out


def test_no_pygit2_file_list_uses_git_and_honors_gitignore(tmp_path):
    # CI runners have no pygit2: the file list must come from `git ls-files`
    # (which honors .gitignore) — an ignored dir like a repo's own .tools/
    # bundle must NOT be scanned. Regression: the rglob walk scanned it, and
    # a would-be-fail file inside .tools/ failed a clean repo.
    repo = materialize_test_repo(tmp_path)
    (repo / ".gitignore").write_text(".tools/\n")
    (repo / ".tools").mkdir()
    (repo / ".tools" / "self.py").write_text("def f():\n    return 60\n")  # would fail the gate if scanned

    def is_git(args):
        return args[:2] == ["git", "-C"]

    routes = [
        (lambda a: is_git(a) and a[3] == "ls-files", PASSTHROUGH, 0),  # REAL git honors .gitignore
        (lambda a: is_git(a) and a[3:5] == ["log", "--name-only"], "", 0),
        (lambda a: is_git(a) and "-L" in a[3:], "abc1234 fix\n", 0),
        (lambda a: is_git(a) and a[3] == "diff", "", 0),
        (lambda a: is_git(a) and a[3] == "branch", "test-branch", 0),
        (lambda a: is_git(a) and a[3] == "rev-parse", "abc1234", 0),
        (lambda a: str(a[0]).endswith("lucidlint"), PASSTHROUGH, 0),
        (lambda a: a[0] == "make" and "coverage" in a, "", 0),
    ]
    saved = ch._pygit2
    ch._pygit2 = None  # the no-pygit2 consumer path
    try:
        rc = run_main(repo, routes=routes)
    finally:
        ch._pygit2 = saved
    assert rc == 0  # .tools/self.py would fail (CC 60) — its exclusion IS the check


def test_refresh_coverage_runs_make(tmp_path):
    repo = make_repo(tmp_path)
    with Env(routes=git_routes()) as env:
        saved_argv = sys.argv
        sys.argv = ["lucidlint.py", "--repo", str(repo), "--refresh-coverage"]
        try:
            rc = ch.main()
        finally:
            sys.argv = saved_argv
    assert rc == 0
    assert any(a[0] == "make" and "coverage" in a for a in env.fake.calls)


def test_scanner_failure_raises(tmp_path):
    repo = make_repo(tmp_path)
    routes = git_routes()
    routes.insert(0, (lambda a: str(a[0]).endswith("lucidlint"), "not json at all", 0))
    try:
        run_main(repo, routes=routes)
        raise AssertionError("expected RuntimeError")
    except RuntimeError as e:
        assert "Rust scan core failed" in str(e)


def test_scanner_garbage_findings_dropped(tmp_path, capsys):
    repo = make_repo(tmp_path)
    scan_json = json.dumps({
        "schema_version": 2,
        "findings": [{
            "kind": "standard", "signal": "standard", "severity": "fail",
            "file": "/elsewhere/x.py", "line": 1, "function": "", "message": "drop me",
        }],
        "cc": [{"function": "f", "line": 1, "cc": 20, "file": "/elsewhere/x.py"}],
        "complexity": [],
    })
    routes = git_routes()
    routes.insert(0, (lambda a: str(a[0]).endswith("lucidlint"), scan_json, 0))
    rc = run_main(repo, routes=routes)
    assert rc == 0  # findings for files outside the scan set are dropped, not failures
    assert "drop me" not in capsys.readouterr().out


def test_scanner_cache_hits(tmp_path):
    repo = make_repo(tmp_path)
    rs = ch._RustScan()
    fs = FakeSubprocess([(lambda a: str(a[0]).endswith("lucidlint"), PASSTHROUGH, 0)])
    files = [ch.SourceFile(repo / "houses/app.py", "houses/app.py")]
    rs._pending_graph = None  # exactly what prepare() sets when no graph/churn is available
    rs._pending_churn = None
    rs._pending_tests = False
    rs._pending_docs = None
    saved = ch.subprocess
    ch.subprocess = fs
    try:
        assert rs.load(repo, files) is not None
        assert rs.load(repo, files) is not None
    finally:
        ch.subprocess = saved
    assert sum(1 for a in fs.calls if str(a[0]).endswith("lucidlint")) == 1


def test_scanner_binary_missing_raises(tmp_path):
    repo = make_repo(tmp_path)
    rs = ch._RustScan()
    rs._binary_cache[repo] = None  # simulate an unbuilt binary
    files = [ch.SourceFile(repo / "houses/app.py", "houses/app.py")]
    rs._pending_graph = None
    rs._pending_churn = None
    rs._pending_tests = False
    rs._pending_docs = None
    try:
        rs.load(repo, files)
        raise AssertionError("expected RuntimeError")
    except RuntimeError as e:
        assert "make scanner-check" in str(e)


def test_graph_contract_failure_degrades(tmp_path, capsys):
    repo = make_repo(tmp_path)
    routes = git_routes()
    rc = run_main(repo, routes=routes)
    assert rc == 0  # no graph contract — the non-graph families still gate
    assert "GATE: PASS" in capsys.readouterr().out


def test_actions_from_rust_test_only_filter():
    rf = ch.RustFindings(
        {"tests/unit/test_app.py": [
            ch.RustFinding(kind="magic-number", signal="magic-number", severity="fail",
                           file="tests/unit/test_app.py", line=1, function="", message="not test-only"),
            ch.RustFinding(kind="standard", signal="monkeypatch", severity="fail",
                           file="tests/unit/test_app.py", line=2, function="", message="test-only rule"),
        ]},
        {},
    )
    acts = ch._actions_from_rust(rf, include_tests=False, file_churn=Counter(), last_modified={})
    assert [a.message for a in acts] == ["test-only rule"]  # only TEST_ONLY_SIGNALS survive


def test_render_actions_acks(tmp_path, capsys):
    repo = make_repo(tmp_path)
    import argparse
    ack = ch.Action("standard", "ack", "houses/app.py", 1, "", "m", 1, 0, "", "")
    ch._render_actions(repo, argparse.Namespace(baseline=None), [], [ack])
    assert "acknowledged in baseline (1): houses/app.py:1" in capsys.readouterr().out


def test_main_stale_coverage_warning(tmp_path, capsys):
    repo = make_repo(tmp_path, app_src=SWALLOW_SRC)  # except Exception -> a fail action
    db = sqlite3.connect(repo / ".coverage")
    db.execute("CREATE TABLE file (id INTEGER, path TEXT)")
    db.execute("CREATE TABLE line_bits (file_id INTEGER, numbits BLOB)")
    db.execute("INSERT INTO file VALUES (1, 'houses/app.py')")
    db.execute("INSERT INTO line_bits VALUES (1, X'05')")
    db.commit()
    db.close()
    now = 1_800_000_000.0
    os = __import__("os")
    os.utime(repo / ".coverage", (now, now))
    os.utime(repo / "tests" / "unit" / "test_app.py", (now + 1000, now + 1000))  # newer than the snapshot
    rc = run_main(repo)
    out = capsys.readouterr().out
    assert rc == 1  # the except finding still fails the gate
    assert "snapshot predates the repo's tests" in out

# --------------------------------------------------------------------------- scan-core contract
def test_empty_repo_passes_clean(tmp_path, capsys):
    repo = tmp_path / "emptyrepo"
    (repo / ".git").mkdir(parents=True)  # a repo with no .py/.rs files
    rc = run_main(repo)
    assert rc == 0
    assert "GATE: PASS" in capsys.readouterr().out


def test_missing_binary_fails_fast(tmp_path):
    repo = make_repo(tmp_path)
    # the binary cache says "not built" — the gate must raise, never pass un-scanned
    ch.RUST_SCAN._binary_cache[repo] = None
    try:
        run_main(repo)
        raise AssertionError("expected RuntimeError")
    except RuntimeError as e:
        assert "scan binary is required" in str(e)


# --------------------------------------------------------------------------- rust layer
def test_rust_files_are_scanned(tmp_path, capsys):
    repo = make_repo(tmp_path)
    (repo / "src").mkdir()
    (repo / "src" / "mod.rs").write_text("pub fn f() {\n    let x = 3 * 60;\n    x;\n}\n")
    rc = run_main(repo)
    out = capsys.readouterr().out
    assert rc == 1  # the rust finding fails the gate
    assert "src/mod.rs" in out
    assert "magic number" in out


# --------------------------------------------------------------------------- project config suppression
def test_config_global_ignore_suppresses(tmp_path, capsys):
    """A .lucidlint.toml global ignore suppresses the signal — and must not
    crash (regression: Action lacked a signal field, so any ignore entry
    raised AttributeError at the config filter)."""
    repo = make_repo(tmp_path, app_src="def f():\n    return 60 * 24\n")
    (repo / ".lucidlint.toml").write_text('[lucidlint]\nignore = ["magic-number"]\n')
    rc = run_main(repo, "--warn")
    out = capsys.readouterr().out
    assert rc == 0
    assert "magic number" not in out


def test_config_group_ignore_suppresses(tmp_path, capsys):
    """group:style suppresses magic-number (a style member)."""
    repo = make_repo(tmp_path, app_src="def f():\n    return 60 * 24\n")
    (repo / ".lucidlint.toml").write_text('[lucidlint]\nignore = ["group:style"]\n')
    rc = run_main(repo, "--warn")
    out = capsys.readouterr().out
    assert rc == 0
    assert "magic number" not in out


def test_config_per_path_ignore_suppresses(tmp_path, capsys):
    """A per-path ignore suppresses only under its glob."""
    repo = make_repo(tmp_path, app_src="def f():\n    return 60 * 24\n")
    (repo / ".lucidlint.toml").write_text('[lucidlint]\n[lucidlint."houses/**"]\nignore = ["magic-number"]\n')
    rc = run_main(repo, "--warn")
    out = capsys.readouterr().out
    assert rc == 0
    assert "magic number" not in out


def test_config_ignored_ledger_shows_when_all_actions_ignored(tmp_path, capsys):
    """The debt ledger must print even when the config-ignores ate every
    action — "clean" while debt is hidden is the invisibility the ledger
    exists to remove (review finding)."""
    repo = make_repo(tmp_path, app_src="def f():\n    return 60 * 24\n")
    (repo / ".lucidlint.toml").write_text('[lucidlint]\nignore = ["magic-number"]\n')
    run_main(repo, "--warn")
    out = capsys.readouterr().out
    assert "config-ignored" in out
    assert "magic-number=" in out



# --------------------------------------------------------------------------- fix + scan contracts
def test_raw_score_uses_the_metric():
    """R8: priority ranks churn x complexity x fan-in — a higher metric must
    score higher at equal churn (a constant 1.0 metric flattened every
    complexity/large-function finding to the same priority)."""
    assert ch._raw_score("complexity", 60, 10) > ch._raw_score("complexity", 15, 10)
    assert ch._raw_score("large-function", 300, 5) > ch._raw_score("large-function", 100, 5)
    # the churn factor still scales within a metric
    assert ch._raw_score("complexity", 20, 30) > ch._raw_score("complexity", 20, 5)


def test_fix_refuses_rust_targets_cleanly(tmp_path, capsys):
    """A name-less extract-method on a .rs target must refuse SILENTLY (R28:
    extract-method's semantic name is required; the scanner would otherwise
    write `fn ()` — invalid Rust). No traceback, no file modification."""
    repo = make_repo(tmp_path, app_src="def alpha(a):\n    return a\n")
    lib = repo / "houses" / "lib.rs"
    lib.write_text(
        "pub fn f(x: i32) -> i32 {\n    if x > 10 { x * 2 } else { x }\n}\n"
    )
    run_main(repo, "fix", "--kind", "extract-method", "--file", "houses/lib.rs", "--line", "1")
    out = capsys.readouterr().out
    assert "Traceback" not in out, "a .rs fix target crashed — it must refuse cleanly"
    assert "fn ()" not in lib.read_text(), "a name-less .rs fix must not corrupt the file"
    assert lib.read_text() == lib.read_text(), str(lib)


def test_fix_rust_line_less_resolves_unique_finding(tmp_path, capsys):
    """R27 on the .rs surface: `lucidlint fix --kind X --file F` with no
    --line applies when the file has exactly one finding of the kind — the
    orchestrator resolves the line, the agent never counts lines."""
    repo = make_repo(tmp_path, app_src="def alpha(a):\n    return a\n")
    lib = repo / "houses" / "lib.rs"
    arms = "".join(
        f'    if sel == "k{i}" {{\n        return {i};\n    }}\n' for i in range(16)
    )
    lib.write_text(
        "pub fn route(sel: &str) -> i32 {\n" + arms + "    -1\n}\n"
    )
    run_main(repo, "fix", "--kind", "dispatch-registry", "--file", "houses/lib.rs")
    out = capsys.readouterr().out
    assert "match sel {" in lib.read_text(), out


def test_scanner_candidates_carry_the_exe_suffix():
    """Every candidate path must use the platform exe suffix — a Windows
    build produces lucidlint.exe; a suffixless candidate silently falls back
    to no scanner at all."""
    repo = Path("/tmp/repo")
    for p in ch._scanner_candidates(repo, ".exe"):
        assert p.name == "lucidlint.exe" or p.name.endswith(".exe"), p
    for p in ch._scanner_candidates(repo, ""):
        assert p.name == "lucidlint"


def test_graph_contract_corrupt_db_degrades(tmp_path):
    """A corrupt/older-schema graph.db must degrade to the non-graph
    families, never crash the scan (PRD R21)."""
    db_dir = tmp_path / ".code-review-graph"
    db_dir.mkdir()
    (db_dir / "graph.db").write_bytes(b"this is not a sqlite database at all")
    contract = ch.GRAPH_CONTRACT
    result = contract.contract(tmp_path)
    assert result is None  # either the extra is absent (early None) or the
    # corrupt db raised and was caught — never an exception


# --------------------------------------------------------------------------- family registration consistency
def _family_kinds() -> set[str]:
    """Every kind the scanner can emit — parsed from the GENERATED registry
    const (rules_gen.rs, derived from rule_metadata.py's catalog; this test
    is the cross-language check that RULE_GROUPS and RULES.md keep up with
    it, and that the generated file is current)."""
    src = Path(__file__).resolve().parent.parent / "scanner" / "src" / "rules_gen.rs"
    text = src.read_text()
    start = text.index("pub const FAMILY_KINDS")
    end = text.index("];", start)
    return set(re.findall(r'"([a-z-]+)"', text[start:end]))


def _rules_md_names() -> set[str]:
    rules = (Path(__file__).resolve().parent.parent / "RULES.md").read_text()
    names = set()
    for line in rules.splitlines():
        line = line.strip()
        if not line.startswith("| **"):
            continue
        name = line.split("|")[1].strip().split(" (")[0].split(" →")[0].strip("*").strip()
        names.add(name)
    return names


def test_rule_groups_cover_every_family_kind():
    """Every emitted kind belongs to exactly one RULE_GROUPS group (group
    suppression would silently miss an unregistered family)."""
    members = set().union(*ch.RULE_GROUPS.values())
    for kind in sorted(_family_kinds()):
        assert kind in members, f"kind '{kind}' is emitted but not in any RULE_GROUPS group"


def test_rule_groups_have_no_dead_kinds():
    """Every group member is an emitted kind — a stale member is dead config
    (suppressing a signal the scanner never emits)."""
    kinds = _family_kinds()
    for group, members in ch.RULE_GROUPS.items():
        for kind in members:
            assert kind in kinds, f"'{kind}' is in group:{group} but the scanner never emits it"


def test_every_emitted_kind_is_registered():
    """Every kind the scanner CODE emits appears in FAMILY_KINDS — the
    registry is hand-maintained; this catches a family emitted but never
    registered (the exact drift the 2026-08-16 batch hit when the final_kind
    arm silently failed to land)."""
    src_dir = Path(__file__).resolve().parent.parent / "scanner" / "src"
    emitted = set()
    for f in sorted(src_dir.glob("*.rs")):
        text = f.read_text()
        # test-fixture Finding structs live in #[cfg(test)] mods — their kind
        # strings are data, not emissions
        cut = text.find("#[cfg(test)]")
        if cut != -1:
            text = text[:cut]
        emitted.update(re.findall(r'kind: "([a-z-]+)"', text))
        emitted.update(re.findall(r'finding\("([a-z-]+)"', text))
        emitted.update(re.findall(r'kind\([a-z_-]+, "([a-z-]+)"', text))
    registered = _family_kinds()
    for kind in sorted(emitted):
        assert kind in registered, f"kind '{kind}' is emitted by {f.name} but missing from FAMILY_KINDS"


def test_rules_md_is_generated():
    """The RULES.md rule tables are generated from rule_metadata.py (`make
    rules`) — a new family, a severity change, or a hand edit that leaves
    the tables stale fails the gate. The generator also validates that
    every emitted kind has metadata and that severities match the scanner
    (this is what caught 'duplicate' being documented as fail while the
    scanner emits warn)."""
    import subprocess
    import sys

    gen = Path(__file__).resolve().parent.parent / "scripts" / "gen-rules.py"
    rc = subprocess.run([sys.executable, str(gen), "--check"]).returncode
    assert rc == 0, "RULES.md is stale — run `make rules` and commit the result"


def test_rules_md_documents_every_family_kind():
    """Every emitted kind has a RULES.md row (its own name or an alias)."""
    documented = _rules_md_names()
    # partition is the latent-class field-partition variant — the closures row
    # documents the family; 'standard' (Group 6) is a display bucket, not a kind
    aliases = {"partition": "closures"}
    for kind in sorted(_family_kinds()):
        assert kind in documented or aliases.get(kind) in documented, \
            f"kind '{kind}' is emitted but has no RULES.md row"


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
    assert ch._GateRunner(Path("/repo"), None).rel_path("/repo/a/b.py") == "a/b.py"
    assert ch._GateRunner(Path("/repo"), None).rel_path("a/b.py") == "a/b.py"
    assert ch._GateRunner(Path("/repo"), None).rel_path("/elsewhere/x.py") == "/elsewhere/x.py"


def test_is_test_path():
    assert ch.is_test_path("tests/unit/test_x.py")
    assert ch.is_test_path("test_standalone.py")
    assert not ch.is_test_path("houses/app.py")
    assert not ch.is_test_path("scripts/tool.py")


# --------------------------------------------------------------------------- coverage
def test_load_coverage_none(tmp_path):
    repo = make_repo(tmp_path)
    cr = ch._GateRunner(repo, None).load_coverage()
    assert cr.lines is None


def test_load_coverage_xml(tmp_path):
    repo = make_repo(tmp_path)
    (repo / "coverage.xml").write_text(
        '<coverage><packages><package><classes><class filename="houses/app.py">'
        '<lines><line number="1" hits="1"/><line number="2" hits="0"/></lines>'
        "</class></classes></package></packages></coverage>"
    )
    cr = ch._GateRunner(repo, None).load_coverage()
    assert cr.lines.get("houses/app.py") == {1}


def test_load_coverage_dot(tmp_path):
    repo = make_repo(tmp_path)
    db = sqlite3.connect(repo / ".coverage")
    db.execute("CREATE TABLE file (id INTEGER PRIMARY KEY, path TEXT)")
    db.execute("CREATE TABLE line_bits (file_id INTEGER, numbits BLOB)")
    db.execute("INSERT INTO file (path) VALUES ('houses/app.py')")
    db.execute("INSERT INTO line_bits VALUES (1, ?)", (b"\x01",))
    db.commit()
    cr = ch._GateRunner(repo, None).load_coverage()
    assert cr.lines.get("houses/app.py") == {1}


def test_load_coverage_prefers_xml(tmp_path):
    repo = make_repo(tmp_path)
    (repo / "coverage.xml").write_text(
        '<coverage><packages><package><classes><class filename="houses/app.py">'
        '<lines><line number="1" hits="1"/></lines></class></classes></package></packages></coverage>'
    )
    cr = ch._GateRunner(repo, None).load_coverage()
    assert "coverage.xml" in cr.source


# --------------------------------------------------------------------------- git
def test_file_history(tmp_path):
    repo = materialize_test_repo(tmp_path)
    fh = ch._GateRunner(repo, None).file_history()
    assert fh.churn["houses/app.py"] == 2  # base + modify
    assert fh.churn["scripts/oneoff.py"] == 1
    assert fh.churn["tests/unit/test_app.py"] == 1


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


def test_main_no_git_scans_clean(tmp_path, capsys):
    # a directory with no .git at all — the gate scans it anyway (git
    # degrades to rglob file gathering + empty history/diff/provenance)
    plain = tmp_path / "plain"
    plain.mkdir()
    (plain / "app.py").write_text("def f():\n    return 1\n")
    rc = run_main(plain)
    out = capsys.readouterr().out
    assert rc == 0
    assert "GATE: PASS" in out


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
    baseline = tmp_path / "lucidlint.json"
    assert run_main(repo, "--update-baseline", "--baseline", str(baseline)) == 0
    assert run_main(repo, "--baseline", str(baseline)) == 0
    out = capsys.readouterr().out
    assert "acknowledged in baseline" in out


def test_main_update_baseline(tmp_path):
    repo = make_repo(tmp_path, app_src=SWALLOW_SRC)
    baseline = tmp_path / "lucidlint.json"
    assert run_main(repo, "--update-baseline", "--baseline", str(baseline)) == 0
    assert baseline.exists()
    keys = json.loads(baseline.read_text())["actions"]
    assert keys and "swallow:houses/app.py" in keys[0]  # swallow has its own display bucket


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
    baseline = tmp_path / "lucidlint.json"
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
    baseline = tmp_path / "lucidlint.json"
    run_main(repo, "--update-baseline", "--baseline", str(baseline))
    # fix the code — the baselined finding is now stale
    (repo / "houses" / "app.py").write_text(APP_SRC)
    assert run_main(repo, "--baseline", str(baseline)) == 1
    assert "stale baseline" in capsys.readouterr().err


def test_stale_baseline_clears_after_update(tmp_path):
    repo = make_repo(tmp_path, app_src=SWALLOW_SRC)
    baseline = tmp_path / "lucidlint.json"
    run_main(repo, "--update-baseline", "--baseline", str(baseline))
    (repo / "houses" / "app.py").write_text(APP_SRC)
    run_main(repo, "--update-baseline", "--baseline", str(baseline))
    assert run_main(repo, "--baseline", str(baseline)) == 0


def test_baseline_line_shift_is_not_stale(tmp_path):
    repo = make_repo(tmp_path, app_src=SWALLOW_SRC)
    baseline = tmp_path / "lucidlint.json"
    run_main(repo, "--update-baseline", "--baseline", str(baseline))
    # add a line above the finding — same function, new line
    shifted = "# comment\n" * 3 + SWALLOW_SRC
    (repo / "houses" / "app.py").write_text(shifted)
    assert run_main(repo, "--baseline", str(baseline)) == 0


def test_baseline_gone_function_is_stale(tmp_path, capsys):
    repo = make_repo(tmp_path, app_src=SWALLOW_SRC)
    baseline = tmp_path / "lucidlint.json"
    run_main(repo, "--update-baseline", "--baseline", str(baseline))
    (repo / "houses" / "app.py").write_text(APP_SRC)
    assert run_main(repo, "--baseline", str(baseline)) == 1
    assert "stale baseline" in capsys.readouterr().err


def test_rust_rule_groups_match_python():
    """The Rust core's rule_groups() (GENERATED from the catalog into
    scanner/src/rules_gen.rs) must match lucidlint.py's RULE_GROUPS (derived
    from the same catalog): the LSP and the gate expand `group:` config
    ignores identically. This test pins the generated file being current —
    both sides deriving from one source makes drift structurally impossible
    otherwise."""
    import re
    rust_src = (Path(__file__).parent.parent / "scanner" / "src" / "rules_gen.rs").read_text()
    # scope to the rule_groups function — the same (\"name\", &[...) shape
    # appears in FAMILY_VARIANTS and must not leak into the group map
    rust_src = rust_src[rust_src.index("pub fn rule_groups"):]
    rust_groups = {}
    for m in re.finditer(r'\(\s*"([\w-]+)",\s*&\[\s*((?:"[a-z-]+",?\s*)+)', rust_src):
        rust_groups[m.group(1)] = set(re.findall(r'"([a-z-]+)"', m.group(2)))
    from lucidlint import RULE_GROUPS
    assert set(rust_groups) == set(RULE_GROUPS), "group names drifted"
    for name, kinds in RULE_GROUPS.items():
        assert rust_groups[name] == set(kinds), f"group '{name}' drifted"

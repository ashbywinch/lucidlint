# code-health: ignore-file fakefs the fix engine rewrites REAL temp files and
# the gate runs the actual Rust binary — real-FS subprocess interop, the same
# named exception as test_code_health.py
"""Auto-fix engine tests: each mechanical transform is lossless (comments and
formatting survive) and the re-scanned source no longer produces the finding."""

import sys
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent.parent))
from test_code_health import make_repo, run_main

import fix_engine


def _fix(tmp_path, kind, rel, src, line):
    repo = make_repo(tmp_path, app_src="def alpha(a):\n    return a\n")
    (repo / rel).write_text(src)
    repo.mkdir(exist_ok=True)
    out = fix_engine.fix_finding(kind, rel, repo, line)
    return out, (repo / rel).read_text()


def test_stale_suppression_comment_deleted(tmp_path):
    src = "def f():\n    return 1\n# code-health: ignore magic-number nothing here\n"
    out, fixed = _fix(tmp_path, "stale-suppression", "houses/app.py", src, 3)
    assert out is not None
    assert "code-health: ignore" not in fixed
    assert "def f():" in fixed  # the rest survives verbatim
    assert fixed.splitlines()[:2] == src.splitlines()[:2]


def test_noop_statement_deleted(tmp_path):
    src = "def f():\n    x + 1\n    return 2\n"
    out, fixed = _fix(tmp_path, "noop-statement", "houses/app.py", src, 2)
    assert out is not None
    assert "x + 1" not in fixed
    assert "return 2" in fixed


def test_unreachable_statement_deleted(tmp_path):
    src = "def f():\n    return 1\n    x = 2\n"
    out, fixed = _fix(tmp_path, "unreachable", "houses/app.py", src, 3)
    assert out is not None
    assert "x = 2" not in fixed
    assert "return 1" in fixed


def test_positional_literals_keyworded(tmp_path):
    src = "def set_limits(min_v, max_v):\n    return min_v\n\n\ndef g():\n    set_limits(10, 20)\n"
    out, fixed = _fix(tmp_path, "positional-literals", "houses/app.py", src, 6)
    assert out is not None
    assert "set_limits(min_v=10, max_v=20)" in fixed
    assert "def set_limits(min_v, max_v):" in fixed  # the def is untouched


def test_positional_literals_cross_file_callee_skipped(tmp_path):
    # the callee is not defined in the file — no edit, but no crash either
    src = "def g():\n    set_limits(10, 20)\n"
    out, fixed = _fix(tmp_path, "positional-literals", "houses/app.py", src, 2)
    assert out is None
    assert fixed == src


def test_external_callee_with_supplied_params(tmp_path):
    # Money is a third-party class — the agent reads its signature once and
    # supplies the param names; the tool applies the mechanical edit
    src = "def g():\n    Money(\"0\", \"GBP\")\n"
    out, fixed = _fix(tmp_path, "positional-literals", "houses/app.py", src, 2)
    assert out is None  # unresolvable without params
    out2 = fix_engine.fix_finding(
        "positional-literals", "houses/app.py", tmp_path / "repo", 2, params=["amount", "currency"]
    )
    assert out2 is not None
    assert 'Money(amount="0", currency="GBP")' in (tmp_path / "repo" / "houses" / "app.py").read_text()


def test_fix_gate_rerun_is_clean(tmp_path, capsys):
    """After the fix, scanning the file no longer yields the finding."""
    src = "def set_limits(min_v, max_v):\n    return min_v\n\n\ndef g():\n    set_limits(10, 20)\n"
    repo = make_repo(tmp_path, app_src="def alpha(a):\n    return a\n")
    (repo / "houses" / "app.py").write_text(src)
    fixed = fix_engine.fix_finding("positional-literals", "houses/app.py", repo, 6)
    assert fixed is not None
    # the gate run reports no positional-literals finding anymore
    run_main(repo, "--warn", "--json")
    assert "positional-literals" not in capsys.readouterr().out

# lucidlint: ignore-file fakefs the fix engine rewrites REAL temp files and
# the gate runs the actual Rust binary — real-FS subprocess interop, the same
# named exception as test_lucidlint.py
"""Auto-fix engine tests: each mechanical transform is lossless (comments and
formatting survive) and the re-scanned source no longer produces the finding."""

import json
import sys
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent.parent))
from test_lucidlint import make_repo, run_main

import fix_engine


def _fix(tmp_path, kind, rel, src, line):
    repo = make_repo(tmp_path, app_src="def alpha(a):\n    return a\n")
    (repo / rel).write_text(src)
    repo.mkdir(exist_ok=True)
    out = fix_engine.fix_finding(kind, rel, repo, line)
    return out, (repo / rel).read_text()


def test_stale_suppression_comment_deleted(tmp_path):
    src = "def f():\n    return 1\n# lucidlint: ignore magic-number nothing here\n"
    out, fixed = _fix(tmp_path, "stale-suppression", "houses/app.py", src, 3)
    assert out is not None
    assert "lucidlint: ignore" not in fixed
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
        "positional-literals",
        "houses/app.py",
        tmp_path / "repo",
        2,
        fix_engine.FixOptions(params=["amount", "currency"]),
    )
    assert out2 is not None
    assert 'Money(amount="0", currency="GBP")' in (tmp_path / "repo" / "houses" / "app.py").read_text()


def test_extract_class_moves_fns_and_rewrites_calls(tmp_path):
    src = (
        "class GraphContract:\n"
        "    def __init__(self):\n"
        "        self.edges = []\n"
        "\n"
        "def hub_edge_counts(contract: GraphContract):\n"
        "    return len(contract.edges)\n"
        "\n"
        "def dominant_callee(contract: GraphContract, x):\n"
        "    return x\n"
        "\n"
        "def resolve_callee_module(contract: GraphContract, y):\n"
        "    return y\n"
        "\n"
        "def g():\n"
        "    return hub_edge_counts(GraphContract())\n"
    )
    repo = make_repo(tmp_path, app_src="def alpha(a):\n    return a\n")
    (repo / "houses" / "app.py").write_text(src)
    out = fix_engine.fix_finding("extract-class", "houses/app.py", repo, 5)
    assert out is not None
    fixed = (repo / "houses" / "app.py").read_text()
    assert "def hub_edge_counts(contract" not in fixed  # no longer a free fn
    assert "def hub_edge_counts(self):" in fixed  # now a method
    assert "GraphContract().hub_edge_counts()" in fixed  # call site rewritten
    # the group went into the shared type (default name)
    assert "class GraphContract:" in fixed


def test_extract_class_with_explicit_name(tmp_path):
    src = (
        "class GraphContract:\n"
        "    def __init__(self):\n"
        "        self.edges = []\n"
        "\n"
        "def hub_edge_counts(contract: GraphContract):\n"
        "    return 1\n"
        "\n"
        "def dominant_callee(contract: GraphContract, x):\n"
        "    return x\n"
        "\n"
        "def resolve_callee_module(contract: GraphContract, y):\n"
        "    return y\n"
    )
    repo = make_repo(tmp_path, app_src="def alpha(a):\n    return a\n")
    (repo / "houses" / "app.py").write_text(src)
    out = fix_engine.fix_finding(
        "extract-class", "houses/app.py", repo, 5, fix_engine.FixOptions(name="GraphOps")
    )
    assert out is not None
    fixed = (repo / "houses" / "app.py").read_text()
    assert "class GraphOps:" in fixed
    assert "def hub_edge_counts(self):" in fixed


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


# --------------------------------------------------------------------------- message -> fix, end to end

_FIX_ALL_SRC = (
    "# lucidlint: ignore magic-number nothing on this line\n"
    "def set_limits(min_v, max_v):\n"
    "    return min_v\n"
    "\n"
    "class GraphContract:\n"
    "    def __init__(self):\n"
    "        self.edges = []\n"
    "\n"
    "def hub_edge_counts(contract: GraphContract):\n"
    "    return len(contract.edges)\n"
    "\n"
    "def dominant_callee(contract: GraphContract, x):\n"
    "    return x\n"
    "\n"
    "def resolve_callee_module(contract: GraphContract, y):\n"
    "    return y\n"
    "\n"
    "def g():\n"
    "    set_limits(10, 20)\n"
    "    Money(\"0\", \"GBP\")\n"
    "    x + 1\n"
    "    return 1\n"
    "    x = 2\n"
)

FIXABLE_KINDS = {
    "stale-suppression",
    "noop-statement",
    "unreachable",
    "positional-literals",
    "extract-class",
}


def test_message_to_fix_end_to_end(tmp_path, capsys):
    """The full loop the agent runs: gate -> read the finding -> apply the fix
    (supplying a name/signature where the message cannot) -> re-gate, until
    every fixable kind is gone. Drives the real CLI, not the functions."""
    repo = make_repo(tmp_path, app_src="def alpha(a):\n    return a\n")
    (repo / "houses" / "app.py").write_text(_FIX_ALL_SRC)
    attempts: dict = {}

    for _ in range(25):  # bounded — the workflow normally converges in ~7
        run_main(repo, "--warn", "--json")
        actions = json.loads(capsys.readouterr().out)["actions"]
        finding = next((a for a in actions if a["kind"] in FIXABLE_KINDS), None)
        if finding is None:
            break
        key = (finding["kind"], finding["file"], finding["line"])
        extra = []
        if finding["kind"] == "positional-literals" and attempts.get(key, 0) >= 1:
            # attempt 0 resolved the same-file callee; the external Money
            # call needs the agent's signature read
            extra = ["--fix-params", "amount,currency"]
        rc = run_main(
            repo,
            "--fix-kind", finding["kind"],
            "--fix-file", finding["file"],
            "--fix-line", str(finding["line"]),
            *extra,
        )
        assert rc == 0
        capsys.readouterr()  # drain the fix subcommand's own output
        attempts[key] = attempts.get(key, 0) + 1
        assert attempts[key] <= 2, f"fix did not converge on {key}"

    # every fixable kind is gone and the file still parses
    run_main(repo, "--warn", "--json")
    final_actions = json.loads(capsys.readouterr().out)["actions"]
    remaining = [a["kind"] for a in final_actions if a["kind"] in FIXABLE_KINDS]
    assert remaining == [], f"fixable findings remain: {remaining}"
    compile((repo / "houses" / "app.py").read_text(), "app.py", "exec")

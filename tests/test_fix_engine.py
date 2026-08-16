# lucidlint: ignore-file fakefs the fix engine rewrites REAL temp files and
# the gate runs the actual Rust binary — real-FS subprocess interop, the same
# named exception as test_lucidlint.py
"""Auto-fix engine tests: each mechanical transform is lossless (comments and
formatting survive) and the re-scanned source no longer produces the finding."""

import json
import re
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
        "    return len(contract.edges) + x\n"
        "\n"
        "def resolve_callee_module(contract: GraphContract, y):\n"
        "    return contract.edges\n"
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
        "    return len(contract.edges) + x\n"
        "\n"
        "def resolve_callee_module(contract: GraphContract, y):\n"
        "    return contract.edges\n"
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
    "    return len(contract.edges) + x\n"
    "\n"
    "def resolve_callee_module(contract: GraphContract, y):\n"
    "    return contract.edges\n"
    "\n"
    "class ConfigManager:\n"
    "    def load(self):\n"
    "        return 1\n"
    "\n"
    "def build(a, b, c, d, e, f):\n"
    "    return a + b + c + d + e + f\n"
    "\n"
    "def g():\n"
    "    set_limits(10, 20)\n"
    "    Money(\"0\", \"GBP\")\n"
    "    x + 1\n"
    "    return 1\n"
    "    x = 2\n"
)

# the gate reports display kinds; the fix command maps them to transforms
FIXABLE_KINDS = {
    "stale-suppression": "stale-suppression",
    "noop-statement": "noop-statement",
    "unreachable": "unreachable",
    "positional-literals": "positional-literals",
    "latent-class": "extract-class",  # strewing's display kind
    "magic-number": "magic-number",
    "vague-name": "vague-name",
    "long-param-list": "long-param-list",
}

# kinds whose fix needs the agent's semantic bit (supplied on the retry)
FIX_NAMES = {
    "magic-number": "MAX_RETRIES",
    "vague-name": "ConfigRegistry",
    "long-param-list": "BuildOptions",
}


def test_magic_literal_becomes_constant(tmp_path):
    src = "def g():\n    return 60 * 24\n"
    repo = make_repo(tmp_path, app_src="def alpha(a):\n    return a\n")
    (repo / "houses" / "app.py").write_text(src)
    out = fix_engine.fix_finding(
        "magic-number", "houses/app.py", repo, 2, fix_engine.FixOptions(name="MINUTES_PER_DAY")
    )
    assert out is not None
    fixed = (repo / "houses" / "app.py").read_text()
    assert "MINUTES_PER_DAY = 60" in fixed
    assert "return MINUTES_PER_DAY * 24" in fixed


def test_vague_name_rename(tmp_path):
    src = "class DataManager:\n    def run(self):\n        return 1\n\n\ndef use():\n    return DataManager()\n"
    repo = make_repo(tmp_path, app_src="def alpha(a):\n    return a\n")
    (repo / "houses" / "app.py").write_text(src)
    out = fix_engine.fix_finding(
        "vague-name", "houses/app.py", repo, 1, fix_engine.FixOptions(name="DataRegistry")
    )
    assert out is not None
    fixed = (repo / "houses" / "app.py").read_text()
    assert "class DataRegistry:" in fixed
    assert "return DataRegistry()" in fixed
    assert "DataManager" not in fixed


def test_parameter_object_introduced(tmp_path):
    src = (
        "def build(a, b, c, d, e, f):\n"
        "    return a + b + c + d + e + f\n"
        "\n"
        "def g():\n"
        "    return build(1, 2, 3, 4, 5, 6)\n"
    )
    repo = make_repo(tmp_path, app_src="def alpha(a):\n    return a\n")
    (repo / "houses" / "app.py").write_text(src)
    out = fix_engine.fix_finding(
        "long-param-list", "houses/app.py", repo, 1, fix_engine.FixOptions(name="BuildOptions")
    )
    assert out is not None
    fixed = (repo / "houses" / "app.py").read_text()
    assert "class BuildOptions:" in fixed
    assert "def build(options: BuildOptions):" in fixed
    assert "return options.a + options.b + options.c" in fixed
    assert "build(BuildOptions(a=1, b=2, c=3, d=4, e=5, f=6))" in fixed


def test_extract_class_renames_receiver_and_internal_calls(tmp_path):
    """The missed-class scenario that tripped a hand-fix: moved methods must
    rename the receiver to self AND rewrite inter-fn calls to self-calls —
    otherwise the extraction produces broken code."""
    src = (
        "class _FnBodyState:\n"
        "    def __init__(self, line):\n"
        "        self.line = line\n"
        "\n"
        "def _window_score(state: _FnBodyState, i, j, min_lines):\n"
        "    if state.line > 0:\n"
        "        return 1\n"
        "    return 0\n"
        "\n"
        "def _window_has_outvars(state: _FnBodyState, j, writes_all):\n"
        "    return state.line > 0\n"
        "\n"
        "def _best_seam(state: _FnBodyState, min_lines=2):\n"
        "    return _window_score(state, 0, 1, min_lines)\n"
    )
    repo = make_repo(tmp_path, app_src="def alpha(a):\n    return a\n")
    p = repo / "houses" / "app.py"
    p.write_text(src)
    out = fix_engine.fix_finding("extract-class", "houses/app.py", repo, 5)
    assert out is not None
    fixed = p.read_text()
    assert "def _window_score(self, i, j, min_lines):" in fixed  # receiver renamed
    assert "if self.line > 0:" in fixed  # body reference renamed
    assert "self._window_score(0, 1, min_lines)" in fixed  # inter-fn call rewritten
    ns = {}
    exec(compile(fixed, "app.py", "exec"), ns)
    assert ns["_FnBodyState"](1)._best_seam() == 1  # behavior preserved


def test_extract_method_preview_and_confirm(tmp_path):
    src = (
        "def process(data, factor):\n"
        "    results = []\n"
        "    for item in data:\n"
        "        results.append(item * factor)\n"
        "    return sum(results)\n"
    )
    repo = make_repo(tmp_path, app_src="def alpha(a):\n    return a\n")
    p = repo / "houses" / "app.py"
    p.write_text(src)
    # preview needs NO name — the seam shows with a placeholder, the agent
    # names AFTER seeing it (the S3 flow)
    new_source, desc = fix_engine.propose_finding(
        "extract-method", "houses/app.py", repo, 1, fix_engine.FixOptions()
    )
    assert desc is not None
    assert "_extracted(" in new_source  # the placeholder in the diff
    assert p.read_text() == src  # nothing written
    # apply with a name — normalized to private: scale_total -> _scale_total
    out = fix_engine.fix_finding(
        "extract-method", "houses/app.py", repo, 1, fix_engine.FixOptions(name="scale_total")
    )
    assert out is not None
    fixed = p.read_text()
    assert "def _scale_total(" in fixed
    assert fixed.count("_scale_total(") >= 2  # the def AND the call site
    # the extraction is valid Python and preserves behavior
    ns = {}
    exec(compile(fixed, "app.py", "exec"), ns)
    assert ns["process"]([1, 2, 3], 2) == 12


def test_extract_method_descends_into_loop_body(tmp_path):
    """Nested descent: a loop body too complex to move whole (>13 decisions)
    yields an inner chunk as the seam — the call lands INSIDE the loop, the
    loop survives, and behavior is preserved. (The old flat analysis could
    only move whole statements.)"""
    src = (
        "def grade_all(rows, mode):\n"
        "    out = []\n"
        "    for row in rows:\n"
        "        s = row['score']\n"
        "        if s >= 95:\n"
        "            out.append('A+')\n"
        "        elif s >= 90:\n"
        "            out.append('A')\n"
        "        elif s >= 85:\n"
        "            out.append('A-')\n"
        "        elif s >= 80:\n"
        "            out.append('B+')\n"
        "        elif s >= 75:\n"
        "            out.append('B')\n"
        "        elif s >= 70:\n"
        "            out.append('B-')\n"
        "        elif s >= 65:\n"
        "            out.append('C+')\n"
        "        elif s >= 60:\n"
        "            out.append('C')\n"
        "        elif s >= 55:\n"
        "            out.append('C-')\n"
        "        elif s >= 50:\n"
        "            out.append('D+')\n"
        "        elif s >= 45:\n"
        "            out.append('D')\n"
        "        elif s >= 40:\n"
        "            out.append('D-')\n"
        "        else:\n"
        "            out.append('F')\n"
        "        if mode == 'debug':\n"
        "            out.append(row['name'])\n"
        "    return out\n"
    )
    repo = make_repo(tmp_path, app_src="def alpha(a):\n    return a\n")
    p = repo / "houses" / "app.py"
    p.write_text(src)
    out = fix_engine.fix_finding(
        "extract-method", "houses/app.py", repo, 1, fix_engine.FixOptions(name="grade_row")
    )
    assert out is not None
    fixed = p.read_text()
    assert "for row in rows:" in fixed  # the loop survives
    # the call sits INSIDE the loop (4-space indent), not at fn level
    call_lines = [ln for ln in fixed.splitlines() if "_grade_row(" in ln and not ln.lstrip().startswith("def ")]
    assert call_lines, "no call site found"
    assert call_lines[0].startswith("        ") or call_lines[0].startswith("    ")
    # behavior preserved
    ns = {}
    exec(compile(fixed, "app.py", "exec"), ns)
    rows = [{"score": 97, "name": "x"}, {"score": 50, "name": "y"}]
    assert ns["grade_all"](rows, "debug") == ["A+", "x", "D+", "y"]
    assert ns["grade_all"](rows, "quiet") == ["A+", "D+"]


def test_extract_method_no_safe_seam(tmp_path):
    # every block feeds the rest of the function — no out-var-free seam
    src = (
        "def f(a, b):\n"
        "    x = a + b\n"
        "    y = x * 2\n"
        "    return y\n"
    )
    repo = make_repo(tmp_path, app_src="def alpha(a):\n    return a\n")
    p = repo / "houses" / "app.py"
    p.write_text(src)
    new_source, desc = fix_engine.propose_finding(
        "extract-method", "houses/app.py", repo, 1, fix_engine.FixOptions(name="calc")
    )
    assert desc is None  # no self-contained seam — refuse rather than break


def test_extract_method_cli_preview_confirm_flow(tmp_path, capsys):
    """The S3 CLI protocol: NO name previews (the seam with a placeholder
    name, nothing written); a name applies DIRECTLY — the name IS the
    commitment, no --confirm dance. Agents are bad at line numbers — the
    preview IS the line-number-free contract."""
    src = (
        "def process(data, factor):\n"
        "    results = []\n"
        "    for item in data:\n"
        "        results.append(item * factor)\n"
        "    return sum(results)\n"
    )
    repo = make_repo(tmp_path, app_src="def alpha(a):\n    return a\n")
    p = repo / "houses" / "app.py"
    p.write_text(src)
    # preview WITHOUT a name: prints a diff, writes NOTHING
    rc = run_main(
        repo, "--fix-kind", "extract-method", "--fix-file", "houses/app.py",
        "--fix-line", "1",
    )
    assert rc == 0
    out = capsys.readouterr().out
    assert "+++" in out  # the call-site hunk was shown
    assert "def _extracted(" in out  # the placeholder method head
    assert "Extracted (the method being created" in out
    assert "placeholder" in out  # the agent knows _extracted is not final
    assert p.read_text() == src  # nothing written yet
    # WITH a name: applies directly (no --confirm — the name is the commit)
    rc = run_main(
        repo, "--fix-kind", "extract-method", "--fix-file", "houses/app.py",
        "--fix-line", "1", "--fix-name", "accumulate",
    )
    assert rc == 0
    capsys.readouterr()
    fixed = p.read_text()
    assert "def _accumulate(" in fixed
    assert fixed.count("_accumulate(") >= 2  # the def AND the call site
    assert " accumulate(" not in fixed  # every reference is private
    ns = {}
    exec(compile(fixed, "app.py", "exec"), ns)
    assert ns["process"]([1, 2, 3], 2) == 12  # behavior preserved
    # the gate still passes (no fail findings introduced)
    run_main(repo, "--warn", "--json")
    data = json.loads(capsys.readouterr().out)
    assert not [a for a in data["actions"] if a["severity"] == "fail"]


def test_strewing_message_tees_up_the_fix(tmp_path, capsys):
    """The output gap that caused a manual missed-class hand-fix: the finding
    must say the fix exists, with the exact command (R27)."""
    src = (
        "class _FnBodyState:\n"
        "    def __init__(self, line):\n"
        "        self.line = line\n"
        "\n"
        "def _window_score(state: _FnBodyState, i, j, min_lines):\n"
        "    if state.line > 0:\n"
        "        return 1\n"
        "    return 0\n"
        "\n"
        "def _window_has_outvars(state: _FnBodyState, j, writes_all):\n"
        "    return state.line > 0\n"
        "\n"
        "def _best_seam(state: _FnBodyState, min_lines=2):\n"
        "    return _window_score(state, 0, 1, min_lines)\n"
    )
    repo = make_repo(tmp_path, app_src="def alpha(a):\n    return a\n")
    (repo / "houses" / "app.py").write_text(src)
    run_main(repo, "--warn", "--json")
    data = json.loads(capsys.readouterr().out)
    strewing = [a for a in data["actions"] if a["kind"] == "latent-class"]
    assert strewing, "expected a strewing finding"
    assert "fix: extract-class" in strewing[0]["message"], strewing[0]["message"]


def test_fix_without_line_resolves_single_finding(tmp_path, capsys):
    """R27: no --fix-line needed when the file has exactly one finding of the
    kind — the tool scans and owns the coordinates."""
    src = "def g():\n    x + 1\n    return 2\n"
    repo = make_repo(tmp_path, app_src="def alpha(a):\n    return a\n")
    (repo / "houses" / "app.py").write_text(src)
    # no --fix-line: the noop is the only finding of its kind in the file
    rc = run_main(repo, "--fix-kind", "noop-statement", "--fix-file", "houses/app.py")
    assert rc == 0
    out = capsys.readouterr().out
    assert "fix:" in out
    assert "x + 1" not in (repo / "houses" / "app.py").read_text()


def test_extract_class_previews_without_writing(tmp_path, capsys):
    """Structural fixes preview a diff and write NOTHING without --confirm —
    the reviewable-application contract for every structural kind."""
    src = (
        "class S:\n"
        "    def __init__(self):\n"
        "        self.x = 0\n"
        "\n"
        "def a(s: S):\n"
        "    return s.x\n"
        "\n"
        "def b(s: S):\n"
        "    return s.x + 1\n"
        "\n"
        "def c(s: S):\n"
        "    return s.x + 2\n"
    )
    repo = make_repo(tmp_path, app_src="def alpha(a):\n    return a\n")
    p = repo / "houses" / "app.py"
    p.write_text(src)
    rc = run_main(repo, "--fix-kind", "latent-class", "--fix-file", "houses/app.py", "--fix-line", "5")
    assert rc == 0
    out = capsys.readouterr().out
    assert "+++" in out  # the preview diff
    assert p.read_text() == src  # nothing written
    rc = run_main(repo, "--fix-kind", "latent-class", "--fix-file", "houses/app.py", "--fix-line", "5", "--confirm")
    assert rc == 0
    capsys.readouterr()
    assert "def a(self):" in p.read_text()


# --------------------------------------------------------------------------- R27: follow the message's fix directive

_INDEPENDENT_IFS = "".join(
    f"    if c{i} > 0:\n        out.append({1 << i})\n" for i in range(14)
)

# (finding kind, fixture, before-expr, after-expr, expected, --fix-name)
DIRECTIVE_CASES = [
    (
        "stale-suppression",
        "# lucidlint: ignore magic-number nothing on this line\ndef g():\n    return 1\n",
        "g()", "g()", "1", None,
    ),
    (
        "noop-statement",
        "def g():\n    x = 1\n    x + 1\n    return 2\n",
        "g()", "g()", "2", None,
    ),
    (
        "unreachable",
        "def g():\n    return 1\n    x = 2\n",
        "g()", "g()", "1", None,
    ),
    (
        "positional-literals",
        "def set_limits(min_v, max_v):\n    return min_v\n\ndef g():\n    return set_limits(10, 20)\n",
        "g()", "g()", "10", None,
    ),
    (
        "magic-number",
        "def g():\n    return 60 * 2\n",
        "g()", "g()", "120", "MAX_RETRIES",
    ),
    (
        "vague-name",
        (
            "class DataManager:\n    def run(self):\n        return 1\n"
            "    def stop(self):\n        return 2\n"
            "    def reset(self):\n        return 3\n"
            "    def start(self):\n        return 4\n"
            "    def pause(self):\n        return 5\n"
            "    def resume(self):\n        return 6\n"
            "\ndef use():\n    return DataManager()\n"
        ),
        "use().run()", "use().run()", "1", "DataRegistry",
    ),
    (
        "long-param-list",
        (
            "def build(a, b, c, d, e, f):\n"
            "    return a + b + c + d + e + f\n"
            "\ndef g():\n    return build(1, 2, 3, 4, 5, 6)\n"
        ),
        "g()", "g()", "21", "BuildOptions",
    ),
    (
        "latent-class",
        (
            "class _FnBodyState:\n    def __init__(self, line):\n        self.line = line\n\n"
            "def _window_score(state: _FnBodyState, i, j, min_lines):\n"
            "    if state.line > 0:\n        return 1\n    return 0\n\n"
            "def _window_has_outvars(state: _FnBodyState, j, writes_all):\n"
            "    return state.line > 0\n\n"
            "def _best_seam(state: _FnBodyState, min_lines=2):\n"
            "    return _window_score(state, 0, 1, min_lines)\n"
        ),
        "_best_seam(_FnBodyState(1))", "_FnBodyState(1)._best_seam()", "1", None,
    ),
    (
        "complexity",
        (
            "def score(" + ", ".join(f"c{i}" for i in range(14)) + "):\n"
            "    out = []\n"
            + _INDEPENDENT_IFS
            + "    label = 's' + str(sum(out))\n    return label\n"
        ),
        "score(1, 0, 1, 0, 1, 0, 1, 0, 1, 0, 1, 0, 1, 0)",
        "score(1, 0, 1, 0, 1, 0, 1, 0, 1, 0, 1, 0, 1, 0)",
        "s5461", "sum_bonus",
    ),
]


def test_every_fix_directive_clears_its_finding(tmp_path, capsys):
    """R27 compliance: for every fixable family, the finding's message carries
    a `fix: <command>` directive, and following it EXACTLY (substituting the
    name, adding --confirm where the result is novel) clears the finding and
    preserves behavior."""
    for kind, src, before_expr, after_expr, expected, name_value in DIRECTIVE_CASES:
        tmp = tmp_path / kind
        repo = make_repo(tmp, app_src="def alpha(a):\n    return a\n")
        p = repo / "houses" / "app.py"
        p.write_text(src)
        before = {}
        exec(compile(src, "before.py", "exec"), before)
        assert str(eval(before_expr, before)) == expected, f"{kind}: before-behavior"

        run_main(repo, "--warn", "--json")
        data = json.loads(capsys.readouterr().out)
        finding = next((a for a in data["actions"] if a["kind"] == kind), None)
        assert finding is not None, f"{kind}: no finding"
        directive = re.search(r"fix: ([a-z-]+)(?: --fix-name <([^>]+)>)?", finding["message"])
        assert directive, f"{kind}: message has no fix directive: {finding['message']}"
        fix_kind = directive.group(1)

        args = [
            "--fix-kind", fix_kind,
            "--fix-file", finding["file"],
            "--fix-line", str(finding["line"]),
        ]
        # name-driven kinds get the table's name whether the message still
        # carries the slot or not (the extract-method directive no longer
        # demands a name — the preview supplies it; the suite knows it)
        if name_value:
            args += ["--fix-name", name_value]
        if fix_engine.KIND_ALIASES.get(fix_kind, fix_kind) in fix_engine.PREVIEW_KINDS:
            args.append("--confirm")
        rc = run_main(repo, *args)
        assert rc == 0, f"{kind}: fix command failed"
        capsys.readouterr()

        run_main(repo, "--warn", "--json")
        after = json.loads(capsys.readouterr().out)
        assert not [a for a in after["actions"] if a["kind"] == kind], (
            f"{kind}: finding remains after following the directive"
        )

        after_ns = {}
        exec(compile(p.read_text(), "after.py", "exec"), after_ns)
        assert str(eval(after_expr, after_ns)) == expected, f"{kind}: behavior changed"


def test_message_to_fix_end_to_end(tmp_path, capsys):
    """The full loop the agent runs: gate -> read the finding -> apply the fix
    (supplying a name/signature where the message cannot) -> re-gate, until
    every fixable kind is gone. Drives the real CLI, not the functions."""
    repo = make_repo(tmp_path, app_src="def alpha(a):\n    return a\n")
    (repo / "houses" / "app.py").write_text(_FIX_ALL_SRC)
    attempts: dict = {}

    last_count: int | None = None  # fixable count before the previous fix
    prev_applied = False
    for _ in range(25):  # bounded — the workflow normally converges in ~7
        run_main(repo, "--warn", "--json")
        actions = json.loads(capsys.readouterr().out)["actions"]
        fixable = [a for a in actions if a["kind"] in FIXABLE_KINDS]
        finding = next(iter(fixable), None)
        if finding is None:
            break
        # monotone convergence: an applied fix must strictly reduce the count
        # from the count that existed before it; a no-op (the params/name
        # retry protocol) leaves it unchanged
        if last_count is not None:
            if prev_applied:
                assert len(fixable) < last_count, (
                    f"applied fix did not reduce findings ({last_count} -> "
                    f"{len(fixable)}) — the loop would thrash; fix the transform"
                )
            else:
                assert len(fixable) <= last_count, (
                    f"no-op fix changed the file ({last_count} -> {len(fixable)})"
                )
        fix_kind = FIXABLE_KINDS[finding["kind"]]
        key = (finding["kind"], finding["file"], finding["line"])
        extra = []
        if fix_kind == "positional-literals" and attempts.get(key, 0) >= 1:
            # attempt 0 resolved the same-file callee; the external Money
            # call needs the agent's signature read
            extra = ["--fix-params", "amount,currency"]
        if fix_kind in FIX_NAMES and attempts.get(key, 0) >= 1:
            # name-driven fixes need the agent's name on the retry
            extra = ["--fix-name", FIX_NAMES[fix_kind]]
        rc = run_main(
            repo,
            "--fix-kind", fix_kind,
            "--fix-file", finding["file"],
            "--fix-line", str(finding["line"]),
            "--confirm",  # structural fixes preview unless confirmed
            *extra,
        )
        assert rc == 0
        fix_out = capsys.readouterr().out  # "fix: ..." or "fix: nothing to change"
        attempts[key] = attempts.get(key, 0) + 1
        assert attempts[key] <= 2, f"fix did not converge on {key}"
        prev_applied = "nothing to change" not in fix_out
        last_count = len(fixable)

    # every fixable kind is gone and the file still parses
    run_main(repo, "--warn", "--json")
    final_actions = json.loads(capsys.readouterr().out)["actions"]
    remaining = [a["kind"] for a in final_actions if a["kind"] in FIXABLE_KINDS]
    assert remaining == [], f"fixable findings remain: {remaining}"
    compile((repo / "houses" / "app.py").read_text(), "app.py", "exec")

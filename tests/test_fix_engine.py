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


def test_tuple_record_becomes_a_class(tmp_path):
    # the record direction: a CLASS, not a NamedTuple — the build sites
    # construct it, the positional reads and destructures become attribute
    # reads, and the class is prepended (the fixer's end-to-end shape)
    src = (
        "em = {r[\"id\"]: (p, n) for r in epics}\n"
        "def render(em):\n"
        "    for cid, (p, nm) in em.items():\n"
        "        if em[cid][0]:\n"
        "            print(nm)\n"
        "    a, b = em[cid]\n"
        "    return a, b\n"
    )
    repo = make_repo(tmp_path, app_src="def alpha(a):\n    return a\n")
    p = repo / "houses" / "app.py"
    p.write_text(src)
    _req("tuple-record", "houses/app.py", repo, 1, fix_engine.FixOptions(name="Page"), source=src).fix_finding()
    out = p.read_text()
    assert "class _Page:" in out
    assert "def __init__(self, p, n):" in out
    assert "_Page(p, n)" in out  # the build site constructs the class
    assert "em[cid].p" in out  # the constant-index read becomes an attribute
    assert "a, b = (em[cid].p, em[cid].n)" in out


def test_undeclared_attribute_declares_the_member(tmp_path):
    src = (
        "class C:\n"
        "    def __init__(self, x: int, repo):\n"
        "        self.x = x\n"
        "        self.count = 0\n"
        "        self.names = []\n"
    )
    repo = make_repo(tmp_path, app_src="def alpha(a):\n    return a\n")
    (repo / "houses" / "app.py").write_text(src)
    _req(
        "undeclared-attribute", "houses/app.py", repo, 3, fix_engine.FixOptions(), source=src
    ).fix_finding()
    assert "self.x: int = x" in (repo / "houses" / "app.py").read_text()
    _req(
        "undeclared-attribute", "houses/app.py", repo, 5, fix_engine.FixOptions(), source=src
    ).fix_finding()
    out = (repo / "houses" / "app.py").read_text()
    assert "self.names: list = []" in out
    # a member assigned OUTSIDE __init__ is not auto-annotated (the fix
    # would have to invent a default) — nothing to change


def _req(kind, rel, repo, line, opts=None, source=None):
    """A fix request in the test's old (kind, rel, repo, line, opts) shape."""
    return fix_engine._FixRequest(
        kind=kind, repo=repo, rel=rel, line=line, opts=opts or fix_engine.FixOptions(), source=source
    )


def _fix_finding(kind, rel, repo, line, opts=None):
    return _req(kind, rel, repo, line, opts).fix_finding()


def _propose_finding(kind, rel, repo, line, opts=None):
    return _req(kind, rel, repo, line, opts).propose_finding()


def _dispatch(source, line, opts=None):
    return _req("dispatch-registry", None, None, line, opts, source=source).fix_dispatch_registry()


def _rule_table(source, line, opts=None):
    return _req("rule-table", None, None, line, opts, source=source).fix_rule_table()


def _rust_run_compare(tmp_path, before: str, after: str, main_body: str) -> None:
    """Compile AND RUN both versions with the same `main`, asserting equal
    stdout — the behavior-preservation contract for Rust fixes (a compile-
    only check cannot catch arm reordering, dropped branches, or a wrong
    fallback)."""
    import subprocess
    rustc = str(Path.home() / ".cargo" / "bin" / "rustc")
    if not Path(rustc).is_file():
        rustc = "rustc"

    def run(src: str) -> str:
        f = tmp_path / "prog.rs"
        f.write_text(src + "\n" + main_body)
        exe = tmp_path / "prog"
        r = subprocess.run([rustc, str(f), "-o", str(exe)], capture_output=True, text=True)
        assert r.returncode == 0, f"compile failed:\n{r.stderr}\n{src}"
        r2 = subprocess.run([str(exe)], capture_output=True, text=True)
        assert r2.returncode == 0, f"run failed:\n{r2.stderr}"
        return r2.stdout

    assert run(before) == run(after), "the fix changed observable behavior"


def _fix(tmp_path, kind, rel, src, line):
    repo = make_repo(tmp_path, app_src="def alpha(a):\n    return a\n")
    (repo / rel).write_text(src)
    repo.mkdir(exist_ok=True)
    out = _fix_finding(kind, rel, repo, line)
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
    out2 = _fix_finding(
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
    out = _fix_finding("extract-class", "houses/app.py", repo, 5)
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
    out = _fix_finding(
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
    fixed = _fix_finding("positional-literals", "houses/app.py", repo, 6)
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
    out = _fix_finding(
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
    out = _fix_finding(
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
    out = _fix_finding(
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
    out = _fix_finding("extract-class", "houses/app.py", repo, 5)
    assert out is not None
    fixed = p.read_text()
    assert "def _window_score(self, i, j, min_lines):" in fixed  # receiver renamed
    assert "if self.line > 0:" in fixed  # body reference renamed
    assert "self._window_score(0, 1, min_lines)" in fixed  # inter-fn call rewritten
    ns = {}
    exec(compile(fixed, "app.py", "exec"), ns)
    assert ns["_FnBodyState"](1)._best_seam() == 1  # behavior preserved


def test_extract_class_renames_after_nested_function(tmp_path):
    """A nested def inside a moved fn must not clobber the receiver-rename
    context: everything AFTER the nested def is still inside the strewing
    fn, so its receiver references must be renamed to self. The regression:
    leave_FunctionDef reset the visitor's current fn to None, leaving the
    post-nested-def receiver references unrenamed and the moved method
    referencing a param that no longer exists."""
    src = (
        "class _FnBodyState:\n"
        "    def __init__(self, line):\n"
        "        self.line = line\n"
        "\n"
        "def _score(state: _FnBodyState, i, j):\n"
        "    def helper(v):\n"
        "        return v * 2\n"
        "    if state.line > 0:\n"
        "        return helper(state.line) + i\n"
        "    return 0\n"
        "\n"
        "def _other(state: _FnBodyState, n):\n"
        "    return state.line + n\n"
        "\n"
        "def _run(state: _FnBodyState):\n"
        "    return _score(state, 1, 2)\n"
    )
    repo = make_repo(tmp_path, app_src="def alpha(a):\n    return a\n")
    p = repo / "houses" / "app.py"
    p.write_text(src)
    out = _fix_finding("extract-class", "houses/app.py", repo, 5)
    assert out is not None
    fixed = p.read_text()
    assert "def _score(self, i, j):" in fixed
    assert "self.line" in fixed  # the post-nested-def reference renamed
    assert "def _other(self, n):" in fixed and "self.line" in fixed
    ns = {}
    exec(compile(fixed, "app.py", "exec"), ns)
    assert ns["_FnBodyState"](3)._run() == 7  # helper(3) + 1 == 7, behavior preserved


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
    new_source, desc = _propose_finding(
        "extract-method", "houses/app.py", repo, 1, fix_engine.FixOptions()
    )
    assert desc is not None
    assert "_extracted(" in new_source  # the placeholder in the diff
    assert p.read_text() == src  # nothing written
    # apply with a name — normalized to private: scale_total -> _scale_total
    out = _fix_finding(
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
    out = _fix_finding(
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


def test_extract_method_keyword_args_are_not_phantom_params(tmp_path):
    """The 2026-08-16 self-fix regression: a keyword-argument name
    (`with_changes(leading_lines=[])` — the `leading_lines`) is not a
    variable reference, but the seam analysis counted it as a free var,
    so the proposal carried a phantom parameter and the rewritten call
    would have raised NameError at runtime."""
    src = (
        "def wrap(stmts):\n"
        "    body_stmts = stmts\n"
        "    if body_stmts:\n"
        "        body_stmts[0] = body_stmts[0].with_changes(leading_lines=[])\n"
        "    return body_stmts\n"
    )
    repo = make_repo(tmp_path, app_src="def alpha(a):\n    return a\n")
    p = repo / "houses" / "app.py"
    p.write_text(src)
    out = _fix_finding(
        "extract-method", "houses/app.py", repo, 1, fix_engine.FixOptions(name="reset_leading")
    )
    assert out is not None
    fixed = p.read_text()
    assert "def _reset_leading(body_stmts):" in fixed
    # the call takes ONLY the real free var — no phantom leading_lines param
    assert "_reset_leading(body_stmts)" in fixed
    assert "leading_lines)" not in fixed


def test_extract_method_refuses_nested_function_target(tmp_path):
    """A complexity finding on a nested function cannot host the extracted
    helper (the insert lands at module level — the call would NameError).
    Refuse rather than write a broken file."""
    src = (
        "def outer():\n"
        "    def inner():\n"
        "        if a:\n"
        "            x = 1\n"
        "        if b:\n"
        "            x = 2\n"
        "        return x\n"
        "    return inner()\n"
    )
    repo = make_repo(tmp_path, app_src="def alpha(a):\n    return a\n")
    p = repo / "houses" / "app.py"
    p.write_text(src)
    new_source, desc = _propose_finding(
        "extract-method", "houses/app.py", repo, 2, fix_engine.FixOptions(name="_x")
    )
    assert desc is None  # no safe seam — refuse, do not corrupt


def test_extract_method_min_bound_refuses_insufficient_splits(tmp_path):
    """CC-splitting: when the function's only seams take too few decisions to
    land the ORIGINAL under CC 15 (the extracted side is bounded separately),
    the tool refuses — a 1-decision extraction of a 16-CC function leaves 15
    and the fix loop would thrash."""
    src = "def f(" + ", ".join(f"c{i}" for i in range(16)) + "):\n" + "".join(
        f"    if c{i}:\n        x{i} = {i}\n" for i in range(16)
    ) + "    return " + " + ".join(f"x{i}" for i in range(16)) + "\n"
    repo = make_repo(tmp_path, app_src="def alpha(a):\n    return a\n")
    p = repo / "houses" / "app.py"
    p.write_text(src)
    new_source, desc = _propose_finding(
        "extract-method", "houses/app.py", repo, 1, fix_engine.FixOptions(name="_x")
    )
    assert desc is None  # every same-container window is a single if — no split


def test_decision_count_matches_radon_for_nested_and_or():
    """radon counts len(BoolOp values) - 1. A parenthesized nested chain
    `(a and b) and (c and d)` is ONE radon BoolOp with 4 values -> 3
    decisions. The regression: a shared single _in_boolop flag double-
    counted nested nodes (4 instead of 3), skewing the extract-method
    decision bounds (<=13 seam gate, CC-split min)."""
    import libcst as cst

    import fix_engine as fe

    # three operands -> 2 decisions; a parenthesized 4-operand chain -> 3
    for src, expected in [
        ("def f(a, b, c):\n    return a and b and c\n", 2),
        ("def f(a, b, c, d):\n    return (a and b) and (c and d)\n", 3),
        ("def f(a, b, c, d):\n    return a and (b and (c and d))\n", 3),
    ]:
        stmt = cst.parse_module(src).body[0].body.body[0]
        n = fe._stmt_decision_count(stmt)
        assert n == expected, (src, n)



def test_rust_extract_method_applies(tmp_path):
    """extract-method works on Rust via the scan core (syn) — a seam whose
    free variables are fn params (types known) and that has no out-vars
    moves into a private helper. The Python/libcst engine cannot touch a .rs
    target; the fix routes through the Rust binary."""
    src = (
        "pub fn enrich(items: &mut Vec<i32>, factor: i32) -> () {\n"
        "    if items.is_empty() {\n"
        "        return;\n"
        "    }\n"
        "    for it in items.iter_mut() {\n"
        "        *it *= factor;\n"
        "    }\n"
        "}\n"
    )
    repo = make_repo(tmp_path, app_src="def alpha(a):\n    return a\n")
    p = repo / "houses" / "lib.rs"
    p.write_text(src)
    before = p.read_text()
    run_main(
        repo, "fix", "--kind", "extract-method", "--file", "houses/lib.rs",
        "--line", "1", "--name", "_apply_enrich",
    )
    fixed = p.read_text()
    assert "fn _apply_enrich(items: &mut Vec<i32>, factor: i32)" in fixed
    assert "_apply_enrich(items, factor);" in fixed
    assert "fn enrich(" in fixed  # the original survives, now delegating
    # behavior preserved: compile AND run both versions, compare the output
    main = (
        "fn main() {\n"
        "    let mut v = vec![1, 2, 3, 4];\n"
        "    enrich(&mut v, 3);\n"
        "    println!(\"out: {}\", v.iter().map(|x| x.to_string()).collect::<Vec<_>>().join(\",\"));\n"
        "}\n"
    )
    _rust_run_compare(tmp_path, before, fixed, main)


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
        repo, "fix", "--kind", "extract-method", "--file", "houses/app.py",
        "--line", "1",
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
        repo, "fix", "--kind", "extract-method", "--file", "houses/app.py",
        "--line", "1", "--name", "accumulate",
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
    assert "fix: lucidlint fix --kind extract-class" in strewing[0]["message"], strewing[0]["message"]


def test_fix_without_line_resolves_single_finding(tmp_path, capsys):
    """R27: no --fix-line needed when the file has exactly one finding of the
    kind — the tool scans and owns the coordinates."""
    src = "def g():\n    x + 1\n    return 2\n"
    repo = make_repo(tmp_path, app_src="def alpha(a):\n    return a\n")
    (repo / "houses" / "app.py").write_text(src)
    # no --fix-line: the noop is the only finding of its kind in the file
    rc = run_main(repo, "fix", "--kind", "noop-statement", "--file", "houses/app.py")
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
    rc = run_main(repo, "fix", "--kind", "latent-class", "--file", "houses/app.py", "--line", "5")
    assert rc == 0
    out = capsys.readouterr().out
    assert "+++" in out  # the preview diff
    assert p.read_text() == src  # nothing written
    rc = run_main(repo, "fix", "--kind", "latent-class", "--file", "houses/app.py", "--line", "5", "--confirm")
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
        # the directive is the FULL command now — the message carries its own
        # file/line (R27: the tool owns the coordinates), so executing it
        # proves the message alone tells the agent what to run
        directive = re.search(
            r"fix: lucidlint fix --kind ([a-z-]+) --file (\S+) --line (\d+)(?: --name <([^>]+)>)?",
            finding["message"],
        )
        assert directive, f"{kind}: message has no full fix command: {finding['message']}"
        fix_kind, fix_file, fix_line = directive.group(1), directive.group(2), int(directive.group(3))
        assert fix_file == finding["file"], f"{kind}: directive file {fix_file} != finding {finding['file']}"
        assert fix_line == finding["line"], f"{kind}: directive line {fix_line} != finding {finding['line']}"

        args = [
            "fix",
            "--kind", fix_kind,
            "--file", fix_file,
            "--line", str(fix_line),
        ]
        # name-driven kinds get the table's name whether the message still
        # carries the slot or not (the extract-method directive no longer
        # demands a name — the preview supplies it; the suite knows it)
        if name_value:
            args += ["--name", name_value]
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
            extra = ["--params", "amount,currency"]
        if fix_kind in FIX_NAMES and attempts.get(key, 0) >= 1:
            # name-driven fixes need the agent's name on the retry
            extra = ["--name", FIX_NAMES[fix_kind]]
        rc = run_main(
            repo,
            "fix",
            "--kind", fix_kind,
            "--file", finding["file"],
            "--line", str(finding["line"]),
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


def test_extract_method_refusal_is_silent(tmp_path, capsys):
    """R28: a fix that cannot apply refuses with NO explanation — the tool
    never tells the user it could not figure out a fix (the silence is the
    signal; the user sees the code and figures it out)."""
    repo = make_repo(tmp_path)
    (repo / "houses" / "app.py").write_text(
        "def price(t):\n    x = 3 * t\n    return x\n"
    )
    rc = run_main(repo, "fix", "--kind", "extract-method",
                  "--file", "houses/app.py", "--line", "1",
                  "--name", "_apply", "--confirm")
    assert rc == 0
    out = capsys.readouterr().out
    assert "nothing to change" in out
    assert "out-variable" not in out and "extractable" not in out, out
    assert "no " not in out.replace("nothing to change", ""), out

def test_dispatch_registry_preserves_behavior(tmp_path):
    """The dispatch-registry fix: an if/elif chain over a selector becomes a
    dict of selector -> handler functions. Behavior must be identical for
    every selector, including the no-match fallback."""
    src = '''def run_tool(tool, args, facts):
    people = facts["people"]
    if tool == "search_people":
        q = str(args.get("query", "")).strip().lower()
        return {"hits": [p for p in people if q in p.lower()]}
    if tool == "person":
        return {"id": str(args.get("id", ""))}
    if tool == "relationships":
        return {"rels": [r for r in facts["rels"] if r.get("a") == args.get("id")]}
    return {"error": "unknown tool"}
'''
    fixed = _dispatch(src, 1)
    assert fixed is not None and fixed != src
    facts = {"people": ["Alice Smith", "Bob"], "rels": [{"a": "1"}]}
    before, after = {}, {}
    exec(src, before)
    exec(fixed, after)
    for tool, args in [
        ("search_people", {"query": "ali"}),
        ("search_people", {}),
        ("person", {"id": "1"}),
        ("relationships", {"id": "9"}),
        ("bogus", {}),
    ]:
        b = before["run_tool"](tool, args, facts)
        a = after["run_tool"](tool, args, facts)
        assert b == a, (tool, b, a)
    assert "_REGISTRY" in fixed and "_search_people" in fixed
    assert "_route_" not in fixed  # the literal names the handler, no prefix
    # the registry dispatch is a lookup, not a chain
    assert fixed.count("if tool ==") == 0


def test_dispatch_registry_refuses_non_chain(tmp_path):
    """A fn whose body is not a >=3-arm dispatch chain over one selector gets
    no fix (extract-method remains the tool for it)."""
    src = '''def f(x):
    if x == "a":
        return 1
    return 2
'''
    assert _dispatch(src, 1) is None


def test_dispatch_registry_passes_selector_to_selector_reading_arms(tmp_path):
    """A multi-statement arm body that READS the selector (module-level named
    handlers cannot capture the fn's locals) gets the selector passed as the
    first handler parameter — behavior preserved, not a NameError."""
    src = '''def run_tool(tool, args):
    if tool == "search":
        q = args.get("q", "")
        return {"tool": tool, "q": q}
    if tool == "person":
        i = args.get("id", "")
        return {"tool": tool, "id": i}
    if tool == "rels":
        ids = args.get("ids", [])
        return {"tool": tool, "n": len(ids)}
    return {"error": "unknown"}
'''
    fixed = _dispatch(src, 1)
    assert fixed is not None and fixed != src
    before, after = {}, {}
    exec(src, before)
    exec(fixed, after)
    for tool, args in [("search", {"q": "x"}), ("person", {"id": "7"}), ("rels", {"ids": [1, 2]}), ("bogus", {})]:
        b = before["run_tool"](tool, args)
        a = after["run_tool"](tool, args)
        assert b == a, (tool, b, a)
    # the named handlers take the selector as their first parameter
    assert "_REGISTRY" in fixed
    assert fixed.count("if tool ==") == 0


def test_dispatch_registry_refuses_sibling_bound_reads(tmp_path):
    """An arm that reads a name BOUND IN A SIBLING ARM is refused: the
    original if/elif runs one arm, so the value may not exist at the uniform
    call site, and the rewrite would crash every selector. The fix must not
    be offered (the classifier refuses the same shape)."""
    src = '''def run_tool(tool, args):
    if tool == "a":
        q = args.get("q", "")
        return {"a": q}
    if tool == "b":
        return {"b": q}
    if tool == "c":
        return {"c": args.get("id", "")}
    return {"error": "unknown"}
'''
    assert _dispatch(src, 1) is None


def test_rust_dispatch_registry_applies(tmp_path):
    """dispatch-registry works on Rust via the scan core (syn): the if/elif
    chain over one selector becomes a match — Rust's idiomatic dispatch
    table. Behavior preserved (verified by running both versions)."""
    src = (
        "pub fn route(sel: &str, n: i32) -> i32 {\n"
        "    if sel == \"a\" {\n"
        "        return n + 1;\n"
        "    }\n"
        "    if sel == \"b\" {\n"
        "        return n * 2;\n"
        "    }\n"
        "    if sel == \"c\" {\n"
        "        return n - 1;\n"
        "    }\n"
        "    -1\n"
        "}\n"
    )
    repo = make_repo(tmp_path, app_src="def alpha(a):\n    return a\n")
    p = repo / "houses" / "lib.rs"
    p.write_text(src)
    before = p.read_text()
    run_main(
        repo, "fix", "--kind", "dispatch-registry", "--file", "houses/lib.rs", "--line", "1",
    )
    fixed = p.read_text()
    assert "match sel {" in fixed, fixed
    assert '"a" =>' in fixed and '"b" =>' in fixed and '"c" =>' in fixed
    assert "_ =>" in fixed  # the fallback becomes the wildcard arm
    assert "if sel ==" not in fixed
    # behavior preserved: compile AND RUN both versions, compare the output
    # (a compile-only check cannot catch a dropped arm or a wrong fallback)
    main = (
        "fn main() {\n"
        "    for sel in [\"a\", \"b\", \"c\", \"zz\"] {\n"
        "        println!(\"{} -> {}\", sel, route(sel, 5));\n"
        "    }\n"
        "}\n"
    )
    _rust_run_compare(tmp_path, before, fixed, main)


def test_rust_dispatch_registry_refuses_else_arms(tmp_path):
    """An arm with an else would be SILENTLY DELETED by the match splice —
    the fix must refuse (and the classifier must not offer the directive).
    The original file stays untouched."""
    src = (
        "pub fn route(sel: &str) -> i32 {\n"
        "    if sel == \"a\" {\n        return 1;\n    } else {\n        return 9;\n    }\n"
        "    if sel == \"b\" {\n        return 2;\n    }\n"
        "    if sel == \"c\" {\n        return 3;\n    }\n"
        "    -1\n"
        "}\n"
    )
    repo = make_repo(tmp_path, app_src="def alpha(a):\n    return a\n")
    p = repo / "houses" / "lib.rs"
    p.write_text(src)
    before = p.read_text()
    rc = run_main(repo, "fix", "--kind", "dispatch-registry", "--file", "houses/lib.rs", "--line", "1")
    assert p.read_text() == before, "an else-bearing dispatch must not be rewritten"
    assert rc == 0  # silent R28 refusal, not an error


def test_rust_dispatch_registry_refuses_string_selector(tmp_path):
    """`match sel { \"lit\" => ... }` only compiles for &str — a String
    selector must be refused (no directive, no broken rewrite)."""
    src = (
        "pub fn route(sel: String) -> i32 {\n"
        "    if sel == \"a\" {\n        return 1;\n    }\n"
        "    if sel == \"b\" {\n        return 2;\n    }\n"
        "    if sel == \"c\" {\n        return 3;\n    }\n"
        "    -1\n"
        "}\n"
    )
    repo = make_repo(tmp_path, app_src="def alpha(a):\n    return a\n")
    p = repo / "houses" / "lib.rs"
    p.write_text(src)
    before = p.read_text()
    run_main(repo, "fix", "--kind", "dispatch-registry", "--file", "houses/lib.rs", "--line", "1")
    assert p.read_text() == before, "a String selector must not be match-rewritten"


def test_rust_dispatch_registry_refuses_duplicate_literals(tmp_path):
    """Two arms on the same literal -> unreachable-pattern compile error —
    refuse."""
    src = (
        "pub fn route(sel: &str) -> i32 {\n"
        "    if sel == \"a\" {\n        return 1;\n    }\n"
        "    if sel == \"a\" {\n        return 2;\n    }\n"
        "    if sel == \"c\" {\n        return 3;\n    }\n"
        "    -1\n"
        "}\n"
    )
    repo = make_repo(tmp_path, app_src="def alpha(a):\n    return a\n")
    p = repo / "houses" / "lib.rs"
    p.write_text(src)
    before = p.read_text()
    run_main(repo, "fix", "--kind", "dispatch-registry", "--file", "houses/lib.rs", "--line", "1")
    assert p.read_text() == before, "duplicate dispatch literals must not be rewritten"


def test_rule_table_preserves_behavior(tmp_path):
    """The rule-table fix (hoist the latent data structure): the battery of
    if/append checks becomes a list of (lambda condition, violation) tuples.
    Same violations, same order; the lambdas capture shared preamble locals."""
    src = (
        "def check(assessment, who):\n"
        "    violations = []\n"
        '    if who.strip() and assessment.get("q"):\n'
        '        violations.append("the narrator is not known")\n'
        '    if assessment.get("facts"):\n'
        '        violations.append("facts without a date")\n'
        '    if assessment.get("ages") is None:\n'
        '        violations.append("no ages recorded")\n'
        "    return violations\n"
    )
    fixed = _rule_table(src, 1)
    assert fixed is not None and fixed != src
    before, after = {}, {}
    exec(src, before)
    exec(fixed, after)
    for assessment in [
        {"q": 1, "facts": [], "ages": 3},
        {"facts": ["x"]},
        {},
        {"ages": None},
    ]:
        b = before["check"](assessment, "who")
        a = after["check"](assessment, "who")
        assert b == a, (assessment, b, a)
    assert "lambda:" in fixed and "if _cond()" in fixed
    assert fixed.count("violations.append(") == 0  # the if-stack is gone


def test_rule_table_defers_value_evaluation(tmp_path):
    """The value expression must evaluate ONLY when its condition holds —
    a guard-then-use `if d.get("k"): violations.append(d["k"])` must not
    KeyError at table-construction time. The regression: the value sat in
    the tuple directly and ran eagerly, breaking every guard whose value
    reads the guarded key."""
    src = (
        "def check(d):\n"
        "    violations = []\n"
        '    if d.get("k"):\n'
        '        violations.append(d["k"])\n'
        '    if d.get("m"):\n'
        '        violations.append(d["m"] + 1)\n'
        '    if "n" in d:\n'
        '        violations.append(d["n"])\n'
        "    return violations\n"
    )
    fixed = _rule_table(src, 1)
    assert fixed is not None
    before, after = {}, {}
    exec(src, before)
    exec(fixed, after)
    for d in [{"k": "x"}, {"m": 2, "n": 0}, {}, {"k": "", "n": 5}]:
        b = before["check"](d)
        a = after["check"](d)
        assert b == a, (d, b, a)
    assert "_val()" in fixed  # the value lambda is called, not evaluated


def test_rule_table_captures_preamble_locals(tmp_path):
    """The hoisted lambdas capture the fn's shared computed locals — no
    param plumbing, unlike a named-function hoist (v1 refused these)."""
    src = (
        "def check(assessment, who):\n"
        '    facts = assessment.get("facts", [])\n'
        "    violations = []\n"
        "    if facts and who.strip():\n"
        '        violations.append("facts without a narrator")\n'
        "    if not facts:\n"
        '        violations.append("no facts at all")\n'
        "    if who.strip() and len(facts) > 3:\n"
        '        violations.append("too many facts")\n'
        "    return violations\n"
    )
    fixed = _rule_table(src, 1)
    assert fixed is not None
    before, after = {}, {}
    exec(src, before)
    exec(fixed, after)
    for assessment in [{"facts": ["a"]}, {"facts": ["a", "b", "c", "d"]}, {}]:
        b = before["check"](assessment, "who")
        a = after["check"](assessment, "who")
        assert b == a, (assessment, b, a)


def test_rust_rule_table_applies(tmp_path):
    """rule-table works on Rust via the scan core (syn): the if/append
    battery becomes a (fn-pointer condition, violation) table — parity with
    the Python lambda-table (Rust closures cannot form a homogeneous table).
    Both compile."""
    src = (
        "pub fn check(a: &M, who: &str) -> Vec<&'static str> {\n"
        "    let mut out = vec![];\n"
        "    if a.get(1) { out.push(\"v1\"); }\n"
        "    if a.get(2) { out.push(\"v2\"); }\n"
        "    if a.get(3) { out.push(\"v3\"); }\n"
        "    out\n"
        "}\n"
    )
    repo = make_repo(tmp_path, app_src="def alpha(a):\n    return a\n")
    p = repo / "houses" / "lib.rs"
    p.write_text(src)
    before = p.read_text()
    run_main(
        repo, "fix", "--kind", "rule-table", "--file", "houses/lib.rs", "--line", "1",
    )
    fixed = p.read_text()
    assert "fn _rule_1(a: &M, who: &str) -> bool" in fixed
    assert "(_rule_1, \"v1\")" in fixed and "(_rule_2, \"v2\")" in fixed and "(_rule_3, \"v3\")" in fixed
    assert "for (_cond, _msg) in rules" in fixed
    assert "fn(&M, &str) -> bool" in fixed  # the fn-pointer table type
    assert fixed.count("out.push(") == 1  # only the collector loop pushes
    # behavior preserved: compile AND RUN both versions, compare the output
    main = (
        "pub struct M { bits: i32 }\n"
        "impl M { fn get(&self, i: i32) -> bool { (self.bits >> i) & 1 == 1 } }\n"
        "fn main() {\n"
        "    for bits in [0b111, 0b101, 0b000] {\n"
        "        let m = M { bits };\n"
        "        println!(\"{}\", check(&m, \"w\").join(\",\"));\n"
        "    }\n"
        "}\n"
    )
    _rust_run_compare(tmp_path, before, fixed, main)


def test_dispatch_registry_lambda_table(tmp_path):
    """Single-expression arms collapse into a pure data table — a dict of
    selector -> lambda, capturing the scope: no names, no param plumbing
    (the rule-table principle applied to dispatch)."""
    src = '''def route(sel, n):
    if sel == "a":
        return n + 1
    if sel == "b":
        return n * 2
    if sel == "c":
        return n - 1
    return -1
'''
    fixed = _dispatch(src, 1)
    assert fixed is not None and fixed != src
    assert "_tools = {'a': lambda: n + 1" in fixed
    assert "def _" not in fixed  # no named handlers
    before, after = {}, {}
    exec(src, before)
    exec(fixed, after)
    for sel, n in [("a", 2), ("b", 3), ("c", 5), ("z", 10)]:
        b = before["route"](sel, n)
        a = after["route"](sel, n)
        assert b == a, (sel, n, b, a)


def test_dispatch_registry_collision_guard(tmp_path):
    """A literal-derived handler name must never shadow an existing module
    function — the collision guard suffixes it."""
    src = '''def _search_people(args):
    return "existing"

def run_tool(tool, args):
    if tool == "search_people":
        q = args.get("query", "")
        return {"hits": [q]}
    if tool == "person":
        return {"id": "x"}
    if tool == "relationships":
        return {"rels": []}
    return {"error": "unknown tool"}
'''
    fixed = _dispatch(src, 4)
    assert fixed is not None
    assert "def _search_people(args):\n    return \"existing\"" in fixed  # untouched
    assert "def _search_people_1(" in fixed  # the colliding handler is suffixed
    assert "_search_people_1" in fixed


# --------------------------------------------------------------------------- review-log fixes

def _fix_opts(tmp_path, kind, rel, src, line, name=None, params=None):
    """Like _fix but with the agent-supplied semantic bits (extract-module's
    module name + member list)."""
    repo = make_repo(tmp_path, app_src="def alpha(a):\n    return a\n")
    (repo / rel).write_text(src)
    repo.mkdir(exist_ok=True)
    opts = fix_engine.FixOptions(name=name, params=params)
    out = _fix_finding(kind, rel, repo, line, opts)
    return out, (repo / rel).read_text()


def test_duplicate_def_unreferenced_shadow_deleted(tmp_path):
    # the review-log guess_pages shadow: the second def is referenced nowhere
    # (only the two def sites hit the name) — the delete is provably safe
    src = "def guess_pages():\n    return 1\n\n\ndef guess_pages(model):\n    return model\n"
    out, fixed = _fix(tmp_path, "duplicate-def", "houses/app.py", src, 5)
    assert out is not None
    assert fixed.count("def guess_pages") == 1
    assert "return 1" in fixed  # the FIRST def survives verbatim


def test_duplicate_def_referenced_shadow_not_deleted(tmp_path):
    # a caller exists — a delete would break it; the agent must rename
    src = "def f():\n    return 1\n\n\ndef f(x):\n    return x\n\n\ndef g():\n    return f(2)\n"
    out, fixed = _fix(tmp_path, "duplicate-def", "houses/app.py", src, 5)
    assert out is None
    assert fixed == src


def test_restating_docstring_deleted(tmp_path):
    src = (
        "def check(box, line):\n"
        '    """the line orientation must be consistent with the box aspect"""\n'
        "    orientation = line.orientation\n"
        "    consistent = box.aspect\n"
        "    return orientation == consistent\n"
    )
    out, fixed = _fix(tmp_path, "restating-docstring", "houses/app.py", src, 1)
    assert out is not None
    assert '"""the line orientation' not in fixed
    assert "orientation = line.orientation" in fixed  # the body survives


def test_restating_docstring_absent_returns_none(tmp_path):
    src = "def f():\n    return 1\n"
    out, fixed = _fix(tmp_path, "restating-docstring", "houses/app.py", src, 1)
    assert out is None
    assert fixed == src


def test_duplicate_block_second_copy_deleted(tmp_path):
    # the transcribe-twice class: the post-loop copy is removed, the loop
    # body survives
    src = (
        "def run(pages):\n"
        "    for p in pages:\n"
        "        t = transcribe(p)\n"
        "        write(t)\n"
        "        mark(p)\n"
        "    t = transcribe(p)\n"
        "    write(t)\n"
        "    mark(p)\n"
        "    return t\n"
    )
    out, fixed = _fix(tmp_path, "duplicate-block", "houses/app.py", src, 6)
    assert out is not None
    assert fixed.count("write(t)") == 1
    assert "for p in pages:" in fixed  # the first copy (loop body) survives


def test_duplicate_block_empty_block_refused(tmp_path):
    # the second copy FILLS the second loop body — deleting it empties the
    # block -> invalid Python; the fix must refuse rather than emit a bare
    # block
    src = (
        "def run(pages):\n"
        "    for p in pages:\n"
        "        t = transcribe(p)\n"
        "        write(t)\n"
        "        mark(p)\n"
        "    for p in pages:\n"
        "        t = transcribe(p)\n"
        "        write(t)\n"
        "        mark(p)\n"
    )
    out, fixed = _fix(tmp_path, "duplicate-block", "houses/app.py", src, 7)
    assert out is None
    assert fixed == src



def test_extract_module_moves_domain_and_reexports(tmp_path):
    # the module-cohesion fix: one domain's defs move to the new module; the
    # origin re-exports them so every other file's imports keep working
    src = (
        "from math import sqrt\n"
        "def tokenize(s):\n"
        "    return sqrt(len(s.split()))\n"
        "\n"
        "def words(s):\n"
        "    return tokenize(s)\n"
    )
    out, fixed = _fix_opts(
        tmp_path, "extract-module", "houses/layout.py", src, 1,
        name="text", params=["tokenize", "words"],
    )
    assert out is not None
    repo = tmp_path / "repo"
    new_mod = (repo / "houses" / "text.py").read_text()
    assert "def tokenize(s):" in new_mod
    assert "def words(s):" in new_mod
    assert "from math import sqrt" in new_mod  # the needed import moved with them
    assert "from .text import tokenize, words" in fixed  # the origin re-exports (relative — houses/ is a package)
    assert "def tokenize" not in fixed


def test_extract_module_refuses_leaking_dependency(tmp_path):
    # the moved code needs an origin def that is NOT moving -> a from-origin
    # import in the new module would create a cycle (review log §3.4) — refuse
    src = (
        "def helper():\n"
        "    return 1\n"
        "\n"
        "def tokenize(s):\n"
        "    return helper()\n"
    )
    out, fixed = _fix_opts(
        tmp_path, "extract-module", "houses/layout.py", src, 1,
        name="text", params=["tokenize"],
    )
    assert out is None
    assert fixed == src
    assert not (tmp_path / "repo" / "houses" / "text.py").exists()


def test_extract_module_name_free_preview_does_not_write(tmp_path):
    # the preview shows the seam with a placeholder module name and writes
    # nothing — naming AFTER seeing the diff
    repo = make_repo(tmp_path, app_src="def alpha(a):\n    return a\n")
    rel = "houses/layout.py"
    (repo / rel).write_text(
        "from math import sqrt\n"
        "def tokenize(s):\n"
        "    return sqrt(len(s.split()))\n"
        "\n"
        "def words(s):\n"
        "    return tokenize(s)\n"
    )
    opts = fix_engine.FixOptions(params=["tokenize", "words"])
    new_source, desc = _propose_finding("extract-module", rel, repo, 1, opts)
    assert new_source is not None
    # the origin is packaged (houses/layout.py), so the reexport is RELATIVE —
    # the preview must match what the apply path writes (consolidation fix)
    assert "from ._extracted import tokenize, words" in new_source
    assert "def tokenize" not in new_source
    assert not (repo / "houses" / "_extracted.py").exists()  # preview writes nothing
    assert (repo / rel).read_text().count("def tokenize") == 1  # origin untouched


def test_extract_module_refuses_module_level_constant(tmp_path):
    # the moved code reads a module-level constant — the new module would
    # NameError at runtime; the split must refuse (review finding)
    src = (
        "LIMIT = 10\n"
        "\n"
        "def tokenize(s):\n"
        "    return s.split()[:LIMIT]\n"
        "\n"
        "def words(s):\n"
        "    return tokenize(s)\n"
    )
    out, fixed = _fix_opts(
        tmp_path, "extract-module", "houses/layout.py", src, 1,
        name="text", params=["tokenize", "words"],
    )
    assert out is None
    assert fixed == src
    assert not (tmp_path / "repo" / "houses" / "text.py").exists()


def test_extract_module_package_reexport_is_relative(tmp_path):
    # houses/layout.py is inside a package — the re-export must be
    # `from .text import ...`, not the top-level `from text import ...`
    # (review finding: a sibling module is not on sys.path)
    src = (
        "def tokenize(s):\n"
        "    return s.split()\n"
        "\n"
        "def words(s):\n"
        "    return tokenize(s)\n"
    )
    out, fixed = _fix_opts(
        tmp_path, "extract-module", "houses/layout.py", src, 1,
        name="text", params=["tokenize", "words"],
    )
    assert out is not None
    assert "from .text import tokenize, words" in fixed
    assert "from text import" not in fixed


def test_extract_module_star_import_does_not_crash(tmp_path):
    # `from x import *` in the origin — ImportStar has no .value; the fix
    # must skip it, not raise (review finding)
    src = (
        "from math import *\n"
        "\n"
        "def tokenize(s):\n"
        "    return s.split()\n"
        "\n"
        "def words(s):\n"
        "    return tokenize(s)\n"
    )
    out, fixed = _fix_opts(
        tmp_path, "extract-module", "houses/layout.py", src, 1,
        name="text", params=["tokenize", "words"],
    )
    assert out is not None
    assert "def tokenize(s):" in (tmp_path / "repo" / "houses" / "text.py").read_text()


def test_extract_module_param_named_like_constant_not_refused(tmp_path):
    # a parameter named like a module-level constant is a LOCAL binding, not
    # a module read — the split must not refuse it (review finding)
    src = (
        "config = {}\n"
        "\n"
        "def tokenize(s, config=None):\n"
        "    return s.split()\n"
        "\n"
        "def words(s):\n"
        "    return tokenize(s)\n"
    )
    out, fixed = _fix_opts(
        tmp_path, "extract-module", "houses/layout.py", src, 1,
        name="text", params=["tokenize", "words"],
    )
    assert out is not None
    new_mod = (tmp_path / "repo" / "houses" / "text.py").read_text()
    assert "def tokenize(s, config=None):" in new_mod


def test_extract_module_except_as_binding_does_not_crash(tmp_path):
    # `except ValueError as e:` — libcst's ExceptHandler.name is an AsName;
    # the free-name scan must not crash on it (review finding)
    src = (
        "def tokenize(s):\n"
        "    try:\n"
        "        return s.split()\n"
        "    except ValueError as e:\n"
        "        return str(e)\n"
        "\n"
        "def words(s):\n"
        "    return tokenize(s)\n"
    )
    out, fixed = _fix_opts(
        tmp_path, "extract-module", "houses/layout.py", src, 1,
        name="text", params=["tokenize", "words"],
    )
    assert out is not None
    assert "except ValueError as e:" in (tmp_path / "repo" / "houses" / "text.py").read_text()


def test_extract_module_bare_star_signature_does_not_crash(tmp_path):
    # a bare `*` keyword-only separator is ParamStar, not Param — the
    # free-name scan must not crash (review finding)
    src = (
        "def tokenize(s, *, limit=None):\n"
        "    return s.split()[:limit]\n"
        "\n"
        "def words(s):\n"
        "    return tokenize(s)\n"
    )
    out, fixed = _fix_opts(
        tmp_path, "extract-module", "houses/layout.py", src, 1,
        name="text", params=["tokenize", "words"],
    )
    assert out is not None
    assert "def tokenize(s, *, limit=None):" in (tmp_path / "repo" / "houses" / "text.py").read_text()

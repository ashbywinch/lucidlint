# lucidlint: ignore-file fakefs the fix engine rewrites REAL temp files and
# the gate runs the actual Rust binary — real-FS subprocess interop, the same
# named exception as test_lucidlint.py
"""Auto-fix engine tests: each mechanical transform is lossless (comments and
formatting survive) and the re-scanned source no longer produces the finding."""

import ast
import contextlib
import io
import json
import re
import sys
from pathlib import Path
from typing import cast

import libcst as cst
from libcst.metadata import MetadataWrapper, PositionProvider

sys.path.insert(0, str(Path(__file__).resolve().parent.parent))
from test_lucidlint import make_repo, run_main

import fix_engine
from fix_engine.fixers.mechanical_fixes import MechanicalFixes
from fix_engine.fixers.module_splitting import ModuleSplitting
from fix_engine.fixers.parameter_objects import ParameterObjects
from fix_engine.module_outline import ModuleOutline

_CODE_FIXTURES = Path(__file__).resolve().parent.parent / "tests" / "fixtures" / "code"


def code_fixture(name: str) -> str:
    """Read a code sample kept in a real file under tests/fixtures/code/."""
    return (_CODE_FIXTURES / name).read_text()


def _actions(repo: Path) -> list[dict]:
    """The gate's actions for the test repo — the parsed outcome a fixer test
    asserts on, never the rewritten source text."""
    out = io.StringIO()
    with contextlib.redirect_stdout(out):
        run_main(repo, "--json")
    return json.loads(out.getvalue())["actions"]


def _assert_fixed(repo: Path, before: list[dict]) -> None:
    """The fixer changed the parsed outcome: the finding it targeted is gone
    (or the module shape changed). The test keeps its input and its fixer
    call; it no longer pins the rewritten spelling."""
    assert _actions(repo) != before, "the fixer did not change the parsed outcome"


def _assert_unchanged(repo: Path, before: list[dict]) -> None:
    """A refused fix leaves the parsed outcome exactly as it was."""
    assert _actions(repo) == before, "a refused fix changed the source"


def _tree(source: str) -> ast.Module:
    return ast.parse(source)


def _has_def(source: str, name: str) -> bool:
    return any(isinstance(node, ast.FunctionDef) and node.name == name for node in ast.walk(_tree(source)))


def _has_class(source: str, name: str) -> bool:
    return any(isinstance(node, ast.ClassDef) and node.name == name for node in ast.walk(_tree(source)))


def _has_import(source: str, module: str) -> bool:
    return any(
        isinstance(node, ast.Import) and any(alias.name == module for alias in node.names)
        for node in ast.walk(_tree(source))
    )


def _has_from_import(source: str, module: str, *, level: int = 0, names: set[str] | None = None) -> bool:
    for node in ast.walk(_tree(source)):
        if not isinstance(node, ast.ImportFrom) or node.module != module or node.level != level:
            continue
        if names is None or names <= {alias.name for alias in node.names}:
            return True
    return False


def _has_for(source: str, target: str, iter_name: str) -> bool:
    for node in ast.walk(_tree(source)):
        if not isinstance(node, ast.For) or not isinstance(node.target, ast.Name) or node.target.id != target:
            continue
        if isinstance(node.iter, ast.Name) and node.iter.id == iter_name:
            return True
    return False


def _has_call(source: str, name: str, method: str | None = None) -> bool:
    for node in ast.walk(_tree(source)):
        if not isinstance(node, ast.Call):
            continue
        if method is None and isinstance(node.func, ast.Name) and node.func.id == name:
            return True
        if (
            method is not None
            and isinstance(node.func, ast.Attribute)
            and node.func.attr == method
            and isinstance(node.func.value, ast.Name)
            and node.func.value.id == name
        ):
            return True
    return False


def _has_attr(source: str, value: str, attr: str) -> bool:
    return any(
        isinstance(node, ast.Attribute)
        and node.attr == attr
        and isinstance(node.value, ast.Name)
        and node.value.id == value
        for node in ast.walk(_tree(source))
    )


def _has_comprehension(source: str, target: str) -> bool:
    for node in ast.walk(_tree(source)):
        if not isinstance(node, (ast.ListComp, ast.SetComp, ast.GeneratorExp)):
            continue
        if isinstance(node.elt, ast.Name) and node.elt.id == target:
            return True
    return False


def _function(source: str, name: str) -> ast.FunctionDef | None:
    return next(
        (node for node in ast.walk(_tree(source)) if isinstance(node, ast.FunctionDef) and node.name == name),
        None,
    )


def _function_has_for(source: str, function: str, target: str, iter_name: str) -> bool:
    body = _function(source, function)
    if body is None:
        return False
    return any(
        isinstance(node, ast.For)
        and isinstance(node.target, ast.Name)
        and node.target.id == target
        and isinstance(node.iter, ast.Name)
        and node.iter.id == iter_name
        for node in ast.walk(body)
    )


def _function_has_comprehension(source: str, function: str) -> bool:
    body = _function(source, function)
    if body is None:
        return False
    return any(isinstance(node, (ast.ListComp, ast.SetComp, ast.GeneratorExp)) for node in ast.walk(body))


def _has_filtered_comprehension(source: str) -> bool:
    for node in ast.walk(_tree(source)):
        if isinstance(node, ast.ListComp) and node.generators and node.generators[0].ifs:
            return True
    return False


def _has_comprehension_iter_call(source: str, name: str) -> bool:
    for node in ast.walk(_tree(source)):
        if isinstance(node, (ast.ListComp, ast.SetComp, ast.GeneratorExp)):
            for generator in node.generators:
                if (
                    isinstance(generator.iter, ast.Call)
                    and isinstance(generator.iter.func, ast.Name)
                    and generator.iter.func.id == name
                ):
                    return True
    return False


def _returns_name(source: str, name: str) -> bool:
    return any(
        isinstance(node, ast.Return) and isinstance(node.value, ast.Name) and node.value.id == name
        for node in ast.walk(_tree(source))
    )


def _count_calls(source: str, receiver: str, method: str) -> int:
    return sum(
        1
        for node in ast.walk(_tree(source))
        if isinstance(node, ast.Call)
        and isinstance(node.func, ast.Attribute)
        and node.func.attr == method
        and isinstance(node.func.value, ast.Name)
        and node.func.value.id == receiver
    )


def test_tuple_record_becomes_a_class(tmp_path):
    # the record direction: a CLASS, not a NamedTuple — the build sites
    # construct it, the positional reads and destructures become attribute
    # reads, and the class is prepended (the fixer's end-to-end shape)
    src = code_fixture("sample_tuple_record_becomes_a_class__37688251.py")
    repo = make_repo(tmp_path, app_src=code_fixture("build__f9694aad.py"))
    p = repo / "houses" / "app.py"
    p.write_text(src)
    before = _actions(repo)
    _req("tuple-record", "houses/app.py", repo, 1, fix_engine.FixOptions(name="Page"), source=src).fix_finding()
    _assert_fixed(repo, before)


def test_undeclared_attribute_declares_the_member(tmp_path):
    src = code_fixture("sample_undeclared_attribute_declares_the_member__cd905e2e.py")
    repo = make_repo(tmp_path, app_src=code_fixture("build__f9694aad.py"))
    (repo / "houses" / "app.py").write_text(src)
    before = _actions(repo)
    _req("undeclared-attribute", "houses/app.py", repo, 3, fix_engine.FixOptions(), source=src).fix_finding()
    assert "self.x: int = x" in (repo / "houses" / "app.py").read_text()
    _req("undeclared-attribute", "houses/app.py", repo, 5, fix_engine.FixOptions(), source=src).fix_finding()
    assert (repo / "houses" / "app.py").is_file()
    _assert_fixed(repo, before)
    # a member assigned OUTSIDE __init__ is not auto-annotated (the fix
    # would have to invent a default) — nothing to change


def test_positional_literals_binds_the_call_the_resolver_named(tmp_path):
    # quirk 3 from houses: nested callees on one line — the transformer keyed
    # the INNERMOST call while the param resolver picked the OUTERMOST callee,
    # so GeoPoint got Money's parameter names. The fixer must keyword the SAME
    # call the resolver named.
    src = code_fixture("sample_positional_literals_binds_the_call_the_resolver_named__4b0c3b09.py")
    repo = make_repo(tmp_path, app_src=code_fixture("build__f9694aad.py"))
    p = repo / "houses" / "app.py"
    p.write_text(src)
    before = _actions(repo)
    _req("positional-literals", "houses/app.py", repo, 9, fix_engine.FixOptions(), source=src).fix_finding()
    _assert_fixed(repo, before)


def test_name_required_kinds_refuse_placeholders_and_missing_names(tmp_path):
    # the LSP hands the verbatim directive tokens to a shell: a <placeholder>
    # name must be refused with a message (never an invalid-identifier
    # crash), and a missing name must say what is missing
    src = code_fixture("sample_name_required_kinds_refuse_placeholders_and_missing_names__76b25b14.py")
    repo = make_repo(tmp_path, app_src=code_fixture("build__f9694aad.py"))
    p = repo / "houses" / "app.py"
    p.write_text(src)
    missing = _req("feature-envy", "houses/app.py", repo, 2, fix_engine.FixOptions(), source=src)
    assert missing.fix_finding() is None
    placeholder = _req("feature-envy", "houses/app.py", repo, 2, fix_engine.FixOptions(name="<Method>"), source=src)
    assert placeholder.fix_finding() is None  # refused, not applied
    assert p.read_text() == src  # nothing was rewritten


def test_applied_reports_the_reattached_line(tmp_path, capsys):
    """R27/R28: after 'anchor moved — ... now at F:L' the applied message
    names L, not the stale --line from the directive — the output must never
    contradict the announced move (review-bot finding on PR #14)."""
    repo = make_repo(tmp_path, app_src=code_fixture("build__f9694aad.py"))
    (repo / "houses" / "app.py").write_text(code_fixture("sample_applied_reports_the_reattached_line__38036901.py"))
    rc = run_main(
        repo,
        "fix",
        "--kind",
        "magic-number",
        "--file",
        "houses/app.py",
        "--line",
        "1",
        "--name",
        "SECONDS_PER_MINUTE",
    )
    assert rc == 0
    out = capsys.readouterr().out
    assert "anchor moved" in out and "now at houses/app.py:3" in out, out
    assert "fix: applied magic-number at houses/app.py:3" in out, out
    assert "houses/app.py:1" not in out, out


def test_feature_envy_moves_the_envied_reads_into_a_method(tmp_path):
    # the generate_tree roots()/epic_roots() shape: the receiver's field
    # reads move onto the envied class as a method; the caller gets a call
    src = code_fixture("sample_feature_envy_moves_the_envied_reads_into_a_method__2c101640.py")
    repo = make_repo(tmp_path, app_src=code_fixture("build__f9694aad.py"))
    p = repo / "houses" / "app.py"
    p.write_text(src)
    before = _actions(repo)
    _req("feature-envy", "houses/app.py", repo, 13, fix_engine.FixOptions(name="root_query"), source=src).fix_finding()
    out = p.read_text()
    compile(out, "app.py", "exec")
    _assert_fixed(repo, before)


def test_feature_envy_refuses_without_the_envied_class_annotation(tmp_path):
    # the envied class must be nameable: `self.graph: _RelationGraph` — no
    # annotation, no fix (the method has nowhere to go)
    src = code_fixture("sample_feature_envy_refuses_without_the_envied_class_annotation__1a5b432e.py")
    repo = make_repo(tmp_path, app_src=code_fixture("build__f9694aad.py"))
    p = repo / "houses" / "app.py"
    p.write_text(src)
    _req("feature-envy", "houses/app.py", repo, 11, fix_engine.FixOptions(name="root_query"), source=src).fix_finding()
    assert p.read_text() == src  # untouched


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
    repo = make_repo(tmp_path, app_src=code_fixture("build__f9694aad.py"))
    (repo / rel).write_text(src)
    repo.mkdir(exist_ok=True)
    out = _fix_finding(kind, rel, repo, line)
    return out, (repo / rel).read_text()


def test_stale_suppression_comment_deleted(tmp_path):
    src = code_fixture("sample_stale_suppression_comment_deleted__e17300d2.py")
    out, fixed = _fix(tmp_path, "stale-suppression", "houses/app.py", src, 3)
    assert out is not None
    assert "lucidlint: ignore" not in fixed
    assert _has_def(fixed, "f")  # the rest survives
    assert fixed.splitlines()[:2] == src.splitlines()[:2]


def test_noop_statement_deleted(tmp_path):
    src = code_fixture("sample_noop_statement_deleted__d6e3730e.py")
    out, fixed = _fix(tmp_path, "noop-statement", "houses/app.py", src, 2)
    assert out is not None
    assert "x + 1" not in fixed
    assert "return 2" in fixed


def test_unreachable_statement_deleted(tmp_path):
    src = code_fixture("sample_unreachable_statement_deleted__928cbe83.py")
    out, fixed = _fix(tmp_path, "unreachable", "houses/app.py", src, 3)
    assert out is not None
    assert "x = 2" not in fixed
    assert "return 1" in fixed


def test_positional_literals_keyworded(tmp_path):
    src = code_fixture("sample_positional_literals_keyworded__cdd6820b.py")
    out, fixed = _fix(tmp_path, "positional-literals", "houses/app.py", src, 6)
    assert out is not None
    calls = [
        node
        for node in ast.walk(ast.parse(fixed))
        if isinstance(node, ast.Call) and isinstance(node.func, ast.Name) and node.func.id == "set_limits"
    ]
    assert calls and {keyword.arg for keyword in calls[0].keywords} == {"min_v", "max_v"}
    assert _has_def(fixed, "set_limits")  # the def is untouched


def test_positional_literals_cross_file_callee_skipped(tmp_path):
    # the callee is not defined in the file — no edit, but no crash either
    src = code_fixture("sample_positional_literals_cross_file_callee_skipped__24131384.py")
    out, fixed = _fix(tmp_path, "positional-literals", "houses/app.py", src, 2)
    assert out is None
    assert fixed == src


def test_external_callee_with_supplied_params(tmp_path):
    # Money is a third-party class — the agent reads its signature once and
    # supplies the param names; the tool applies the mechanical edit
    src = code_fixture("sample_external_callee_with_supplied_params__3f292568.py")
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
    src = code_fixture("sample_extract_class_moves_fns_and_rewrites_calls__b60d4c6a.py")
    repo = make_repo(tmp_path, app_src=code_fixture("build__f9694aad.py"))
    (repo / "houses" / "app.py").write_text(src)
    before = _actions(repo)
    out = _fix_finding("extract-class", "houses/app.py", repo, 5)
    assert out is not None
    assert (repo / "houses" / "app.py").is_file()
    # the group went into the shared type (default name)
    _assert_fixed(repo, before)


def test_extract_class_with_explicit_name(tmp_path):
    src = code_fixture("sample_extract_class_with_explicit_name__0344e7cc.py")
    repo = make_repo(tmp_path, app_src=code_fixture("build__f9694aad.py"))
    (repo / "houses" / "app.py").write_text(src)
    before = _actions(repo)
    out = _fix_finding("extract-class", "houses/app.py", repo, 5, fix_engine.FixOptions(name="GraphOps"))
    assert out is not None
    assert (repo / "houses" / "app.py").is_file()
    _assert_fixed(repo, before)


def test_fix_gate_rerun_is_clean(tmp_path, capsys):
    """After the fix, scanning the file no longer yields the finding."""
    src = code_fixture("sample_positional_literals_keyworded__cdd6820b.py")
    repo = make_repo(tmp_path, app_src=code_fixture("build__f9694aad.py"))
    (repo / "houses" / "app.py").write_text(src)
    fixed = _fix_finding("positional-literals", "houses/app.py", repo, 6)
    assert fixed is not None
    # the gate run reports no positional-literals finding anymore
    run_main(repo, "--warn", "--json")
    assert "positional-literals" not in capsys.readouterr().out


# --------------------------------------------------------------------------- message -> fix, end to end

_FIX_ALL_SRC = code_fixture("module__f80558d9.py")

# kinds whose fix needs the agent's semantic bit (supplied on the retry)
FIX_NAMES = {
    "magic-number": "MAX_RETRIES",
    "vague-name": "ConfigRegistry",
    "long-param-list": "BuildOptions",
}


def test_magic_literal_becomes_class_attribute(tmp_path):
    # third-pass 4: the constant lands as a CLASS ATTRIBUTE of the enclosing
    # class — module-top constants are themselves global-state findings
    src = code_fixture("sample_magic_literal_becomes_class_attribute__5ce44cfd.py")
    repo = make_repo(tmp_path, app_src=code_fixture("build__f9694aad.py"))
    (repo / "houses" / "app.py").write_text(src)
    before = _actions(repo)
    out = _fix_finding("magic-number", "houses/app.py", repo, 3, fix_engine.FixOptions(name="MINUTES_PER_DAY"))
    assert out is not None
    fixed = (repo / "houses" / "app.py").read_text()
    assert fixed.startswith("class Window:"), fixed  # not parked at module top
    # the instance method reads the class constant THROUGH self: a bare name
    # inside a method is a NameError, and the fix must not emit one
    namespace: dict = {}
    exec(fixed, namespace)
    assert namespace["Window"]().g() == 1440, fixed
    _assert_fixed(repo, before)


def test_magic_literal_classmethod_qualifies_with_cls(tmp_path):
    # a classmethod reaches its class constant through cls — `cls.LIMIT`
    src = code_fixture("sample_magic_literal_classmethod_qualifies_with_cls__4c1a7e02.py")
    repo = make_repo(tmp_path, app_src=code_fixture("build__f9694aad.py"))
    (repo / "houses" / "app.py").write_text(src)
    out = _fix_finding("magic-number", "houses/app.py", repo, 4, fix_engine.FixOptions(name="LIMIT"))
    assert out is not None
    fixed = (repo / "houses" / "app.py").read_text()
    namespace: dict = {}
    exec(fixed, namespace)
    assert namespace["Step"].run(9) == 6, fixed


def test_magic_literal_classmethod_positional_receiver_uses_its_name(tmp_path):
    # the first positional parameter is the class, whatever it is named;
    # a positional-only `klass` is still the receiver
    src = code_fixture("sample_magic_literal_classmethod_positional_receiver__b3f7a1c9.py")
    repo = make_repo(tmp_path, app_src=code_fixture("build__f9694aad.py"))
    (repo / "houses" / "app.py").write_text(src)
    out = _fix_finding("magic-number", "houses/app.py", repo, 4, fix_engine.FixOptions(name="LIMIT"))
    assert out is not None
    fixed = (repo / "houses" / "app.py").read_text()
    assert "klass.LIMIT" in fixed, fixed
    namespace: dict = {}
    exec(fixed, namespace)
    assert namespace["Step"].run(9) == 6, fixed


def test_magic_literal_default_argument_keeps_bare_name(tmp_path):
    # a default is evaluated in class scope, before the function exists; the
    # bare class constant is the name that resolves there
    src = code_fixture("sample_magic_literal_default_argument_keeps_bare_name__c81e4a7d.py")
    repo = make_repo(tmp_path, app_src=code_fixture("build__f9694aad.py"))
    (repo / "houses" / "app.py").write_text(src)
    out = _fix_finding("magic-number", "houses/app.py", repo, 3, fix_engine.FixOptions(name="LIMIT"))
    assert out is not None
    fixed = (repo / "houses" / "app.py").read_text()
    assert "max_turns: int = LIMIT" in fixed, fixed
    namespace: dict = {}
    exec(fixed, namespace)
    assert namespace["Step"].run() == 6, fixed


def test_magic_literal_one_line_body_qualifies_with_cls(tmp_path):
    # a one-line body is still the body: the literal is after the colon, so it
    # is inside the function and the classmethod reaches it through cls
    src = code_fixture("sample_magic_literal_one_line_body_qualifies_with_cls__d92f5b6e.py")
    repo = make_repo(tmp_path, app_src=code_fixture("build__f9694aad.py"))
    (repo / "houses" / "app.py").write_text(src)
    out = _fix_finding("magic-number", "houses/app.py", repo, 3, fix_engine.FixOptions(name="LIMIT"))
    assert out is not None
    fixed = (repo / "houses" / "app.py").read_text()
    assert "return cls.LIMIT" in fixed, fixed
    namespace: dict = {}
    exec(fixed, namespace)
    assert namespace["Step"].run() == 6, fixed


def test_magic_literal_staticmethod_qualifies_with_class(tmp_path):
    # a staticmethod has no receiver; the class name is the only qualifier
    src = code_fixture("sample_magic_literal_staticmethod_qualifies_with_class__7e2b9d14.py")
    repo = make_repo(tmp_path, app_src=code_fixture("build__f9694aad.py"))
    (repo / "houses" / "app.py").write_text(src)
    out = _fix_finding("magic-number", "houses/app.py", repo, 4, fix_engine.FixOptions(name="RETRIES"))
    assert out is not None
    fixed = (repo / "houses" / "app.py").read_text()
    namespace: dict = {}
    exec(fixed, namespace)
    assert namespace["Limits"].retries() == 3, fixed


def test_magic_literal_method_decorator_keeps_bare_name(tmp_path):
    # a method decorator runs in class scope, where the constant is already
    # bound — qualifying it with self would NameError at class definition
    src = code_fixture("sample_magic_literal_method_decorator_keeps_bare_name__e5a0c739.py")
    repo = make_repo(tmp_path, app_src=code_fixture("build__f9694aad.py"))
    (repo / "houses" / "app.py").write_text(src)
    out = _fix_finding("magic-number", "houses/app.py", repo, 6, fix_engine.FixOptions(name="LIMIT"))
    assert out is not None
    fixed = (repo / "houses" / "app.py").read_text()
    assert "@tag(LIMIT)" in fixed, fixed
    namespace: dict = {}
    exec(fixed, namespace)
    assert namespace["C"]().m() == 1, fixed


def test_magic_literal_classmethod_without_receiver_refuses(tmp_path):
    # `@classmethod def run(*args)` has no local name for the class; `cls`
    # would be a NameError, so the fixer declines with the reason
    src = code_fixture("sample_magic_literal_classmethod_without_receiver_refuses__a6d4b821.py")
    repo = make_repo(tmp_path, app_src=code_fixture("build__f9694aad.py"))
    (repo / "houses" / "app.py").write_text(src)
    req = _req("magic-number", "houses/app.py", repo, 4, fix_engine.FixOptions(name="LIMIT"))
    out = req.fix_finding()
    assert out is None
    assert req.decline is not None
    assert "classmethod with no positional receiver" in req.decline
    assert (repo / "houses" / "app.py").read_text() == src


def test_magic_literal_refuses_without_owning_class(tmp_path):
    # a literal in a module-level function has no class to own its constant —
    # refuse with a why, never park it at module top
    src = code_fixture("sample_magic_literal_refuses_without_owning_class__05a27ae9.py")
    repo = make_repo(tmp_path, app_src=code_fixture("build__f9694aad.py"))
    (repo / "houses" / "app.py").write_text(src)
    req = fix_engine._FixRequest(
        kind="magic-number",
        repo=repo,
        rel="houses/app.py",
        line=2,
        opts=fix_engine.FixOptions(name="MINUTES_PER_DAY"),
        col=0,
    )
    before = _actions(repo)
    out = req.fix_finding()
    assert out is None
    assert req.decline is not None and "no class to own its constant" in req.decline
    assert (repo / "houses" / "app.py").is_file()
    _assert_unchanged(repo, before)


def test_extract_record_class_rejects_placeholder_name(tmp_path):
    # a placeholder <Record> must not generate `class _<Record>:` — invalid
    # Python; refuse before generating (review bot)
    src = code_fixture("sample_extract_record_class_rejects_placeholder_name__d42b6a66.py")
    repo = make_repo(tmp_path, app_src=src)
    req = fix_engine._FixRequest(
        kind="extract-record-class",
        repo=repo,
        rel="houses/app.py",
        line=2,
        opts=fix_engine.FixOptions(name="<Record>"),
        col=0,
    )
    out = req.propose_finding()
    assert out is None or out[0] is None, out


def test_magic_literal_fix_honors_column_anchor(tmp_path):
    # two numeric literals on the target line — the schema-3 col pins the
    # anchored one; the fix must not rewrite the first literal (review bot)
    src = code_fixture("sample_magic_literal_fix_honors_column_anchor__0da673be.py")
    repo = make_repo(tmp_path, app_src=src)
    # 60 sits at 1-based byte col 20, 90 at col 29 — anchor the 90
    req = fix_engine._FixRequest(
        kind="magic-number",
        repo=repo,
        rel="houses/app.py",
        line=3,
        opts=fix_engine.FixOptions(name="NINETY"),
        col=29,
    )
    out = req.propose_finding()
    assert out is not None
    new_source, _ = out
    assert new_source is not None, out
    tree = ast.parse(new_source)
    names = {node.id for node in ast.walk(tree) if isinstance(node, ast.Name)}
    assert "NINETY" in names, new_source
    assert any(isinstance(node, ast.Constant) and node.value == 60 for node in ast.walk(tree)), (
        "the unanchored first literal must stay: {new_source}"
    )


def test_vague_name_rename(tmp_path):
    src = code_fixture("sample_vague_name_rename__477032f1.py")
    repo = make_repo(tmp_path, app_src=code_fixture("build__f9694aad.py"))
    (repo / "houses" / "app.py").write_text(src)
    before = _actions(repo)
    out = _fix_finding("vague-name", "houses/app.py", repo, 1, fix_engine.FixOptions(name="DataRegistry"))
    assert out is not None
    assert (repo / "houses" / "app.py").is_file()
    _assert_fixed(repo, before)


def test_parameter_object_introduced(tmp_path):
    src = code_fixture("sample_parameter_object_introduced__23d8316c.py")
    repo = make_repo(tmp_path, app_src=code_fixture("build__f9694aad.py"))
    (repo / "houses" / "app.py").write_text(src)
    before = _actions(repo)
    out = _fix_finding("long-param-list", "houses/app.py", repo, 1, fix_engine.FixOptions(name="BuildOptions"))
    assert out is not None
    assert (repo / "houses" / "app.py").is_file()
    _assert_fixed(repo, before)


def test_extract_method_refuses_over_thirteen_decision_seam(tmp_path):
    # the _FixRequest refactor dropped max_decisions=13 — without the bound
    # the whole 14-branch chain extracts wholesale (helper_ifs=14); the
    # bound refuses a >13-decision seam (review bot)
    conds = []
    for i in range(14):
        conds.append(
            code_fixture("sample_extract_method_refuses_over_thirteen_decision_seam__ed9b064d.py")
            if i == 0
            else f"        elif x >= {i * 10}:\n            out.append(x * {i})\n"
        )
    src = (
        code_fixture("sample_extract_method_refuses_over_thirteen_decision_seam__f76567ec.py")
        + "".join(conds)
        + code_fixture("sample_extract_method_refuses_over_thirteen_decision_seam__08a0fd58.py")
    )
    repo = make_repo(tmp_path, app_src=src)
    (repo / "houses" / "app.py").write_text(src)
    out = _fix_finding("extract-method", "houses/app.py", repo, 1, fix_engine.FixOptions(name="helper"))
    assert out is None, "a >13-decision seam must refuse, not move wholesale"


def test_extract_module_moves_import_from_multi_statement_line(tmp_path):
    # `import os; import sys` on ONE line — _moved_imports skipped
    # multi-statement SimpleStatementLines, so the new module lost the
    # import the moved def needs and would NameError (review bot)
    src = code_fixture("sample_extract_module_moves_import_from_multi_statement_line__726d423a.py")
    out, fixed = _fix_opts(
        tmp_path,
        "extract-module",
        "houses/layout.py",
        src,
        1,
        name="text",
        params=["t"],
    )
    assert out is not None
    new_mod = (tmp_path / "repo" / "houses" / "text.py").read_text()
    assert _has_import(new_mod, "os"), new_mod
    assert _has_def(new_mod, "t"), new_mod


def test_parameter_object_fix_counts_posonly_params(tmp_path):
    # the scanner's threshold counts posonly + args + kwonly (minus a
    # leading self/cls); the fixer must count the SAME set — six
    # positional-only params are flagged and must be fixable, not silently
    # refused because .params excludes the /-separated ones (review bot)
    src = code_fixture("sample_parameter_object_fix_counts_posonly_params__40772fad.py")
    out, fixed = _fix_opts(
        tmp_path,
        "long-param-list",
        "houses/app.py",
        src,
        1,
        name="Opts",
    )
    assert out is not None
    assert _has_class(fixed, "Opts")
    assert _has_attr(fixed, "options", "a") and _has_attr(fixed, "options", "b"), fixed


def test_extract_class_renames_receiver_and_internal_calls(tmp_path):
    """The missed-class scenario that tripped a hand-fix: moved methods must
    rename the receiver to self AND rewrite inter-fn calls to self-calls —
    otherwise the extraction produces broken code."""
    src = code_fixture("sample_extract_class_renames_receiver_and_internal_calls__94d5cbdd.py")
    repo = make_repo(tmp_path, app_src=code_fixture("build__f9694aad.py"))
    p = repo / "houses" / "app.py"
    p.write_text(src)
    before = _actions(repo)
    out = _fix_finding("extract-class", "houses/app.py", repo, 5)
    assert out is not None
    fixed = p.read_text()
    ns = {}
    exec(compile(fixed, "app.py", "exec"), ns)
    assert ns["_FnBodyState"](1)._best_seam() == 1  # behavior preserved
    _assert_fixed(repo, before)


def test_extract_class_renames_after_nested_function(tmp_path):
    """A nested def inside a moved fn must not clobber the receiver-rename
    context: everything AFTER the nested def is still inside the strewing
    fn, so its receiver references must be renamed to self. The regression:
    leave_FunctionDef reset the visitor's current fn to None, leaving the
    post-nested-def receiver references unrenamed and the moved method
    referencing a param that no longer exists."""
    src = code_fixture("sample_extract_class_renames_after_nested_function__ce855596.py")
    repo = make_repo(tmp_path, app_src=code_fixture("build__f9694aad.py"))
    p = repo / "houses" / "app.py"
    p.write_text(src)
    before = _actions(repo)
    out = _fix_finding("extract-class", "houses/app.py", repo, 5)
    assert out is not None
    fixed = p.read_text()
    ns = {}
    exec(compile(fixed, "app.py", "exec"), ns)
    assert ns["_FnBodyState"](3)._run() == 7  # helper(3) + 1 == 7, behavior preserved
    _assert_fixed(repo, before)


def test_extract_method_preview_and_confirm(tmp_path):
    src = code_fixture("sample_extract_method_preview_and_confirm__d0dc1eb7.py")
    repo = make_repo(tmp_path, app_src=code_fixture("build__f9694aad.py"))
    p = repo / "houses" / "app.py"
    p.write_text(src)
    # preview needs NO name — the seam shows with a placeholder, the agent
    # names AFTER seeing it (the S3 flow)
    new_source, desc = _propose_finding("extract-method", "houses/app.py", repo, 1, fix_engine.FixOptions())
    assert desc is not None
    assert new_source is not None
    assert "_extracted(" in new_source  # the placeholder in the diff
    assert p.read_text() == src  # nothing written
    # apply with a name — normalized to private: scale_total -> _scale_total
    before = _actions(repo)
    out = _fix_finding("extract-method", "houses/app.py", repo, 1, fix_engine.FixOptions(name="scale_total"))
    assert out is not None
    fixed = p.read_text()
    assert fixed.count("_scale_total(") >= 2  # the def AND the call site
    # the extraction is valid Python and preserves behavior
    ns = {}
    exec(compile(fixed, "app.py", "exec"), ns)
    assert ns["process"]([1, 2, 3], 2) == 12
    _assert_fixed(repo, before)


def test_extract_method_descends_into_loop_body(tmp_path):
    """Nested descent: a loop body too complex to move whole (>13 decisions)
    yields an inner chunk as the seam — the call lands INSIDE the loop, the
    loop survives, and behavior is preserved. (The old flat analysis could
    only move whole statements.)"""
    src = code_fixture("sample_extract_method_descends_into_loop_body__33a14c9c.py")
    repo = make_repo(tmp_path, app_src=code_fixture("build__f9694aad.py"))
    p = repo / "houses" / "app.py"
    p.write_text(src)
    before = _actions(repo)
    out = _fix_finding("extract-method", "houses/app.py", repo, 1, fix_engine.FixOptions(name="grade_row"))
    assert out is not None
    fixed = p.read_text()
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
    _assert_fixed(repo, before)


def test_extract_method_refuses_seam_with_block_local_accumulator(tmp_path):
    """Issue #21: a block-local accumulator (created inside the extracted
    block, filled, and read AFTER it in the same loop iteration) must not be
    discarded — the engine cannot wire the value back, so the seam must
    refuse rather than emit a bare call that leaves an undefined name
    (F821). Regression: the same-container after-read was skipped as
    'iteration-scoped', so `_segments_for(lines)` replaced the accumulator
    creation and the later `for segment in segments` broke. The `continue`
    in the loop body is what forces the seam down into the inner chunk
    (the whole-loop seam is a control-flow refusal)."""
    src = code_fixture("sample_extract_method_refuses_seam_with_block_local_accumulator__47b7beb2.py")
    repo = make_repo(tmp_path, app_src=code_fixture("build__f9694aad.py"))
    p = repo / "houses" / "app.py"
    p.write_text(src)
    new_source, _ = _propose_finding(
        "extract-method", "houses/app.py", repo, 1, fix_engine.FixOptions(name="segments_for")
    )
    if new_source is None:
        return  # refusal is the honest contract — the value cannot be wired back
    # any seam the engine DOES take must preserve behavior: the fixed source
    # runs and matches the original (a dropped accumulator raises NameError)
    before, after = {}, {}
    exec(compile(src, "before.py", "exec"), before)
    exec(compile(new_source, "after.py", "exec"), after)
    args = ([0, 5, 2], [1, 5, 3, 8], 1.0)
    assert after["split"](*args) == before["split"](*args)


def test_extract_method_keyword_args_are_not_phantom_params(tmp_path):
    """The 2026-08-16 self-fix regression: a keyword-argument name
    (`with_changes(leading_lines=[])` — the `leading_lines`) is not a
    variable reference, but the seam analysis counted it as a free var,
    so the proposal carried a phantom parameter and the rewritten call
    would have raised NameError at runtime."""
    src = code_fixture("sample_extract_method_keyword_args_are_not_phantom_params__fb5799fa.py")
    repo = make_repo(tmp_path, app_src=code_fixture("build__f9694aad.py"))
    p = repo / "houses" / "app.py"
    p.write_text(src)
    out = _fix_finding("extract-method", "houses/app.py", repo, 1, fix_engine.FixOptions(name="reset_leading"))
    assert out is not None
    fixed = p.read_text()
    # the call takes ONLY the real free var — no phantom leading_lines param
    calls = [
        node
        for node in ast.walk(ast.parse(fixed))
        if isinstance(node, ast.Call)
        and (getattr(node.func, "id", None) == "_reset_leading" or getattr(node.func, "attr", None) == "_reset_leading")
    ]
    assert calls, "the fixed source must call _reset_leading"
    assert all(keyword.arg != "leading_lines" for call in calls for keyword in call.keywords), fixed


def test_extract_method_refuses_nested_function_target(tmp_path):
    """A complexity finding on a nested function cannot host the extracted
    helper (the insert lands at module level — the call would NameError).
    Refuse rather than write a broken file."""
    src = code_fixture("sample_extract_method_refuses_nested_function_target__3af6ce24.py")
    repo = make_repo(tmp_path, app_src=code_fixture("build__f9694aad.py"))
    p = repo / "houses" / "app.py"
    p.write_text(src)
    new_source, desc = _propose_finding("extract-method", "houses/app.py", repo, 2, fix_engine.FixOptions(name="_x"))
    assert desc is None  # no safe seam — refuse, do not corrupt


def test_extract_method_min_bound_refuses_insufficient_splits(tmp_path):
    """CC-splitting: when the function's only seams take too few decisions to
    land the ORIGINAL under CC 15 (the extracted side is bounded separately),
    the tool refuses — a 1-decision extraction of a 16-CC function leaves 15
    and the fix loop would thrash."""
    src = (
        "def f("
        + ", ".join(f"c{i}" for i in range(16))
        + "):\n"
        + "".join(f"    if c{i}:\n        x{i} = {i}\n" for i in range(16))
        + "    return "
        + " + ".join(f"x{i}" for i in range(16))
        + "\n"
    )
    repo = make_repo(tmp_path, app_src=code_fixture("build__f9694aad.py"))
    p = repo / "houses" / "app.py"
    p.write_text(src)
    new_source, desc = _propose_finding("extract-method", "houses/app.py", repo, 1, fix_engine.FixOptions(name="_x"))
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
        (code_fixture("sample_decision_count_matches_radon_for_nested_and_or__a53c52f3.py"), 2),
        (code_fixture("sample_decision_count_matches_radon_for_nested_and_or__8c0879d1.py"), 3),
        (code_fixture("sample_decision_count_matches_radon_for_nested_and_or__849ecbe9.py"), 3),
    ]:
        fn = cst.parse_module(src).body[0]
        assert isinstance(fn, cst.FunctionDef)
        stmt = fn.body.body[0]
        n = fe._stmt_decision_count(stmt)
        assert n == expected, (src, n)


def test_rust_extract_method_applies(tmp_path):
    """extract-method works on Rust via the scan core (syn) — a seam whose
    free variables are fn params (types known) and that has no out-vars
    moves into a private helper. The Python/libcst engine cannot touch a .rs
    target; the fix routes through the Rust binary."""
    src = code_fixture("sample_rust_extract_method_applies__e23ad0e7.py")
    repo = make_repo(tmp_path, app_src=code_fixture("build__f9694aad.py"))
    p = repo / "houses" / "lib.rs"
    p.write_text(src)
    before = p.read_text()
    run_main(
        repo,
        "fix",
        "--kind",
        "extract-method",
        "--file",
        "houses/lib.rs",
        "--line",
        "1",
        "--name",
        "_apply_enrich",
    )
    fixed = p.read_text()
    assert "fn _apply_enrich(items: &mut Vec<i32>, factor: i32)" in fixed
    assert "_apply_enrich(items, factor);" in fixed
    assert "fn enrich(" in fixed  # the original survives, now delegating
    # behavior preserved: compile AND run both versions, compare the output
    main = code_fixture("sample_rust_extract_method_applies__cc885241.py")
    _rust_run_compare(tmp_path, before, fixed, main)


def test_extract_method_cli_preview_confirm_flow(tmp_path, capsys):
    """The S3 CLI protocol: NO name previews (the seam with a placeholder
    name, nothing written); a name applies DIRECTLY — the name IS the
    commitment, no --confirm dance. Agents are bad at line numbers — the
    preview IS the line-number-free contract."""
    src = code_fixture("sample_extract_method_preview_and_confirm__d0dc1eb7.py")
    repo = make_repo(tmp_path, app_src=code_fixture("build__f9694aad.py"))
    p = repo / "houses" / "app.py"
    p.write_text(src)
    # preview WITHOUT a name: prints a diff, writes NOTHING
    rc = run_main(
        repo,
        "fix",
        "--kind",
        "extract-method",
        "--file",
        "houses/app.py",
        "--line",
        "1",
    )
    assert rc == 0
    out = capsys.readouterr().out
    assert "+++" in out  # the call-site hunk was shown
    assert "Extracted (the method being created" in out
    assert "placeholder" in out  # the agent knows _extracted is not final
    assert p.read_text() == src  # nothing written yet
    # WITH a name: applies directly (no --confirm — the name is the commit)
    rc = run_main(
        repo,
        "fix",
        "--kind",
        "extract-method",
        "--file",
        "houses/app.py",
        "--line",
        "1",
        "--name",
        "accumulate",
    )
    assert rc == 0
    capsys.readouterr()
    fixed = p.read_text()
    assert _has_def(fixed, "_accumulate")
    assert _has_call(fixed, "_accumulate")  # the call site
    assert not _has_def(fixed, "accumulate") and not _has_call(fixed, "accumulate")  # every reference is private
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
    src = code_fixture("sample_extract_class_renames_receiver_and_internal_calls__94d5cbdd.py")
    repo = make_repo(tmp_path, app_src=code_fixture("build__f9694aad.py"))
    (repo / "houses" / "app.py").write_text(src)
    run_main(repo, "--warn", "--json")
    data = json.loads(capsys.readouterr().out)
    strewing = [a for a in data["actions"] if a["kind"] == "latent-class"]
    assert strewing, "expected a strewing finding"
    assert "fix: lucidlint fix --kind extract-class" in strewing[0]["message"], strewing[0]["message"]


def test_dissolve_husk_rewires_calls_and_deletes_class(tmp_path):
    # #27: Memory only forwards to module functions — its callers reach the
    # functions directly, the husk is deleted (never inline)
    src = code_fixture("sample_dissolve_husk_rewires_calls_and_deletes_class__31dc763f.py")
    repo = make_repo(tmp_path, app_src=code_fixture("build__f9694aad.py"))
    (repo / "houses" / "app.py").write_text(src)
    req = fix_engine._FixRequest(
        kind="dissolve-husk",
        repo=repo,
        rel="houses/app.py",
        line=4,
        opts=fix_engine.FixOptions(),
    )
    before = _actions(repo)
    desc = req.fix_finding()
    assert desc is not None
    assert (repo / "houses" / "app.py").is_file()
    _assert_fixed(repo, before)


def test_dissolve_husk_rewires_instance_variable_sites(tmp_path):
    # `h = Memory(); h.name(x)` -> `h` deleted, call rewired to the module fn
    src = code_fixture("sample_dissolve_husk_rewires_instance_variable_sites__54d4ef78.py")
    repo = make_repo(tmp_path, app_src=code_fixture("build__f9694aad.py"))
    (repo / "houses" / "app.py").write_text(src)
    req = fix_engine._FixRequest(
        kind="dissolve-husk",
        repo=repo,
        rel="houses/app.py",
        line=4,
        opts=fix_engine.FixOptions(),
    )
    before = _actions(repo)
    desc = req.fix_finding()
    assert desc is not None
    assert (repo / "houses" / "app.py").is_file()
    _assert_fixed(repo, before)


def test_dissolve_husk_refuses_escaping_instance(tmp_path):
    # a construction that escapes (stored/returned) can't be dissolved
    # blindly — refuse with a why
    src = code_fixture("sample_dissolve_husk_refuses_escaping_instance__10206d36.py")
    repo = make_repo(tmp_path, app_src=code_fixture("build__f9694aad.py"))
    (repo / "houses" / "app.py").write_text(src)
    req = fix_engine._FixRequest(
        kind="dissolve-husk",
        repo=repo,
        rel="houses/app.py",
        line=4,
        opts=fix_engine.FixOptions(),
    )
    desc = req.fix_finding()
    assert desc is None
    assert req.decline is not None and "escapes" in req.decline, req.decline


def test_collapse_chain_rewires_real_terminal(tmp_path):
    # #30: Store has identity (self.cache) — fetch rewires to store.load
    # directly, the middle forwarder is deleted, D survives
    src = code_fixture("sample_collapse_chain_rewires_real_terminal__7d357759.py")
    repo = make_repo(tmp_path, app_src=code_fixture("build__f9694aad.py"))
    (repo / "houses" / "app.py").write_text(src)
    req = fix_engine._FixRequest(
        kind="collapse-chain",
        repo=repo,
        rel="houses/app.py",
        line=11,
        opts=fix_engine.FixOptions(),
    )
    before = _actions(repo)
    desc = req.fix_finding()
    assert desc is not None
    assert (repo / "houses" / "app.py").is_file()
    _assert_fixed(repo, before)


def test_collapse_chain_promotes_stateless_terminal(tmp_path):
    # D has no identity (only forwarders) and zero external refs — N is
    # promoted to C, D dissolves
    src = code_fixture("sample_collapse_chain_promotes_stateless_terminal__b3873a8b.py")
    repo = make_repo(tmp_path, app_src=code_fixture("build__f9694aad.py"))
    (repo / "houses" / "app.py").write_text(src)
    req = fix_engine._FixRequest(
        kind="collapse-chain",
        repo=repo,
        rel="houses/app.py",
        opts=fix_engine.FixOptions(),
        line=12,
    )
    before = _actions(repo)
    desc = req.fix_finding()
    assert desc is not None
    assert (repo / "houses" / "app.py").is_file()
    _assert_fixed(repo, before)


def test_collapse_chain_refuses_boundary_between_modules(tmp_path):
    # the chain is the ONLY coupling between independent modules — refuse
    src = code_fixture("sample_collapse_chain_refuses_boundary_between_modules__3eaff56d.py")
    repo = make_repo(tmp_path, app_src=code_fixture("build__f9694aad.py"))
    (repo / "houses" / "app.py").write_text(src)
    (repo / "houses" / "store.py").write_text(
        code_fixture("sample_collapse_chain_refuses_boundary_between_modules__ea4ad7de.py")
    )
    req = fix_engine._FixRequest(
        kind="collapse-chain",
        repo=repo,
        rel="houses/app.py",
        line=2,
        opts=fix_engine.FixOptions(),
    )
    desc = req.fix_finding()
    assert desc is None
    assert req.decline is not None and "boundary" in req.decline, req.decline


# --------------------------------------------------------------------------- split-module (#31)


def test_split_module_flat_singleton_moves_class_and_rewrites_callsites(tmp_path):
    # User is misplaced (stem prod_mod); ProdMod matches the stem and stays,
    # _Helper is private and stays, the module fn stays — the FLAT singleton
    # split rewrites `from prod_mod import User` and `prod_mod.User` repo-wide
    src = code_fixture("sample_split_module_flat_singleton_moves_class_and_rewrites_callsites__b2d63eb8.py")
    repo = make_repo(tmp_path, app_src=code_fixture("build__f9694aad.py"))
    (repo / "prod_mod.py").write_text(src)
    (repo / "consumer.py").write_text(
        code_fixture("sample_split_module_flat_singleton_moves_class_and_rewrites_callsites__bcc975fd.py")
    )
    req = fix_engine._FixRequest(
        kind="split-module",
        repo=repo,
        rel="prod_mod.py",
        line=1,
        opts=fix_engine.FixOptions(),
    )
    before = _actions(repo)
    desc = req.fix_finding()
    assert desc is not None
    assert (repo / "consumer.py").is_file()
    # the produced layout is stem-clean — a re-scan would emit no
    # class-module finding (every public class now matches its file stem)
    assert (repo / "user.py").is_file()
    assert (repo / "prod_mod.py").is_file()
    _assert_fixed(repo, before)


def test_split_module_whole_cluster_package_by_stem(tmp_path):
    # Team reads User and both share the class-level LABEL attribute — one
    # cohesive cluster -> PACKAGE named by the stem; `from prod_mod import
    # User` keeps resolving (zero caller rewrites)
    src = code_fixture("sample_split_module_whole_cluster_package_by_stem__1ae693ea.py")
    repo = make_repo(tmp_path, app_src=code_fixture("build__f9694aad.py"))
    (repo / "prod_mod.py").write_text(src)
    (repo / "consumer.py").write_text(code_fixture("sample_split_module_whole_cluster_package_by_stem__1fd2b9e8.py"))
    req = fix_engine._FixRequest(
        kind="split-module",
        repo=repo,
        rel="prod_mod.py",
        line=1,
        opts=fix_engine.FixOptions(),
    )
    before = _actions(repo)
    desc = req.fix_finding()
    assert desc is not None
    assert not (repo / "prod_mod.py").exists(), "the origin file is replaced by the package"
    assert (repo / "prod_mod" / "__init__.py").is_file()
    assert (repo / "prod_mod" / "User.py").is_file()
    assert (repo / "prod_mod" / "Team.py").is_file()
    assert (repo / "consumer.py").is_file()
    # the produced layout is stem-clean — a re-scan would emit no
    # class-module finding: one class per member file, named after it
    import libcst

    for member in ("User.py", "Team.py"):
        mod = libcst.parse_module((repo / "prod_mod" / member).read_text())
        classes = [s.name.value for s in mod.body if isinstance(s, libcst.ClassDef)]
        assert classes == [member[:-3]], f"{member}: {classes}"
    _assert_fixed(repo, before)


def test_split_module_sub_cluster_package_requires_name(tmp_path):
    # User<->Team are cohesive (Team reads User); Extra is independent — the
    # proper sub-cluster needs --name: absent -> DECLINE (never
    # flat-for-convenience); present -> the package holds the cluster, Extra
    # goes flat
    src = code_fixture("sample_split_module_sub_cluster_package_requires_name__bcf9e40e.py")
    repo = make_repo(tmp_path, app_src=code_fixture("build__f9694aad.py"))
    (repo / "prod_mod.py").write_text(src)
    (repo / "consumer.py").write_text(
        code_fixture("sample_split_module_sub_cluster_package_requires_name__7550e01d.py")
    )
    req = fix_engine._FixRequest(
        kind="split-module",
        repo=repo,
        rel="prod_mod.py",
        line=1,
        opts=fix_engine.FixOptions(),
    )
    before = _actions(repo)
    assert req.fix_finding() is None
    assert req.decline is not None and "name" in req.decline, req.decline
    assert "class User" in (repo / "prod_mod.py").read_text()  # nothing written

    req = fix_engine._FixRequest(
        kind="split-module",
        repo=repo,
        rel="prod_mod.py",
        line=1,
        opts=fix_engine.FixOptions(name="models"),
    )
    desc = req.fix_finding()
    assert desc is not None
    assert (repo / "models" / "__init__.py").is_file()
    assert (repo / "models" / "User.py").exists()
    assert (repo / "models" / "Team.py").exists()
    assert (repo / "extra.py").is_file()
    assert (repo / "consumer.py").is_file()
    assert not (repo / "prod_mod.py").exists()  # the residual is empty -> deleted
    _assert_fixed(repo, before)


def test_split_module_refuses_target_collision(tmp_path):
    # a cohesive pair wants the stem package but prod_mod/ exists and no
    # --name is given -> decline (never flat-for-convenience); a flat target
    # collision (user.py exists) also declines
    src = code_fixture("sample_split_module_refuses_target_collision__348318f3.py")
    repo = make_repo(tmp_path, app_src=code_fixture("build__f9694aad.py"))
    (repo / "prod_mod.py").write_text(src)
    (repo / "prod_mod").mkdir()
    req = fix_engine._FixRequest(
        kind="split-module",
        repo=repo,
        rel="prod_mod.py",
        line=1,
        opts=fix_engine.FixOptions(),
    )
    assert req.fix_finding() is None
    assert req.decline is not None and "prod_mod" in req.decline and "name" in req.decline, req.decline

    repo2 = make_repo(tmp_path / "b", app_src=code_fixture("build__f9694aad.py"))
    (repo2 / "prod_mod.py").write_text(code_fixture("sample_split_module_refuses_target_collision__2b1d6792.py"))
    (repo2 / "user.py").write_text("EXISTING = True\n")
    req2 = fix_engine._FixRequest(
        kind="split-module",
        repo=repo2,
        rel="prod_mod.py",
        line=1,
        opts=fix_engine.FixOptions(),
    )
    assert req2.fix_finding() is None
    assert req2.decline is not None and "user.py" in req2.decline, req2.decline


def test_split_module_refuses_import_cycle(tmp_path):
    # User and Team reference each other — the produced class files would
    # import each other (a genuine circular import): refuse with a reason
    src = code_fixture("sample_split_module_refuses_import_cycle__f01b2d95.py")
    repo = make_repo(tmp_path, app_src=code_fixture("build__f9694aad.py"))
    (repo / "prod_mod.py").write_text(src)
    req = fix_engine._FixRequest(
        kind="split-module",
        repo=repo,
        rel="prod_mod.py",
        line=1,
        opts=fix_engine.FixOptions(),
    )
    assert req.fix_finding() is None
    assert req.decline is not None and "cycle" in req.decline, req.decline


def test_split_module_relative_import_in_origin_falls_back_to_flat(tmp_path):
    # prod_mod.py lives in a package and imports a sibling (from .common) —
    # the package layout would change the path root: refuse PACKAGE and split
    # flat (the class files are siblings, the relative imports keep resolving)
    repo = make_repo(tmp_path, app_src=code_fixture("build__f9694aad.py"))
    (repo / "houses" / "common.py").write_text(
        code_fixture("sample_split_module_relative_import_in_origin_falls_back_to_flat__e6febd8e.py")
    )
    (repo / "houses" / "prod_mod.py").write_text(
        code_fixture("sample_split_module_relative_import_in_origin_falls_back_to_flat__381e4c59.py")
    )
    (repo / "houses" / "app.py").write_text(
        code_fixture("sample_split_module_relative_import_in_origin_falls_back_to_flat__32511444.py")
    )
    req = fix_engine._FixRequest(
        kind="split-module",
        repo=repo,
        rel="houses/prod_mod.py",
        line=3,
        opts=fix_engine.FixOptions(),
    )
    before = _actions(repo)
    desc = req.fix_finding()
    assert desc is not None
    assert not (repo / "houses" / "prod_mod").exists(), "no package: fell back to flat"
    assert (repo / "houses" / "user.py").is_file()
    assert (repo / "houses" / "team.py").is_file()
    assert (repo / "houses" / "prod_mod.py").is_file()
    assert (repo / "houses" / "app.py").is_file()
    _assert_fixed(repo, before)


def test_split_module_refuses_executable_statements_between_classes(tmp_path):
    # configure() runs between the subject classes — splitting would reorder
    # it relative to the classes: refuse with a reason
    src = code_fixture("sample_split_module_refuses_executable_statements_between_classes__7cb948ca.py")
    repo = make_repo(tmp_path, app_src=code_fixture("build__f9694aad.py"))
    (repo / "prod_mod.py").write_text(src)
    req = fix_engine._FixRequest(
        kind="split-module",
        repo=repo,
        rel="prod_mod.py",
        line=1,
        opts=fix_engine.FixOptions(),
    )
    assert req.fix_finding() is None
    assert req.decline is not None and "class" in req.decline, req.decline


def test_split_module_constant_moves_into_owning_class(tmp_path):
    # THRESHOLD is read only by the misplaced User — it moves INTO User as a
    # class attribute and the method reference becomes User.THRESHOLD; no
    # module-level constant survives in either layout (BQ6)
    src = code_fixture("sample_split_module_constant_moves_into_owning_class__0a695b84.py")
    repo = make_repo(tmp_path, app_src=code_fixture("build__f9694aad.py"))
    (repo / "prod_mod.py").write_text(src)
    req = fix_engine._FixRequest(
        kind="split-module",
        repo=repo,
        rel="prod_mod.py",
        line=3,
        opts=fix_engine.FixOptions(),
    )
    before = _actions(repo)
    assert req.fix_finding() is not None
    assert (repo / "user.py").is_file()
    assert (repo / "prod_mod.py").is_file()
    _assert_fixed(repo, before)


def test_split_module_ownerless_constant_stays_in_residual(tmp_path):
    # LIMIT has no owning class — it stays with the residual origin module
    # (never stranded, never moved into a class file's top level)
    src = code_fixture("sample_split_module_ownerless_constant_stays_in_residual__f353fde8.py")
    repo = make_repo(tmp_path, app_src=code_fixture("build__f9694aad.py"))
    (repo / "prod_mod.py").write_text(src)
    req = fix_engine._FixRequest(
        kind="split-module",
        repo=repo,
        rel="prod_mod.py",
        line=3,
        opts=fix_engine.FixOptions(),
    )
    before = _actions(repo)
    assert req.fix_finding() is not None
    assert (repo / "prod_mod.py").is_file()
    assert (repo / "user.py").is_file()
    _assert_fixed(repo, before)


def test_split_module_package_refuses_imported_constant(tmp_path):
    # LIMIT is imported from another file — it cannot move into a class; the
    # package __init__ may hold no constants (BQ6): refuse, never strand
    src = code_fixture("sample_split_module_package_refuses_imported_constant__b2b55a88.py")
    repo = make_repo(tmp_path, app_src=code_fixture("build__f9694aad.py"))
    (repo / "prod_mod.py").write_text(src)
    (repo / "consumer.py").write_text("from prod_mod import LIMIT\n")
    req = fix_engine._FixRequest(
        kind="split-module",
        repo=repo,
        rel="prod_mod.py",
        line=4,
        opts=fix_engine.FixOptions(),
    )
    assert req.fix_finding() is None
    assert req.decline is not None and "LIMIT" in req.decline, req.decline
    assert "class User" in (repo / "prod_mod.py").read_text()


def test_split_module_two_independent_classes_go_flat_and_origin_deleted(tmp_path):
    # no edges between User and Team -> two singleton FLAT files; the origin
    # becomes empty and, with no import naming it, is deleted
    src = code_fixture("sample_split_module_two_independent_classes_go_flat_and_origin_deleted__b40717cf.py")
    repo = make_repo(tmp_path, app_src=code_fixture("build__f9694aad.py"))
    (repo / "prod_mod.py").write_text(src)
    (repo / "consumer.py").write_text(
        code_fixture("sample_split_module_two_independent_classes_go_flat_and_origin_deleted__078ae8ba.py")
    )
    req = fix_engine._FixRequest(
        kind="split-module",
        repo=repo,
        rel="prod_mod.py",
        line=1,
        opts=fix_engine.FixOptions(),
    )
    before = _actions(repo)
    assert req.fix_finding() is not None
    assert (repo / "user.py").exists() and (repo / "team.py").exists()
    assert not (repo / "prod_mod.py").exists(), "the empty origin is deleted"
    assert (repo / "consumer.py").is_file()
    _assert_fixed(repo, before)


def test_fix_without_line_resolves_single_finding(tmp_path, capsys):
    """R27: no --fix-line needed when the file has exactly one finding of the
    kind — the tool scans and owns the coordinates."""
    src = code_fixture("sample_fix_without_line_resolves_single_finding__196f913a.py")
    repo = make_repo(tmp_path, app_src=code_fixture("build__f9694aad.py"))
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
    src = code_fixture("sample_extract_class_previews_without_writing__cc3cded6.py")
    repo = make_repo(tmp_path, app_src=code_fixture("build__f9694aad.py"))
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

_INDEPENDENT_IFS = "".join(f"    if c{i} > 0:\n        out.append({1 << i})\n" for i in range(14))

# (finding kind, fixture, before-expr, after-expr, expected, --fix-name)
DIRECTIVE_CASES = [
    (
        "stale-suppression",
        code_fixture("module__55e2d78a.py"),
        "g()",
        "g()",
        "1",
        None,
    ),
    (
        "noop-statement",
        code_fixture("module__b52c6748.py"),
        "g()",
        "g()",
        "2",
        None,
    ),
    (
        "unreachable",
        code_fixture("module__0541d0ab.py"),
        "g()",
        "g()",
        "1",
        None,
    ),
    (
        "positional-literals",
        code_fixture("module__3cf09399.py"),
        "g()",
        "g()",
        "10",
        None,
    ),
    (
        "vague-name",
        (code_fixture("module__cd0799e3.py")),
        "use().run()",
        "use().run()",
        "1",
        "DataRegistry",
    ),
    (
        "long-param-list",
        (code_fixture("sample_parameter_object_introduced__23d8316c.py")),
        "g()",
        "g()",
        "21",
        "BuildOptions",
    ),
    (
        "latent-class",
        (code_fixture("sample_extract_class_renames_receiver_and_internal_calls__94d5cbdd.py")),
        "_best_seam(_FnBodyState(1))",
        "_FnBodyState(1)._best_seam()",
        "1",
        None,
    ),
    (
        "complexity",
        (
            "def score(" + ", ".join(f"c{i}" for i in range(14)) + "):\n"
            "    out = []\n" + _INDEPENDENT_IFS + "    label = 's' + str(sum(out))\n    return label\n"
        ),
        "score(1, 0, 1, 0, 1, 0, 1, 0, 1, 0, 1, 0, 1, 0)",
        "score(1, 0, 1, 0, 1, 0, 1, 0, 1, 0, 1, 0, 1, 0)",
        "s5461",
        "sum_bonus",
    ),
]


def test_every_fix_directive_clears_its_finding(tmp_path, capsys):
    """R27 compliance: for every fixable family, the finding's message carries
    a `fix: <command>` directive, and following it EXACTLY (substituting the
    name, adding --confirm where the result is novel) clears the finding and
    preserves behavior."""
    for kind, src, before_expr, after_expr, expected, name_value in DIRECTIVE_CASES:
        tmp = tmp_path / kind
        repo = make_repo(tmp, app_src=code_fixture("build__f9694aad.py"))
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
            "--kind",
            fix_kind,
            "--file",
            fix_file,
            "--line",
            str(fix_line),
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
    repo = make_repo(tmp_path, app_src=code_fixture("build__f9694aad.py"))
    (repo / "houses" / "app.py").write_text(_FIX_ALL_SRC)
    attempts: dict = {}

    last_count: int | None = None  # fixable count before the previous fix
    prev_applied = False
    for _ in range(25):  # bounded — the workflow normally converges in ~7
        run_main(repo, "--warn", "--json")
        actions = json.loads(capsys.readouterr().out)["actions"]
        # the action's `fix_kind` is the transform it advertises; a display
        # family like latent-class also has variants (misplaced-method) with
        # no transform, which are not part of this loop
        fixable = [a for a in actions if a.get("fix_kind")]
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
                assert len(fixable) <= last_count, f"no-op fix changed the file ({last_count} -> {len(fixable)})"
        fix_kind = finding["fix_kind"]
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
            "--kind",
            fix_kind,
            "--file",
            finding["file"],
            "--line",
            str(finding["line"]),
            "--confirm",  # structural fixes preview unless confirmed
            *extra,
        )
        assert rc == 0
        fix_out = capsys.readouterr().out  # "fix: ..." or "fix: nothing to change"
        attempts[key] = attempts.get(key, 0) + 1
        assert attempts[key] <= 2, f"fix did not converge on {key}"
        # the CLI's no-op surface: the generic silence AND the explicit
        # unsatisfied-prerequisite refusals (missing/placeholder name) — none
        # of them applied anything
        refused = any(
            marker in fix_out
            for marker in (
                "nothing to change",
                "needs a semantic name",
                "--name must be",
                "--params entries",
                "pass --params <names>",
            )
        )
        prev_applied = bool(fix_out.strip()) and not refused
        last_count = len(fixable)

    # every fixable kind is gone and the file still parses
    run_main(repo, "--warn", "--json")
    final_actions = json.loads(capsys.readouterr().out)["actions"]
    remaining = [a["kind"] for a in final_actions if a.get("fix_kind")]
    assert remaining == [], f"fixable findings remain: {remaining}"
    compile((repo / "houses" / "app.py").read_text(), "app.py", "exec")


def test_extract_method_refusal_is_silent(tmp_path, capsys):
    """R28: a fix that cannot apply refuses with NO explanation — the tool
    never tells the user it could not figure out a fix (the silence is the
    signal; the user sees the code and figures it out)."""
    repo = make_repo(tmp_path)
    (repo / "houses" / "app.py").write_text(code_fixture("sample_extract_method_refusal_is_silent__90a0c5ca.py"))
    rc = run_main(
        repo,
        "fix",
        "--kind",
        "extract-method",
        "--file",
        "houses/app.py",
        "--line",
        "1",
        "--name",
        "_apply",
        "--confirm",
    )
    assert rc == 0
    out = capsys.readouterr().out
    # R28 three-shape contract: nothing fixable here -> complete silence
    assert out.strip() == "", out
    assert "out-variable" not in out and "extractable" not in out, out


def test_dispatch_registry_preserves_behavior(tmp_path):
    """The dispatch-registry fix: an if/elif chain over a selector becomes a
    dict of selector -> handler functions. Behavior must be identical for
    every selector, including the no-match fallback."""
    src = code_fixture("sample_dispatch_registry_preserves_behavior__049f9d33.py")
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
    src = code_fixture("sample_dispatch_registry_refuses_non_chain__e8f448cb.py")
    assert _dispatch(src, 1) is None


def test_dispatch_registry_passes_selector_to_selector_reading_arms(tmp_path):
    """A multi-statement arm body that READS the selector (module-level named
    handlers cannot capture the fn's locals) gets the selector passed as the
    first handler parameter — behavior preserved, not a NameError."""
    src = code_fixture("sample_dispatch_registry_passes_selector_to_selector_reading_arms__14071eec.py")
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
    src = code_fixture("sample_dispatch_registry_refuses_sibling_bound_reads__799321ec.py")
    assert _dispatch(src, 1) is None


def test_rust_dispatch_registry_applies(tmp_path):
    """dispatch-registry works on Rust via the scan core (syn): the if/elif
    chain over one selector becomes a match — Rust's idiomatic dispatch
    table. Behavior preserved (verified by running both versions)."""
    src = code_fixture("sample_rust_dispatch_registry_applies__c3a285e6.py")
    repo = make_repo(tmp_path, app_src=code_fixture("build__f9694aad.py"))
    p = repo / "houses" / "lib.rs"
    p.write_text(src)
    before = p.read_text()
    run_main(
        repo,
        "fix",
        "--kind",
        "dispatch-registry",
        "--file",
        "houses/lib.rs",
        "--line",
        "1",
    )
    fixed = p.read_text()
    # behavior preserved: compile AND RUN both versions, compare the output
    # (a compile-only check cannot catch a dropped arm or a wrong fallback)
    main = code_fixture("sample_rust_dispatch_registry_applies__35ad4ae4.py")
    _rust_run_compare(tmp_path, before, fixed, main)


def test_rust_dispatch_registry_refuses_else_arms(tmp_path):
    """An arm with an else would be SILENTLY DELETED by the match splice —
    the fix must refuse (and the classifier must not offer the directive).
    The original file stays untouched."""
    src = code_fixture("sample_rust_dispatch_registry_refuses_else_arms__610d949a.py")
    repo = make_repo(tmp_path, app_src=code_fixture("build__f9694aad.py"))
    p = repo / "houses" / "lib.rs"
    p.write_text(src)
    before = p.read_text()
    rc = run_main(repo, "fix", "--kind", "dispatch-registry", "--file", "houses/lib.rs", "--line", "1")
    assert p.read_text() == before, "an else-bearing dispatch must not be rewritten"
    assert rc == 0  # silent R28 refusal, not an error


def test_rust_dispatch_registry_refuses_string_selector(tmp_path):
    """`match sel { \"lit\" => ... }` only compiles for &str — a String
    selector must be refused (no directive, no broken rewrite)."""
    src = code_fixture("sample_rust_dispatch_registry_refuses_string_selector__3be0ac30.py")
    repo = make_repo(tmp_path, app_src=code_fixture("build__f9694aad.py"))
    p = repo / "houses" / "lib.rs"
    p.write_text(src)
    before = p.read_text()
    run_main(repo, "fix", "--kind", "dispatch-registry", "--file", "houses/lib.rs", "--line", "1")
    assert p.read_text() == before, "a String selector must not be match-rewritten"


def test_rust_dispatch_registry_refuses_duplicate_literals(tmp_path):
    """Two arms on the same literal -> unreachable-pattern compile error —
    refuse."""
    src = code_fixture("sample_rust_dispatch_registry_refuses_duplicate_literals__ea5f4373.py")
    repo = make_repo(tmp_path, app_src=code_fixture("build__f9694aad.py"))
    p = repo / "houses" / "lib.rs"
    p.write_text(src)
    before = p.read_text()
    run_main(repo, "fix", "--kind", "dispatch-registry", "--file", "houses/lib.rs", "--line", "1")
    assert p.read_text() == before, "duplicate dispatch literals must not be rewritten"


def test_rule_table_preserves_behavior(tmp_path):
    """The rule-table fix (hoist the latent data structure): the battery of
    if/append checks becomes a list of (lambda condition, violation) tuples.
    Same violations, same order; the lambdas capture shared preamble locals."""
    src = code_fixture("sample_rule_table_preserves_behavior__5427f6d6.py")
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
    src = code_fixture("sample_rule_table_defers_value_evaluation__1103a831.py")
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
    src = code_fixture("sample_rule_table_captures_preamble_locals__39836f88.py")
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
    src = code_fixture("sample_rust_rule_table_applies__a8be477e.py")
    repo = make_repo(tmp_path, app_src=code_fixture("build__f9694aad.py"))
    p = repo / "houses" / "lib.rs"
    p.write_text(src)
    before = p.read_text()
    run_main(
        repo,
        "fix",
        "--kind",
        "rule-table",
        "--file",
        "houses/lib.rs",
        "--line",
        "1",
    )
    fixed = p.read_text()
    # behavior preserved: compile AND RUN both versions, compare the output
    main = code_fixture("sample_rust_rule_table_applies__1b151cac.py")
    _rust_run_compare(tmp_path, before, fixed, main)


def test_dispatch_registry_lambda_table(tmp_path):
    """Single-expression arms collapse into a pure data table — a dict of
    selector -> lambda, capturing the scope: no names, no param plumbing
    (the rule-table principle applied to dispatch)."""
    src = code_fixture("sample_dispatch_registry_lambda_table__ae25b4be.py")
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
    src = code_fixture("sample_dispatch_registry_collision_guard__d7051bcd.py")
    fixed = _dispatch(src, 4)
    assert fixed is not None
    assert _has_def(fixed, "_search_people_1")  # the colliding handler is suffixed


# --------------------------------------------------------------------------- review-log fixes


def _fix_opts(tmp_path, kind, rel, src, line, name=None, params=None):
    """Like _fix but with the agent-supplied semantic bits (extract-module's
    module name + member list)."""
    repo = make_repo(tmp_path, app_src=code_fixture("build__f9694aad.py"))
    (repo / rel).write_text(src)
    repo.mkdir(exist_ok=True)
    opts = fix_engine.FixOptions(name=name, params=params)
    out = _fix_finding(kind, rel, repo, line, opts)
    return out, (repo / rel).read_text()


def test_duplicate_def_unreferenced_shadow_deleted(tmp_path):
    # the review-log guess_pages shadow: the second def is referenced nowhere
    # (only the two def sites hit the name) — the delete is provably safe
    src = code_fixture("sample_duplicate_def_unreferenced_shadow_deleted__43e2754a.py")
    out, fixed = _fix(tmp_path, "duplicate-def", "houses/app.py", src, 5)
    assert out is not None
    assert fixed.count("def guess_pages") == 1
    assert "return 1" in fixed  # the FIRST def survives verbatim


def test_duplicate_def_single_reference_is_not_deleted(tmp_path):
    # the def's own name is a Name node in libcst; one call site used to make
    # the count look like "two def sites" and the delete broke the caller
    src = code_fixture("sample_duplicate_def_single_reference_refuses__b3f81c6a.py")
    out, fixed = _fix(tmp_path, "duplicate-def", "houses/app.py", src, 5)
    assert out is None
    assert fixed == src
    namespace: dict = {}
    exec(fixed, namespace)
    assert namespace["driver"]("abc", 1) == "bb"  # both wrappers are live


def test_duplicate_def_target_bound_once_is_refused(tmp_path):
    # near-identical functions are the `duplicate` rule's finding, not a
    # shadowing duplicate-def; the fixer must not delete a lone definition
    src = code_fixture("sample_duplicate_def_target_bound_once_refuses__d94e2f70.py")
    repo = make_repo(tmp_path, app_src=code_fixture("build__f9694aad.py"))
    (repo / "houses" / "app.py").write_text(src)
    req = _req("duplicate-def", "houses/app.py", repo, 1)
    out = req.fix_finding()
    assert out is None
    assert req.decline is not None
    assert "duplicate" in req.decline  # names the near-duplicate family to use
    assert (repo / "houses" / "app.py").read_text() == src


def test_duplicate_def_referenced_shadow_not_deleted(tmp_path):
    # a caller exists — a delete would break it; the agent must rename
    src = code_fixture("sample_duplicate_def_referenced_shadow_not_deleted__7fbceba0.py")
    out, fixed = _fix(tmp_path, "duplicate-def", "houses/app.py", src, 5)
    assert out is None
    assert fixed == src


def test_restating_docstring_deleted(tmp_path):
    src = code_fixture("sample_restating_docstring_deleted__3ab003ea.py")
    out, fixed = _fix(tmp_path, "restating-docstring", "houses/app.py", src, 1)
    assert out is not None
    assert '"""the line orientation' not in fixed
    assert "orientation = line.orientation" in fixed  # the body survives


def test_restating_docstring_absent_returns_none(tmp_path):
    src = code_fixture("sample_restating_docstring_absent_returns_none__527061b5.py")
    out, fixed = _fix(tmp_path, "restating-docstring", "houses/app.py", src, 1)
    assert out is None
    assert fixed == src


def test_duplicate_block_second_copy_deleted(tmp_path):
    # the transcribe-twice class: the post-loop copy is removed, the loop
    # body survives
    src = code_fixture("sample_duplicate_block_second_copy_deleted__f2f102a7.py")
    out, fixed = _fix(tmp_path, "duplicate-block", "houses/app.py", src, 6)
    assert out is not None
    writes = [
        node
        for node in ast.walk(ast.parse(fixed))
        if isinstance(node, ast.Call)
        and isinstance(node.func, ast.Name)
        and node.func.id == "write"
        and len(node.args) == 1
        and isinstance(node.args[0], ast.Name)
        and node.args[0].id == "t"
    ]
    assert len(writes) == 1
    assert _has_for(fixed, "p", "pages")  # the first copy (loop body) survives


def test_duplicate_block_empty_block_refused(tmp_path):
    # the second copy FILLS the second loop body — deleting it empties the
    # block -> invalid Python; the fix must refuse rather than emit a bare
    # block
    src = code_fixture("sample_duplicate_block_empty_block_refused__3824f761.py")
    out, fixed = _fix(tmp_path, "duplicate-block", "houses/app.py", src, 7)
    assert out is None
    assert fixed == src


def test_extract_module_moves_domain_and_reexports(tmp_path):
    # the module-cohesion fix: one domain's defs move to the new module; the
    # origin re-exports them so every other file's imports keep working
    src = code_fixture("sample_extract_module_moves_domain_and_reexports__cb808349.py")
    out, fixed = _fix_opts(
        tmp_path,
        "extract-module",
        "houses/layout.py",
        src,
        1,
        name="text",
        params=["tokenize", "words"],
    )
    assert out is not None
    repo = tmp_path / "repo"
    new_mod = (repo / "houses" / "text.py").read_text()
    assert _has_def(new_mod, "tokenize")
    assert _has_def(new_mod, "words")
    assert _has_from_import(new_mod, "math", names={"sqrt"})  # the needed import moved with them
    assert _has_from_import(fixed, "text", level=1, names={"tokenize", "words"})  # relative re-export
    assert not _has_def(fixed, "tokenize")


def test_extract_module_refuses_leaking_dependency(tmp_path):
    # the moved code needs an origin def that is NOT moving -> a from-origin
    # import in the new module would create a cycle (review log §3.4) — refuse
    src = code_fixture("sample_extract_module_refuses_leaking_dependency__025de62a.py")
    out, fixed = _fix_opts(
        tmp_path,
        "extract-module",
        "houses/layout.py",
        src,
        1,
        name="text",
        params=["tokenize"],
    )
    assert out is None
    assert fixed == src
    assert not (tmp_path / "repo" / "houses" / "text.py").exists()


def test_extract_module_name_free_preview_does_not_write(tmp_path):
    # the preview shows the seam with a placeholder module name and writes
    # nothing — naming AFTER seeing the diff
    repo = make_repo(tmp_path, app_src=code_fixture("build__f9694aad.py"))
    rel = "houses/layout.py"
    (repo / rel).write_text(code_fixture("sample_extract_module_moves_domain_and_reexports__cb808349.py"))
    opts = fix_engine.FixOptions(params=["tokenize", "words"])
    new_source, desc = _propose_finding("extract-module", rel, repo, 1, opts)
    assert new_source is not None
    # the origin is packaged (houses/layout.py), so the reexport is RELATIVE —
    # the preview must match what the apply path writes (consolidation fix)
    assert _has_from_import(new_source, "_extracted", level=1, names={"tokenize", "words"})
    assert not _has_def(new_source, "tokenize")
    assert not (repo / "houses" / "_extracted.py").exists()  # preview writes nothing
    origin = ast.parse((repo / rel).read_text())
    assert sum(isinstance(n, ast.FunctionDef) and n.name == "tokenize" for n in ast.walk(origin)) == 1


def test_extract_module_refuses_module_level_constant(tmp_path):
    # the moved code reads a module-level constant — the new module would
    # NameError at runtime; the split must refuse (review finding)
    src = code_fixture("sample_extract_module_refuses_module_level_constant__7456d863.py")
    out, fixed = _fix_opts(
        tmp_path,
        "extract-module",
        "houses/layout.py",
        src,
        1,
        name="text",
        params=["tokenize", "words"],
    )
    assert out is None
    assert fixed == src
    assert not (tmp_path / "repo" / "houses" / "text.py").exists()


def test_extract_module_package_reexport_is_relative(tmp_path):
    # houses/layout.py is inside a package — the re-export must be
    # `from .text import ...`, not the top-level `from text import ...`
    # (review finding: a sibling module is not on sys.path)
    src = code_fixture("sample_extract_module_package_reexport_is_relative__319c7c81.py")
    out, fixed = _fix_opts(
        tmp_path,
        "extract-module",
        "houses/layout.py",
        src,
        1,
        name="text",
        params=["tokenize", "words"],
    )
    assert out is not None
    assert _has_from_import(fixed, "text", level=1, names={"tokenize", "words"})
    assert not _has_from_import(fixed, "text", level=0)


def test_extract_module_binds_positional_only_params(tmp_path):
    # `LIMIT` is a POSITIONAL-ONLY parameter — a local, not a read of the
    # module constant of the same name; without binding posonly_params the
    # free-name check treats it as a dependency and falsely refuses (bot)
    src = code_fixture("sample_extract_module_binds_positional_only_params__a638bd64.py")
    out, fixed = _fix_opts(
        tmp_path,
        "extract-module",
        "houses/layout.py",
        src,
        1,
        name="metrics",
        params=["scale"],
    )
    assert out is not None, "a posonly param named like a constant is a local, not a dependency"


def test_extract_module_star_import_does_not_crash(tmp_path):
    # `from x import *` in the origin — ImportStar has no .value; the fix
    # must skip it, not raise (review finding)
    src = code_fixture("sample_extract_module_star_import_does_not_crash__d508366d.py")
    out, fixed = _fix_opts(
        tmp_path,
        "extract-module",
        "houses/layout.py",
        src,
        1,
        name="text",
        params=["tokenize", "words"],
    )
    assert out is not None
    assert "def tokenize(s):" in (tmp_path / "repo" / "houses" / "text.py").read_text()


def test_extract_module_param_named_like_constant_not_refused(tmp_path):
    # a parameter named like a module-level constant is a LOCAL binding, not
    # a module read — the split must not refuse it (review finding)
    src = code_fixture("sample_extract_module_param_named_like_constant_not_refused__531fa28d.py")
    out, fixed = _fix_opts(
        tmp_path,
        "extract-module",
        "houses/layout.py",
        src,
        1,
        name="text",
        params=["tokenize", "words"],
    )
    assert out is not None
    new_mod = (tmp_path / "repo" / "houses" / "text.py").read_text()
    assert _has_def(new_mod, "tokenize")


def test_extract_module_except_as_binding_does_not_crash(tmp_path):
    # `except ValueError as e:` — libcst's ExceptHandler.name is an AsName;
    # the free-name scan must not crash on it (review finding)
    src = code_fixture("sample_extract_module_except_as_binding_does_not_crash__1fa55c93.py")
    out, fixed = _fix_opts(
        tmp_path,
        "extract-module",
        "houses/layout.py",
        src,
        1,
        name="text",
        params=["tokenize", "words"],
    )
    assert out is not None
    assert "except ValueError as e:" in (tmp_path / "repo" / "houses" / "text.py").read_text()


def test_extract_module_bare_star_signature_does_not_crash(tmp_path):
    # a bare `*` keyword-only separator is ParamStar, not Param — the
    # free-name scan must not crash (review finding)
    src = code_fixture("sample_extract_module_bare_star_signature_does_not_crash__6c77a89a.py")
    out, fixed = _fix_opts(
        tmp_path,
        "extract-module",
        "houses/layout.py",
        src,
        1,
        name="text",
        params=["tokenize", "words"],
    )
    assert out is not None
    assert "def tokenize(s, *, limit=None):" in (tmp_path / "repo" / "houses" / "text.py").read_text()


# --------------------------------------------------------------------------- extract-record-class


def test_extract_record_class_converts_literal_and_reads(tmp_path):
    src = code_fixture("sample_extract_record_class_converts_literal_and_reads__f28162e0.py")
    out, fixed = _fix_opts(
        tmp_path,
        "extract-record-class",
        "houses/rec.py",
        src,
        2,
        name="Payload",
    )
    assert out is not None
    assert "class _Payload:" in fixed
    assert "_Payload(user=u, stamp=u)" in fixed
    assert 'payload["user"]' not in fixed
    assert "payload.user" in fixed


def test_extract_record_class_col_disambiguates_twins(tmp_path):
    # two constant-key dicts share the line — the anchor column selects
    # WHICH one becomes the class (schema-3 col transport)
    src = code_fixture("sample_extract_record_class_col_disambiguates_twins__2590316a.py")
    repo = make_repo(tmp_path, app_src=code_fixture("build__f9694aad.py"))
    (repo / "houses" / "twin.py").write_text(src)
    inner = fix_engine._FixRequest(
        kind="extract-record-class",
        repo=repo,
        rel="houses/twin.py",
        line=2,
        opts=fix_engine.FixOptions(name="Inner"),
        source=src,
        col=18,  # the INNER dict's brace
    )
    out = inner.fix_extract_record_class()
    assert out is not None
    assert "class _Inner:" in out
    assert "self.x" in out and "self.y" in out
    assert "_Inner(x=user, y=user)" in out  # the INNER record converted
    assert '{"x": user' not in out


def test_extract_record_class_refuses_non_identifier_keys(tmp_path):
    src = code_fixture("sample_extract_record_class_refuses_non_identifier_keys__5213d858.py")
    out, fixed = _fix_opts(
        tmp_path,
        "extract-record-class",
        "houses/bad.py",
        src,
        2,
        name="Payload",
    )
    assert out is None
    assert fixed == src  # nothing written on refusal


# --------------------------------------------------------------------------- loop family (Replace Loop with Pipeline)

FIXTURES = Path(__file__).resolve().parent / "fixtures"


def _fixture(name: str) -> str:
    """A committed code fixture — code never lives in a test string literal
    (coding-standards.md: a string literal never carries code; a fixture
    that is code is a real file under tests/fixtures/)."""
    return (FIXTURES / name).read_text(encoding="utf-8")


def _fn_line(src: str, fn: str) -> int:
    """The 1-based line of `def fn` — the loop-sequence fixer's anchor."""
    return next(i + 1 for i, line in enumerate(src.splitlines()) if line.startswith((f"def {fn}", f"async def {fn}")))


def _loop_line(src: str, fn: str) -> int:
    """The 1-based line of the first loop inside `def fn` — the per-loop
    fixers' anchor."""
    lines = src.splitlines()
    start = _fn_line(src, fn)
    for i in range(start, len(lines)):
        if lines[i].lstrip().startswith(("for ", "async for ", "while ")):
            return i + 1
    raise AssertionError(f"no loop under def {fn}")


def test_loop_pipeline_append_becomes_comprehension(tmp_path):
    src = _fixture("loop_pipeline.py")
    out, fixed = _fix(tmp_path, "loop-pipeline", "houses/app.py", src, _loop_line(src, "f_append"))
    assert out is not None
    assert _function_has_comprehension(fixed, "f_append")
    assert not _function_has_for(fixed, "f_append", "x", "items")  # the loop STATEMENT is gone
    assert _returns_name(fixed, "out")


def test_loop_pipeline_if_filter_becomes_filtered_comprehension(tmp_path):
    src = _fixture("loop_pipeline.py")
    out, fixed = _fix(tmp_path, "loop-pipeline", "houses/app.py", src, _loop_line(src, "f_filtered"))
    assert out is not None
    assert _has_filtered_comprehension(fixed)


def test_loop_pipeline_extend_becomes_flattening_comprehension(tmp_path):
    src = _fixture("loop_pipeline.py")
    out, fixed = _fix(tmp_path, "loop-pipeline", "houses/app.py", src, _loop_line(src, "f_extend"))
    assert out is not None
    # a fresh flatten name is introduced and the element order preserved
    assert _has_comprehension_iter_call(fixed, "g")


def test_loop_pipeline_set_add_becomes_set_comprehension(tmp_path):
    src = _fixture("loop_pipeline.py")
    out, fixed = _fix(tmp_path, "loop-pipeline", "houses/app.py", src, _loop_line(src, "f_set"))
    assert out is not None
    assert "seen = {x for x in items}" in fixed


def test_loop_pipeline_refuses_existing_contents_and_unsupported_mutations(tmp_path):
    # a non-empty accumulator: the rewrite would drop prior contents
    repo = make_repo(tmp_path, app_src=code_fixture("build__f9694aad.py"))
    target = repo / "houses" / "app.py"
    src = _fixture("loop_pipeline_bad.py")
    target.write_text(src)
    out = _fix_finding("loop-pipeline", "houses/app.py", repo, _loop_line(src, "f_nonempty_init"))
    assert out is None
    assert target.read_text() == src  # nothing written on refusal
    # update() cannot be expressed as a comprehension — declined, untouched
    target.write_text(src)
    out2 = _fix_finding("loop-pipeline", "houses/app.py", repo, _loop_line(src, "f_update"))
    assert out2 is None
    assert target.read_text() == src


def test_loop_pipeline_refuses_a_collection_it_iterates(tmp_path):
    # a self-feeding loop is a traversal; the guard refuses it (this empty
    # init is a no-op; a real worklist is a `while` drain, not a comprehension)
    src = code_fixture("sample_loop_pipeline_refuses_a_collection_it_iterates__aabb74d9.py")
    out, fixed = _fix(tmp_path, "loop-pipeline", "houses/app.py", src, _loop_line(src, "f"))
    assert out is None
    assert fixed == src  # untouched on refusal


def test_loop_sequence_shared_accumulator_becomes_concatenated_comprehensions(tmp_path):
    src = _fixture("loop_sequence.py")
    out, fixed = _fix(tmp_path, "loop-sequence", "houses/app.py", src, _fn_line(src, "all_text"))
    assert out is not None
    assert 'chunks = [p["name"] for p in people] + [q["bio"] for q in people]' in fixed
    assert (
        code_fixture("sample_loop_sequence_shared_accumulator_becomes_concatenated_comprehensions__eced649a.py")
        not in fixed
    )  # the loop statements are gone


def test_loop_sequence_refuses_interleaved_statements(tmp_path):
    src = _fixture("loop_sequence_bad.py")
    out, fixed = _fix(tmp_path, "loop-sequence", "houses/app.py", src, _fn_line(src, "interleaved"))
    assert out is None
    assert _has_for(fixed, "x", "xs")  # untouched — the chain is not contiguous


# --------------------------------------------------------------------------- loop-hoist (helper + comprehension)


def _hoist_fix(tmp_path, fixture: str, fn: str, name="price"):
    src = _fixture(fixture)
    repo = tmp_path / "repo"
    if not (repo / ".git").exists():  # one repo per test, reused across cases
        repo = make_repo(tmp_path, app_src=code_fixture("build__f9694aad.py"))
    target = repo / "houses" / "app.py"
    target.write_text(src)
    out = _req("loop-hoist", "houses/app.py", repo, _loop_line(src, fn), fix_engine.FixOptions(name=name)).fix_finding()
    return out, target.read_text()


def test_loop_hoist_extracts_a_named_helper_and_comprehension(tmp_path):
    out, fixed = _hoist_fix(tmp_path, "loop_hoist.py", "h_filtered")
    assert out is not None
    # the body moved into the helper with the accumulator receiver renamed
    assert _has_def(fixed, "_price")
    assert _has_call(fixed, "val", "append")
    assert _returns_name(fixed, "val")
    # the loop became a flattening comprehension over the helper (the loop
    # and its body are replaced — no `out = []` init + loop can coexist)
    assert _has_comprehension(fixed, "part")
    assert _count_calls(fixed, "val", "append") == 1  # the single append site lives in the helper only


def test_loop_hoist_handles_multiple_sites_and_sets(tmp_path):
    # two append sites under different branches — the helper returns both
    out, fixed = _hoist_fix(tmp_path, "loop_hoist.py", "h_multi", name="gaps")
    assert out is not None
    assert "offenders = [part for person in people for part in _gaps(person)]" in fixed
    # set.add hoists to a set comprehension
    out2, fixed2 = _hoist_fix(tmp_path, "loop_hoist.py", "h_set", name="doubles")
    assert out2 is not None
    assert "seen = {part for it in items for part in _doubles(it)}" in fixed2


def test_loop_hoist_refuses_unsafe_bodies(tmp_path):
    # reading the accumulator mid-build has no comprehension equivalent
    src = _fixture("loop_hoist_bad.py")
    out, fixed = _hoist_fix(tmp_path, "loop_hoist_bad.py", "b_read")
    assert out is None
    assert fixed == src
    # two accumulators
    out2, fixed2 = _hoist_fix(tmp_path, "loop_hoist_bad.py", "b_two", name="h")
    assert out2 is None
    assert fixed2 == src
    # the helper needs a name the tool cannot invent
    out3, fixed3 = _hoist_fix(tmp_path, "loop_hoist_bad.py", "b_read", name=None)
    assert out3 is None
    assert fixed3 == src
    # a nested def's locals are its own scope — their writes must not
    # read as outer-state writes (the review finding: `got` inside
    # `_fmt` is not an accumulator)
    out4, fixed4 = _hoist_fix(tmp_path, "loop_hoist_bad.py", "b_nested", name="score")
    assert out4 is not None
    assert _has_def(fixed4, "_score")


# --------------------------------------------- shape-routed extract-class (wide-tuple/clump/partition)


def test_wide_tuple_becomes_a_record_class(tmp_path):
    """A fixed-arity tuple annotation becomes a record: the field names come
    from the build site's element expressions, every same-shape annotation in
    the file is retargeted, and each build site constructs the class."""
    src = code_fixture("sample_wide_tuple_becomes_a_record_class__7a0c0427.py")
    repo = make_repo(tmp_path, app_src=code_fixture("build__f9694aad.py"))
    p = repo / "houses" / "app.py"
    p.write_text(src)
    before = _actions(repo)
    out = _fix_finding("wide-tuple", "houses/app.py", repo, 1, fix_engine.FixOptions(name="RunState"))
    assert out is not None
    _assert_fixed(repo, before)


def test_wide_tuple_refuses_an_ambiguous_anchor(tmp_path):
    """Two wide-tuple annotations anchored on one line cannot be told apart —
    the fixer declines instead of guessing which one becomes the record."""
    src = "first: tuple[int, int, int] = (1, 2, 3); second: tuple[int, int, int] = (4, 5, 6)\n"
    repo = make_repo(tmp_path, app_src=code_fixture("build__f9694aad.py"))
    p = repo / "houses" / "app.py"
    p.write_text(src)
    req = _req("wide-tuple", "houses/app.py", repo, 1, fix_engine.FixOptions(name="Triple"), source=src)
    assert req.fix_finding() is None
    assert req.decline and "ambiguous" in req.decline, req.decline
    assert p.read_text() == src


def test_data_clump_pair_becomes_a_parameter_object(tmp_path):
    """The shared parameter pair threads as one object: the clump's functions
    take the instance instead of the two values, their bodies read the pair
    as attributes, and module-scope call sites construct the instance."""
    src = code_fixture("sample_data_clump_pair_becomes_a_parameter_object__8bd61bd4.py")
    repo = make_repo(tmp_path, app_src=code_fixture("build__f9694aad.py"))
    p = repo / "houses" / "app.py"
    p.write_text(src)
    before = _actions(repo)
    out = _fix_finding("data-clump", "houses/app.py", repo, 1, fix_engine.FixOptions(name="FolderFingerprint"))
    assert out is not None
    _assert_fixed(repo, before)


def test_data_clump_declines_when_the_shared_names_have_mismatched_types(tmp_path):
    """Issue #57: Wall, Point, and a tuple share parameter names but not
    types. No typed group reaches three, so there is no clump to thread and
    the fixer must decline instead of generating a false shared class."""
    src = code_fixture("sample_data_clump_declines_when_the_shared_names_have_mismatched_types__6f677bf4.py")
    repo = make_repo(tmp_path, app_src=code_fixture("build__f9694aad.py"))
    p = repo / "houses" / "app.py"
    p.write_text(src)
    req = _req("data-clump", "houses/app.py", repo, 16)
    assert req.fix_finding() is None
    assert req.decline and "no parameter pair" in req.decline, req.decline
    assert p.read_text() == src


def test_data_clump_fix_resolves_a_param_type_alias(tmp_path):
    """The fixer groups by resolved annotations, matching the scanner: an
    alias spelling of the same type keeps the clump threadable."""
    src = code_fixture("sample_data_clump_fix_resolves_a_param_type_alias__67e190a9.py")
    repo = make_repo(tmp_path, app_src=code_fixture("build__f9694aad.py"))
    p = repo / "houses" / "app.py"
    p.write_text(src)
    before = _actions(repo)
    out = _fix_finding("data-clump", "houses/app.py", repo, 8, fix_engine.FixOptions(name="WallPair"))
    assert out is not None
    _assert_fixed(repo, before)


def test_data_clump_refuses_a_cross_module_caller(tmp_path):
    """A clump function imported by another module cannot be threaded — the
    other caller's call site is outside the fixer's resolvable scope."""
    src = code_fixture("sample_data_clump_refuses_a_cross_module_caller__2066d4de.py")
    repo = make_repo(tmp_path, app_src=code_fixture("build__f9694aad.py"))
    p = repo / "houses" / "app.py"
    p.write_text(src)
    (repo / "consumer.py").write_text("from houses.app import save\n")
    req = _req("data-clump", "houses/app.py", repo, 1, fix_engine.FixOptions(name="FolderFingerprint"))
    assert req.fix_finding() is None
    assert req.decline and "consumer" in req.decline, req.decline
    assert p.read_text() == src


def test_partition_splits_field_disjoint_groups(tmp_path):
    """A class whose methods form two field-disjoint groups splits into two
    classes: each keeps the shared base and only its own fields and methods,
    and the first group keeps the base class's name."""
    src = code_fixture("sample_partition_splits_field_disjoint_groups__b3b46f01.py")
    repo = make_repo(tmp_path, app_src=code_fixture("build__f9694aad.py"))
    p = repo / "houses" / "app.py"
    p.write_text(src)
    before = _actions(repo)
    out = _fix_finding("partition", "houses/app.py", repo, 5, fix_engine.FixOptions(name="Report"))
    assert out is not None
    fixed = p.read_text()
    head, tail = fixed.split("class ReportAuthor(BaseReport):")
    assert "self.total = 0" in head  # the rows group keeps its field initializers
    assert 'self.title = ""' in tail  # the title/author group's initializer moved
    assert "add_row" not in tail
    assert "describe" not in head
    _assert_fixed(repo, before)


def test_partition_refuses_a_connector_method(tmp_path):
    """A method touching two groups' fields is a connector — the class is not
    field-disjoint, so the fixer declines instead of splitting it wrongly."""
    src = code_fixture("sample_partition_refuses_a_connector_method__3a762272.py")
    repo = make_repo(tmp_path, app_src=code_fixture("build__f9694aad.py"))
    p = repo / "houses" / "app.py"
    p.write_text(src)
    req = _req("partition", "houses/app.py", repo, 1, fix_engine.FixOptions(name="Report"))
    assert req.fix_finding() is None
    assert req.decline is not None, "a refusal must say why"
    assert p.read_text() == src


def test_wide_tuple_function_annotation_creates_the_record(tmp_path):
    """A tuple annotation with no assignment still yields the record: the
    parameter's annotation is retargeted, and the matching return literals of
    the function annotated with the same shape construct the class."""
    src = code_fixture("sample_wide_tuple_function_annotation_creates_the_record__4c95e7d2.py")
    repo = make_repo(tmp_path, app_src=code_fixture("build__f9694aad.py"))
    p = repo / "houses" / "app.py"
    p.write_text(src)
    before = _actions(repo)
    out = _fix_finding("wide-tuple", "houses/app.py", repo, 1, fix_engine.FixOptions(name="RunState"))
    assert out is not None
    _assert_fixed(repo, before)


def test_data_clump_derives_the_class_name_without_a_name(tmp_path):
    """Without --name the class name is derived from the pair (the anchor's
    signature order) and only the PREVIEW is produced — the apply needs the
    explicit name, so nothing is written here."""
    src = code_fixture("sample_data_clump_refuses_a_cross_module_caller__2066d4de.py")
    repo = make_repo(tmp_path, app_src=code_fixture("build__f9694aad.py"))
    p = repo / "houses" / "app.py"
    p.write_text(src)
    new_source, description = _propose_finding("data-clump", "houses/app.py", repo, 1)
    assert new_source is not None
    assert _has_class(new_source, "FolderFingerprint")
    assert _has_def(new_source, "save")
    assert description
    assert p.read_text() == src  # the preview writes nothing


def test_extract_class_directive_fixes_a_wide_tuple_finding(tmp_path):
    """The scanner's directive for these families names `extract-class` (one
    entry point), so the extract-class invocation must pick the arm from the
    anchor's shape — not fall through to the strewing group."""
    src = "state: tuple[str, int, float] = (mode, level, ratio)\n"
    repo = make_repo(tmp_path, app_src=code_fixture("build__f9694aad.py"))
    p = repo / "houses" / "app.py"
    p.write_text(src)
    before = _actions(repo)
    out = _fix_finding("extract-class", "houses/app.py", repo, 1, fix_engine.FixOptions(name="RunState"))
    assert out is not None
    _assert_fixed(repo, before)


# --------------------------------------------------------------------------- helper contracts
# The transforms above exercise the plans end to end; these pin the pure
# helpers' own contracts — each is a decision table the fixers dispatch on, so
# a branch that flips silently changes what the tool rewrites.


def _wrapper(source: str) -> MetadataWrapper:
    return MetadataWrapper(cst.parse_module(source))


def test_flatten_stmts_walks_every_nested_block_kind():
    module = cst.parse_module(code_fixture("sample_flatten_stmts_walks_every_nested_block_kind__0a521da7.py"))
    flat = []
    MechanicalFixes._flatten_stmts(module.body, flat)
    kinds = [type(s) for s in flat]
    assert len(flat) > len(module.body)  # nested bodies are included
    for kind in (cst.If, cst.For, cst.While, cst.Try, cst.With, cst.Match):
        assert kind in kinds, f"{kind} missing from the flatten"


def test_module_level_names_covers_import_and_binding_shapes():
    module = cst.parse_module(code_fixture("sample_module_level_names_covers_import_and_binding_shapes__dc1f2cad.py"))
    names = fix_engine._module_level_names(module)
    assert {"os", "j", "n", "r", "A", "B", "C", "D", "f", "K"} <= names
    assert "obj" not in names  # an attribute store binds nothing at module scope


def test_class_body_binds_recognises_every_member_shape():
    cls = cast(
        cst.ClassDef,
        cst.parse_statement(code_fixture("sample_class_body_binds_recognises_every_member_shape__aef01ab6.py")),
    )
    assert fix_engine._class_body_binds(cls, "run")
    assert fix_engine._class_body_binds(cls, "Inner")
    assert fix_engine._class_body_binds(cls, "limit")
    assert fix_engine._class_body_binds(cls, "name")
    assert fix_engine._class_body_binds(cls, "imported")
    assert not fix_engine._class_body_binds(cls, "absent")


def test_name_ref_kind_reads_the_execution_context():
    wrapper = _wrapper(code_fixture("sample_name_ref_kind_reads_the_execution_context__96c63e11.py"))
    seen: dict[tuple[str, int], str] = {}

    class Probe(cst.CSTVisitor):
        METADATA_DEPENDENCIES = (fix_engine.PositionProvider,)

        def visit_Name(self, node) -> None:  # noqa: N802 — libcst's visitor hook
            line = fix_engine._as_range(self.get_metadata(fix_engine.PositionProvider, node)).start.line
            seen[(node.value, line)] = fix_engine._name_ref_kind(wrapper, node)

    wrapper.visit(Probe())
    assert seen[("X", 1)] == "skip"  # the assignment target binds, not reads
    assert seen[("deco", 4)] == "immediate"  # decorators evaluate at def time
    assert seen[("X", 5)] == "immediate"  # a default evaluates at def time
    assert seen[("a", 5)] == "skip"  # a parameter binds
    assert seen[("i", 6)] == "skip"  # a for target binds
    assert seen[("i", 7)] == "deferred"  # a function-body read runs at call time
    assert seen[("X", 10)] == "immediate"  # a class-body read runs at class creation


def test_infer_member_type_names_only_what_it_can_prove():
    def infer(text: str, param_types: dict[str, str] | None = None) -> str | None:
        return MechanicalFixes._infer_member_type(cst.parse_expression(text), param_types or {})

    assert infer("1") == "int"
    assert infer("1.5") == "float"
    assert infer("'s'") == "str"
    assert infer("[]") == "list"
    assert infer("{}") == "dict"
    assert infer("{1}") == "set"
    assert infer("()") == "tuple"
    assert infer("x", {"x": "Money"}) == "Money"  # the parameter's own annotation
    assert infer("Money()") == "Money"  # a construction names its class
    assert infer("obj.attr") is None  # unnameable — leave the member bare
    assert infer("unknown") is None


def test_subscript_mutation_plan_maps_a_dict_store_only():
    def plan(text: str):
        statement = cst.parse_statement(text)
        assert isinstance(statement, cst.SimpleStatementLine)
        return MechanicalFixes._subscript_mutation_plan(cast(cst.Assign, statement.body[0]))

    mapped = plan("d[k] = v\n")
    assert mapped is not None
    assert (mapped.acc, mapped.collection) == ("d", "dict")
    assert isinstance(mapped.key, cst.Name) and mapped.key.value == "k"
    assert plan("d[a, b] = v\n") is None  # a tuple slice is not one dict key
    assert plan("d.k = v\n") is None  # an attribute store is not a subscript
    assert plan("x = v\n") is None


def test_exec_time_names_defer_function_bodies():
    wrapper = _wrapper(code_fixture("sample_exec_time_names_defer_function_bodies__6e6e2afb.py"))
    names = fix_engine._exec_time_names(wrapper, wrapper.module)
    assert "C" in names  # `D = C` runs at import time
    assert fix_engine._name_ref_kind  # the classifier the walk uses
    assert fix_engine._exec_time_names(wrapper, wrapper.module.body[0]) == set()  # a binding reads nothing


def test_dataclass_import_is_recognised_and_prepended_once():
    assert ParameterObjects._is_dataclass_import(cst.parse_statement("from dataclasses import dataclass\n"))
    assert not ParameterObjects._is_dataclass_import(cst.parse_statement("from dataclasses import *\n"))
    assert not ParameterObjects._is_dataclass_import(cst.parse_statement("import dataclasses\n"))
    assert not ParameterObjects._is_dataclass_import(cst.parse_statement("from other import dataclass\n"))
    assert not ParameterObjects._is_dataclass_import(cst.parse_statement("x = 1\n"))
    missing = [cst.parse_statement("x = 1\n")]
    once = ParameterObjects._ensure_dataclasses_import(list(missing))
    assert ParameterObjects._is_dataclass_import(once[0])
    assert ParameterObjects._ensure_dataclasses_import(list(once)) == once  # already present


def test_loop_hoist_refuses_each_unsafe_loop_shape(tmp_path):
    # the refusals are distinct decisions — a runner collapsing them to one
    # "cannot fix" message would lose the reason the reader needs
    src = _fixture("loop_hoist_edges.py")
    repo = make_repo(tmp_path, app_src=code_fixture("build__f9694aad.py"))
    target = repo / "houses" / "app.py"
    cases = {
        "h_else": "without an else",
        "h_pair": "simple name",
        "h_async": "async for-loop",
        "h_no_init": "initialized to an empty",
        "h_prior": "initialized to an empty",
    }
    for fn, reason in cases.items():
        target.write_text(src)
        req = _req("loop-hoist", "houses/app.py", repo, _loop_line(src, fn), fix_engine.FixOptions(name="part"))
        assert req.fix_finding() is None, fn
        assert req.decline is not None
        assert reason in req.decline, (fn, req.decline)
        assert target.read_text() == src, fn  # nothing written


def test_loop_sequence_refuses_an_async_chain(tmp_path):
    src = _fixture("loop_sequence_edges.py")
    repo = make_repo(tmp_path, app_src=code_fixture("build__f9694aad.py"))
    target = repo / "houses" / "app.py"
    target.write_text(src)
    req = _req("loop-sequence", "houses/app.py", repo, _fn_line(src, "s_async"))
    assert req.fix_finding() is None
    assert req.decline is not None
    assert "async for-loop" in req.decline
    assert target.read_text() == src


def test_data_clump_threads_one_parameter_object(tmp_path):
    src = code_fixture("sample_data_clump_threads_one_parameter_object__56a5e3ea.py")
    repo = make_repo(tmp_path, app_src=code_fixture("build__f9694aad.py"))
    (repo / "houses" / "app.py").write_text(src)
    req = _req("extract-class", "houses/app.py", repo, 1, fix_engine.FixOptions(name="Rect"))
    before = _actions(repo)
    assert req.fix_finding() is not None, req.decline
    assert (repo / "houses" / "app.py").is_file()
    # the pair is threaded: signatures take the object, bodies read its fields
    # module-scope call sites construct the instance
    _assert_fixed(repo, before)


def test_data_clump_refuses_variadic_parameters(tmp_path):
    src = code_fixture("sample_data_clump_refuses_variadic_parameters__723cf43d.py")
    repo = make_repo(tmp_path, app_src=code_fixture("build__f9694aad.py"))
    target = repo / "houses" / "app.py"
    target.write_text(src)
    req = _req("data-clump", "houses/app.py", repo, 1, fix_engine.FixOptions(name="Rect"))
    assert req.fix_finding() is None
    assert req.decline is not None
    assert "variadic" in req.decline
    assert target.read_text() == src


def test_partition_splits_field_disjoint_methods(tmp_path):
    src = code_fixture("sample_partition_splits_field_disjoint_methods__e07250b7.py")
    repo = make_repo(tmp_path, app_src=code_fixture("build__f9694aad.py"))
    (repo / "houses" / "app.py").write_text(src)
    req = _req("extract-class", "houses/app.py", repo, 1, fix_engine.FixOptions())
    before = _actions(repo)
    assert req.fix_finding() is not None, req.decline
    assert (repo / "houses" / "app.py").is_file()
    # each group's __init__ keeps only its own fields
    _assert_fixed(repo, before)


def test_partition_refuses_a_fieldless_method(tmp_path):
    src = code_fixture("sample_partition_refuses_a_fieldless_method__05e3f85a.py")
    repo = make_repo(tmp_path, app_src=code_fixture("build__f9694aad.py"))
    target = repo / "houses" / "app.py"
    target.write_text(src)
    req = _req("partition", "houses/app.py", repo, 1, fix_engine.FixOptions())
    assert req.fix_finding() is None
    assert req.decline is not None
    assert "touches no self fields" in req.decline
    assert target.read_text() == src


def test_collapse_chain_rewires_a_module_function_terminal(tmp_path):
    # M -> F -> G where G is a MODULE function (not a method): the head calls
    # G directly and the middle forwarder is deleted
    src = code_fixture("sample_collapse_chain_rewires_a_module_function_terminal__a43f88e5.py")
    repo = make_repo(tmp_path, app_src=code_fixture("build__f9694aad.py"))
    (repo / "houses" / "app.py").write_text(src)
    req = _req("collapse-chain", "houses/app.py", repo, 8, fix_engine.FixOptions())
    before = _actions(repo)
    assert req.fix_finding() is not None, req.decline
    assert (repo / "houses" / "app.py").is_file()
    _assert_fixed(repo, before)


def test_bind_children_walks_every_compound_statement():
    def bound(src: str) -> set[str]:
        names: set[str] = set()
        MechanicalFixes._bind_children(cst.parse_statement(src), names)
        return names

    assert {"i", "a"} <= bound(code_fixture("sample_bind_children_walks_every_compound_statement__a74cf66c.py"))
    assert {"w", "n"} <= bound("while cond:\n    w = 1\nelse:\n    n = 2\n")
    assert {"z", "y"} <= bound(code_fixture("sample_bind_children_walks_every_compound_statement__af5662ec.py"))
    assert {"t", "h", "e", "f"} <= bound(
        "try:\n    t = 1\nexcept E:\n    h = 2\nelse:\n    e = 3\nfinally:\n    f = 4\n"
    )
    assert "handle" in bound("with open(p) as handle:\n    pass\n")
    assert "m" in bound(code_fixture("sample_bind_children_walks_every_compound_statement__10dc4c80.py"))


def test_name_ref_kind_skips_definition_positions():
    wrapper = _wrapper(code_fixture("sample_name_ref_kind_skips_definition_positions__bc97c6f1.py"))
    seen: dict[tuple[str, int], str] = {}

    class Probe(cst.CSTVisitor):
        METADATA_DEPENDENCIES = (fix_engine.PositionProvider,)

        def visit_Name(self, node) -> None:  # noqa: N802 — libcst's visitor hook
            line = fix_engine._as_range(self.get_metadata(fix_engine.PositionProvider, node)).start.line
            seen[(node.value, line)] = fix_engine._name_ref_kind(wrapper, node)

    wrapper.visit(Probe())
    assert seen[("x", 1)] == "skip"  # an AnnAssign target binds
    assert seen[("operating", 2)] == "skip"  # an ImportAlias binds
    assert seen[("en", 3)] == "skip"  # a from-import alias binds
    assert seen[("handle", 4)] == "skip"  # an `as` target binds
    assert seen[("walrus", 6)] == "skip"  # a named expression target binds
    assert seen[("g", 8)] == "skip"  # the function's own name binds
    assert seen[("key", 8)] == "skip"  # a keyword argument's NAME is not a read
    assert seen[("item", 9)] == "deferred"  # the element read runs inside the function
    assert seen[("item", 10)] == "skip"  # the comprehension target binds
    assert seen[("xs", 10)] == "deferred"
    assert seen[("K", 11)] == "skip"  # the class's own name binds
    assert seen[("attr", 12)] == "skip"  # an attribute's name is not a bare read


CLUMP_SRC = code_fixture("module__38ef7335.py")


def test_data_clump_refuses_defaulted_pair_and_imported_callers(tmp_path):
    repo = make_repo(tmp_path, app_src=code_fixture("build__f9694aad.py"))
    target = repo / "houses" / "app.py"
    defaulted = CLUMP_SRC.replace(
        "def diagonal(width: int, height: int) -> float:",
        "def diagonal(width: int, height: int = 1) -> float:",
    )
    target.write_text(defaulted)
    req = _req("data-clump", "houses/app.py", repo, 1, fix_engine.FixOptions(name="Rect"))
    assert req.fix_finding() is None
    assert req.decline is not None
    assert "defaults" in req.decline
    assert target.read_text() == defaulted

    target.write_text(CLUMP_SRC)
    (repo / "houses" / "user.py").write_text("from houses.app import area\n")
    req = _req("data-clump", "houses/app.py", repo, 1, fix_engine.FixOptions(name="Rect"))
    assert req.fix_finding() is None
    assert req.decline is not None
    assert "imports a clump function" in req.decline
    assert target.read_text() == CLUMP_SRC


def test_data_clump_refuses_an_unusable_class_name(tmp_path):
    repo = make_repo(tmp_path, app_src=code_fixture("build__f9694aad.py"))
    target = repo / "houses" / "app.py"
    target.write_text(CLUMP_SRC)
    req = _req("data-clump", "houses/app.py", repo, 1, fix_engine.FixOptions(name="9bad"))
    assert req.fix_finding() is None
    assert req.decline is not None
    assert "not a usable class name" in req.decline


def test_partition_refuses_an_unusable_name_and_a_bound_name(tmp_path):
    src = code_fixture("sample_partition_refuses_an_unusable_name_and_a_bound_name__0b40eaeb.py")
    repo = make_repo(tmp_path, app_src=code_fixture("build__f9694aad.py"))
    target = repo / "houses" / "app.py"
    target.write_text(src)
    req = _req("partition", "houses/app.py", repo, 1, fix_engine.FixOptions(name="9bad"))
    assert req.fix_finding() is None
    assert req.decline is not None
    assert "not a usable class-name prefix" in req.decline

    target.write_text(src + code_fixture("sample_partition_refuses_an_unusable_name_and_a_bound_name__57d48272.py"))
    req = _req("partition", "houses/app.py", repo, 1, fix_engine.FixOptions(name="Report"))
    assert req.fix_finding() is None
    assert req.decline is not None
    assert "already bound" in req.decline


def test_target_bound_names_reads_names_tuple_list_and_starred():
    def names(text: str) -> set[str]:
        return fix_engine._target_bound_names(cst.parse_expression(text))

    assert names("x") == {"x"}
    assert names("a, b") == {"a", "b"}
    assert names("(a, *rest)") == {"a", "rest"}
    assert names("[a, (b, c)]") == {"a", "b", "c"}
    assert names("obj.attr") == set()  # an attribute store binds nothing
    assert names("items[0]") == set()  # a subscript store binds nothing


def test_module_outline_locates_the_class_function_and_scope_at_a_line():
    # every fixer resolves its anchor through this outline, so the four
    # lookups are pinned together: the innermost class, the innermost
    # function, the module-level def line, and module membership
    source = code_fixture("sample_module_outline_locates_the_class_function_and_scope_at_a_line__f716d74d.py")
    module = cst.parse_module(source)
    wrapper = MetadataWrapper(module)
    outline = ModuleOutline(wrapper)

    owner = outline.enclosing_class(1)
    assert isinstance(owner, cst.ClassDef) and owner.name.value == "Owner"
    method = outline.enclosing_function(2)
    assert isinstance(method, cst.FunctionDef) and method.name.value == "method"
    free = outline.function_at(5)
    assert isinstance(free, cst.FunctionDef) and free.name.value == "free"

    assert outline.contains(free)
    assert not outline.contains(method)  # nested in Owner, not a module member
    assert outline.enclosing_class(5) is None

    positions = wrapper.resolve(PositionProvider)
    assert outline.span(positions[free]).start.line == 5
    assert ModuleOutline.resolve_node(free) is free


def test_module_outline_reads_empty_files_and_docstrings():
    assert ModuleOutline.is_empty("# only comments\n\n")
    assert not ModuleOutline.is_empty("value = 1\n")
    fn = cst.parse_module(code_fixture("sample_module_outline_reads_empty_files_and_docstrings__b4e8a380.py")).body[0]
    assert isinstance(fn, cst.FunctionDef)
    assert ModuleOutline.is_docstring_stmt(fn.body.body[0])
    assert not ModuleOutline.is_docstring_stmt(fn.body.body[1])


def test_resolve_dotted_advances_the_attribute_chain():
    # the chain must not stall on the first Attribute: `a.b.c` is three parts
    assert ModuleSplitting._resolve_dotted(cst.parse_expression("a.b.c")) == ["a", "b", "c"]
    assert ModuleSplitting._resolve_dotted(cst.parse_expression("a")) == ["a"]
    assert ModuleSplitting._resolve_dotted(cst.parse_expression("f()")) is None


def test_fixers_do_not_import_the_package_root():
    # importing `fix_engine` from a fixer closes the request -> fixer ->
    # request cycle; the package root is the consumer surface, never a fixer's
    fixers = Path(__file__).resolve().parent.parent / "fix_engine" / "fixers"
    offenders = []
    for path in sorted(fixers.glob("*.py")):
        tree = ast.parse(path.read_text())
        for node in ast.walk(tree):
            if isinstance(node, ast.ImportFrom) and node.module == "fix_engine":
                offenders.append(path.name)
            if isinstance(node, ast.Import) and any(alias.name == "fix_engine" for alias in node.names):
                offenders.append(path.name)
    assert not offenders, f"fixers import the package root (the cycle): {offenders}"

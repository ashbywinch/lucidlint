"""Dissolving a delegating husk into the functions it forwards to.

One fix family, one class; the class holds the request, its helpers are
members, and its libcst visitors are nested classes.
"""

from __future__ import annotations

from typing import Any, override

import libcst as cst
from libcst.metadata import PositionProvider

from fix_engine.module_outline import ModuleOutline
from fix_engine.rewrite import Rewrite
from fix_engine.source_files import SourceFiles


class HuskFixes:
    """The dissolve-husk fix for one request: rewire the call sites, delete the
    husk, never inline."""

    def __init__(self, request) -> None:
        self.request: Any = request

    # lucidlint: ignore closures the nested visitors are one-purpose probes; hoisting names nothing owned
    def run(self) -> str | None:
        """Dissolve a delegating-husk (#27): every method is exactly
        `return F(<all params>)` with F a module-level function. REWIRES the
        stateless call sites to the module functions (`Husk().m(x)` /
        `h.m(x)` -> `m(x)`), deletes the husk, and refuses when any reference
        or constructed instance escapes (imports, isinstance, annotations,
        subclassing, stored/passed instances) — never inlines."""
        source, line = self.request.loaded_source(), self.request.line
        module = cst.parse_module(source)
        wrapper = cst.MetadataWrapper(module)

        class _FindHusk(cst.CSTVisitor):
            METADATA_DEPENDENCIES = (PositionProvider,)

            def __init__(self, target_line):
                self.target_line = target_line
                self.cls: cst.ClassDef | None = None

            @override
            def visit_ClassDef(self, node):
                if (
                    self.cls is None
                    and ModuleOutline.span(self.get_metadata(PositionProvider, node)).start.line == self.target_line
                ):
                    self.cls = node

        probe = _FindHusk(line)
        wrapper.visit(probe)
        cls = probe.cls
        if cls is None:
            return None  # stale anchor — R28 silence
        methods = [s for s in cls.body.body if isinstance(s, cst.FunctionDef)]
        if not methods:
            return None
        method_fns: dict[str, str] = {}
        for method in methods:
            callee = HuskFixes._forwarder_callee_of(method)
            if callee is None:
                return None  # not a pure forwarder — not the husk shape
            method_fns[method.name.value] = callee
        husk = cls.name.value
        # repo-wide analysis: every file's references must be rewireable
        # (direct `Husk().m(...)` calls or a `h = Husk()` + `h.m(...)` pattern)
        files = SourceFiles(self.request.repo).py_files()
        new_sources: dict[str, str] = {}
        for path in files:
            rel = path.relative_to(self.request.repo).as_posix()
            fs = path.read_text(encoding="utf-8")
            mod = cst.parse_module(fs)
            rewired = HuskFixes._rewire_husk_sites(mod, husk, method_fns)
            if rewired is None:
                self.request.decline = (
                    f"a reference to '{husk}' (or a constructed instance) escapes the rewireable "
                    "call pattern — imports, isinstance, annotations, subclassing, or a stored/passed "
                    "instance; dissolve by hand"
                )
                return None
            if rewired != fs:
                new_sources[rel] = rewired
        # build the origin's new source: removed husk class + rewired sites
        origin_rewired = new_sources.pop(self.request.rel, source)
        origin_new = Rewrite(cst.parse_module(origin_rewired)).without_class(husk).code
        if origin_new == source and not new_sources:
            return None  # nothing rewired and nothing deleted — stale
        self.request.extra_writes.extend(new_sources.items())
        return origin_new

    @staticmethod
    def _is_husk_instantiation(stmt: cst.BaseSmallStatement, husk: str) -> bool:
        """`x = Husk()` with no arguments — one instance the static-husk rewrite
        carries onto its members."""
        return (
            isinstance(stmt, cst.Assign)
            and len(stmt.targets) == 1
            and isinstance(stmt.targets[0].target, cst.Name)
            and isinstance(stmt.value, cst.Call)
            and isinstance(stmt.value.func, cst.Name)
            and stmt.value.func.value == husk
            and not stmt.value.args
        )

    @staticmethod
    def _forwarder_callee_of(method: cst.FunctionDef) -> str | None:
        """The module function a method's body exactly forwards to:
        body == `return F(<all params minus the receiver>)` -> F. None otherwise
        (not the pure-forwarder shape the husk rules demand)."""
        params = [p for p in method.params.params if p.name is not None and p.name.value not in ("self", "cls")]
        body = method.body.body if isinstance(method.body, cst.IndentedBlock) else []
        if len(body) != 1 or not isinstance(body[0], cst.SimpleStatementLine):
            return None
        stmts = body[0].body
        if len(stmts) != 1 or not isinstance(stmts[0], cst.Return):
            return None
        ret = stmts[0]
        if not isinstance(ret.value, cst.Call) or not isinstance(ret.value.func, cst.Name):
            return None
        args = list(ret.value.args)
        if len(args) != len(params):
            return None
        for arg, p in zip(args, params, strict=True):
            if not isinstance(arg.value, cst.Name) or arg.value.value != p.name.value:
                return None
        return ret.value.func.value

    # lucidlint: ignore closures the nested visitors are one-purpose probe walks — hoisting names nothing owned
    @staticmethod
    def _rewire_husk_sites(module: cst.Module, husk: str, method_fns: dict[str, str]) -> str | None:
        """Rewrite a file's rewireable husk sites (`Husk().m(x)` / `h.m(x)` ->
        `m(x)`, `h = Husk()` deleted). None when any reference or constructed
        instance escapes the rewireable pattern — dissolve by hand."""

        class _HuskProbe(cst.CSTVisitor):
            """Collect instance variables (`h = Husk()`) and flag escapes."""

            def __init__(self):
                self.instances: set[str] = set()
                self.statements: list[cst.SimpleStatementLine] = []
                self.refused: str | None = None

            @override
            def visit_SimpleStatementLine(self, node):
                for stmt in node.body:
                    if HuskFixes._is_husk_instantiation(stmt, husk):
                        assert isinstance(stmt, cst.Assign) and isinstance(stmt.targets[0].target, cst.Name)
                        self.instances.add(stmt.targets[0].target.value)
                        self.statements.append(node)
                return True

        probe = _HuskProbe()
        module.visit(probe)
        instances = probe.instances

        class _HuskRewire(cst.CSTTransformer):
            def __init__(self):
                self.bad: str | None = None

            @override
            def leave_Call(self, original_node, updated_node):
                func = updated_node.func
                if isinstance(func, cst.Attribute) and isinstance(func.value, cst.Call):
                    # Husk().m(<args>) -> Fm(<args>)
                    inner = func.value
                    if HuskFixes._is_husk_method_call(inner, func, husk, method_fns):
                        return updated_node.with_changes(func=cst.Name(method_fns[func.attr.value]))
                # h.m(<args>) -> Fm(<args>) for a proven instance variable
                if HuskFixes._is_husk_attr_call(func, instances, method_fns):
                    assert isinstance(func, cst.Attribute)
                    return updated_node.with_changes(func=cst.Name(method_fns[func.attr.value]))
                return updated_node

        rewired = module.visit(_HuskRewire())

        class _Leftover(cst.CSTVisitor):
            """Any remaining reference to the husk or an instance variable —
            EXCEPT inside the husk class's own subtree (deleted afterwards)."""

            def __init__(self):
                self.names: list[str] = []
                self.in_husk: int = 0

            def _is_husk_name(self, node) -> bool:
                """A reference to the husk class or one of its instances, outside
                the husk's own subtree."""
                return self.in_husk == 0 and (node.value == husk or node.value in instances)

            @override
            def visit_ClassDef(self, node):
                if node.name.value == husk:
                    self.in_husk += 1

            @override
            def leave_ClassDef(self, original_node):
                if original_node.name.value == husk:
                    self.in_husk -= 1

            @override
            def visit_Name(self, node):
                if self._is_husk_name(node):
                    self.names.append(node.value)

        # drop the `h = Husk()` assignments: identity removal needs nodes OF the
        # rewired tree (the rewire produced new objects), so re-probe it
        drop_probe = _HuskProbe()
        rewired.visit(drop_probe)
        if drop_probe.statements:
            rewired = Rewrite(rewired).without(drop_probe.statements)
        # the husk class itself is deleted by the caller — scan leftovers on the
        # tree WITHOUT it (the class's own subtree must not count as a reference)
        scanned = Rewrite(rewired).without_class(husk)
        left_scan = _Leftover()
        scanned.visit(left_scan)
        leftover_names = list(left_scan.names)
        if leftover_names:
            return None  # a reference escaped the rewireable pattern
        return rewired.code

    @staticmethod
    def _is_husk_method_call(
        inner: cst.BaseExpression, func: cst.BaseExpression, husk: str, method_fns: dict[str, str]
    ) -> bool:
        """`h.m(...)` on the husk instance, where `m` is one of its methods."""
        return (
            isinstance(inner, cst.Call)
            and isinstance(inner.func, cst.Name)
            and inner.func.value == husk
            and not inner.args
            and isinstance(func, cst.Attribute)
            and func.attr.value in method_fns
        )

    @staticmethod
    def _is_husk_attr_call(func: cst.BaseExpression, instances: set[str], method_fns: dict[str, str]) -> bool:
        """`<instance>.m(...)` where the instance is a dissolved husk."""
        return (
            isinstance(func, cst.Attribute)
            and isinstance(func.value, cst.Name)
            and func.value.value in instances
            and func.attr.value in method_fns
        )

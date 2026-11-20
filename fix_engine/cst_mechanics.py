"""The libcst mechanics every fixer shares.

A fixer answers one finding's shape; these are the moves every shape makes —
where a node sits, what a class body binds, how a module's defs are removed or
replaced, which Python constructs are the compiler's shorthand rather than the
domain's. One class, so the fixers speak about CST shape in one vocabulary
instead of restating it per family.
"""

from __future__ import annotations

import builtins
from typing import TypeGuard, override

import libcst as cst
from libcst.metadata import CodeRange, PositionProvider


class CstMechanics:
    """The shared CST shape helpers."""

    # the builtins are ambient names: a free variable that is a builtin is not
    # an out-variable, and a class body reading one reads nothing it owns
    BUILTINS = frozenset(dir(builtins))

    @staticmethod
    def _as_range(pos: object) -> CodeRange:
        """The resolved metadata position. libcst's get_metadata types its result
        through an UNDEFINED sentinel default — narrow to the CodeRange that is
        always present for nodes visited inside a MetadataWrapper."""
        assert isinstance(pos, CodeRange)
        return pos

    @staticmethod
    def _as_node(node: object) -> cst.CSTNode:
        """The resolved metadata node — the same UNDEFINED-sentinel widening as
        _as_range, for providers that return nodes (ParentNodeProvider)."""
        assert isinstance(node, cst.CSTNode)
        return node

    @staticmethod
    def _self_attr_target(name: str) -> list[cst.AssignTarget]:
        """The `self.<name>` assignment target a record constructor stores."""
        return [cst.AssignTarget(cst.Attribute(cst.Name("self"), cst.Name(name)))]

    @staticmethod
    def _record_class_def(class_name: str, field_names: list[str]) -> cst.ClassDef:
        """The record class: __init__ taking the fields, storing them. A class,
        not a NamedTuple — it can grow behavior without a migration."""
        body = [
            cst.SimpleStatementLine(
                body=[
                    cst.Assign(
                        targets=CstMechanics._self_attr_target(f),
                        value=cst.Name(f),
                    )
                ]
            )
            for f in field_names
        ]
        init = cst.FunctionDef(
            name=cst.Name("__init__"),
            params=cst.Parameters(params=[cst.Param(cst.Name("self"))] + [cst.Param(cst.Name(f)) for f in field_names]),
            body=cst.IndentedBlock(body=body),
        )
        return cst.ClassDef(
            name=cst.Name(class_name),
            body=cst.IndentedBlock(body=[init]),
        )

    class _EnclosingClass(cst.CSTVisitor):
        """The innermost ClassDef containing the line — the method's owner."""

        METADATA_DEPENDENCIES = (PositionProvider,)

        def __init__(self, line: int) -> None:
            self.line: int = line
            self.found: cst.ClassDef | None = None

        @override
        def visit_ClassDef(self, node) -> None:
            pos = CstMechanics._as_range(self.get_metadata(PositionProvider, node))
            if pos.start.line <= self.line <= pos.end.line:
                self.found = node

    @staticmethod
    def _is_self_attribute(value: cst.BaseExpression) -> bool:
        """`self.<attr>` — an attribute read on the receiver."""
        return isinstance(value, cst.Attribute) and isinstance(value.value, cst.Name) and value.value.value == "self"

    @staticmethod
    def _iter_nodes(nodes):
        """Every node in the subtree, parents before children; accepts a node
        or an iterable of nodes."""
        if isinstance(nodes, cst.CSTNode):
            nodes = [nodes]
        for n in nodes:
            yield n
            for c in n.children:
                yield from CstMechanics._iter_nodes(c)

    @staticmethod
    def _iter_nodes_pruned(nodes):
        """Like `_iter_nodes` but never descends into nested scopes — a `def`
        or `class` inside the walked body owns its locals; their writes are
        not the body's writes."""
        if isinstance(nodes, cst.CSTNode):
            nodes = [nodes]
        for n in nodes:
            yield n
            if isinstance(n, (cst.FunctionDef, cst.ClassDef)):
                continue
            for c in n.children:
                yield from CstMechanics._iter_nodes_pruned(c)

    @staticmethod
    def _hoist_target_names(t) -> set[str]:
        """The names a binding TARGET binds — Name/Tuple/List/Starred (a
        subscript/attribute target is a WRITE, not a binding)."""
        if isinstance(t, cst.AssignTarget):
            return CstMechanics._hoist_target_names(t.target)
        if isinstance(t, cst.Name):
            return {t.value}
        if isinstance(t, (cst.Tuple, cst.List)):
            out: set[str] = set()
            for el in t.elements:
                out.update(CstMechanics._hoist_target_names(el.value))
            return out
        if isinstance(t, cst.StarredElement):
            return CstMechanics._hoist_target_names(t.value)
        return set()

    @staticmethod
    def _expr_names(e) -> set[str]:
        return {n.value for n in CstMechanics._iter_nodes([e]) if isinstance(n, cst.Name)}

    @staticmethod
    def _is_keyword_name(node, parent) -> bool:
        """A Name that is a keyword-argument's keyword (`f(a=1)` — the `a`) is
        not a variable reference; counting it made the seam analysis fabricate
        phantom parameters (the `leading_lines` in `with_changes(leading_lines=[])`
        would appear as a free var and the rewritten call would NameError)."""
        return isinstance(parent, cst.Arg) and parent.keyword is node

    @staticmethod
    def _import_base_name(name: cst.Attribute | cst.Name) -> str:
        """The first component of a dotted module path: `a.b.c` binds `a`."""
        base: cst.BaseExpression = name
        while isinstance(base, cst.Attribute):
            base = base.value
        assert isinstance(base, cst.Name)  # an import's dotted path bottoms at a Name
        return base.value

    @staticmethod
    def _collect_alias_names(names, stmt):
        for a in stmt.names:
            if a.asname is not None:
                names.update(CstMechanics._hoist_target_names(a.asname.name))
            else:
                names.add(CstMechanics._import_base_name(a.name))

    @staticmethod
    def _extraction_is_private() -> bool:
        """Is an extracted function private (leading underscore)? Yes — a fresh
        helper cannot have external callers (it did not exist before the fix),
        so by construction any extraction is an implementation detail of its
        source function. The convention propagates automatically: the seam's
        name gets the underscore whether the agent supplies it or not."""

        return True


    class _RemoveNodes(cst.CSTTransformer):
        """Remove exactly the given nodes from the tree (identity)."""

        def __init__(self, targets) -> None:
            self.targets: set[cst.CSTNode] = set(targets)

        @override
        def on_leave(self, original_node, updated_node):
            if original_node in self.targets:
                return cst.RemoveFromParent()
            return updated_node

    @staticmethod
    def _is_docstring_stmt(
        stmt: cst.BaseSmallStatement | cst.BaseStatement,
    ) -> TypeGuard[cst.SimpleStatementLine]:
        return (
            isinstance(stmt, cst.SimpleStatementLine)
            and len(stmt.body) == 1
            and isinstance(stmt.body[0], cst.Expr)
            and isinstance(stmt.body[0].value, (cst.SimpleString, cst.ConcatenatedString))
        )

    class _EnclosingFn(cst.CSTVisitor):
        """The innermost FunctionDef whose span contains the line — the member
        assignment's enclosing constructor, for the annotation inference."""

        METADATA_DEPENDENCIES = (PositionProvider,)

        def __init__(self, line: int) -> None:
            self.line: int = line
            self.found: cst.FunctionDef | None = None

        @override
        def visit_FunctionDef(self, node) -> None:
            pos = CstMechanics._as_range(self.get_metadata(PositionProvider, node))
            if pos.start.line <= self.line <= pos.end.line:
                self.found = node


    class _FindFnLine(cst.CSTVisitor):
        """Locate the first FunctionDef whose def line equals the target — its
        node comes from the wrapper's (metadata-resolvable) tree."""

        METADATA_DEPENDENCIES = (PositionProvider,)

        def __init__(self, line: int) -> None:
            self.line: int = line
            self.found: cst.FunctionDef | None = None

        @override
        def visit_FunctionDef(self, node) -> None:
            if (
                self.found is None
                and CstMechanics._as_range(self.get_metadata(PositionProvider, node)).start.line == self.line
            ):
                self.found = node

    @staticmethod
    def _module_contains(module: cst.Module, fn: cst.FunctionDef | None) -> bool:
        """Is the function still a direct child of the module?"""
        return fn is not None and any(s is fn for s in module.body)

    class _FreeNames(cst.CSTVisitor):
        """The names a function READS from enclosing scopes: every Name node in
        the tree minus the names bound anywhere inside it (parameters, locals,
        for/comprehension/with targets, nested def names). A name bound inside
        cannot be a reference to a module-level binding, so the extract-module
        module-binding check must exclude it (review finding: a moved function
        with a parameter named like a module constant was falsely refused)."""

        def __init__(self) -> None:
            self.names: set[str] = set()
            self.bound: set[str] = set()

        def _add_params(self, params) -> None:
            for p in params:
                if p.name is not None:
                    self.bound.add(p.name.value)

        @override
        def visit_FunctionDef(self, node) -> None:
            self.bound.add(node.name.value)
            self._add_params(node.params.posonly_params)
            self._add_params(node.params.params)
            self._add_params(node.params.kwonly_params)
            # a bare `*` keyword-only separator is ParamStar — no name; the
            # guard must check the NODE type before touching `.name` (review
            # finding: the isinstance on `.name` crashed first)
            if isinstance(node.params.star_arg, cst.Param):
                self.bound.add(node.params.star_arg.name.value)
            if isinstance(node.params.star_kwarg, cst.Param):
                self.bound.add(node.params.star_kwarg.name.value)

        @override
        def visit_Lambda(self, node) -> None:
            self._add_params(node.params.posonly_params)
            self._add_params(node.params.params)
            self._add_params(node.params.kwonly_params)

        @override
        def visit_Name(self, node) -> None:
            self.names.add(node.value)

        @override
        def visit_Assign(self, node) -> None:
            for t in node.targets:
                self.bound.update(CstMechanics._hoist_target_names(t.target))

        @override
        def visit_AnnAssign(self, node) -> None:
            if node.target:
                self.bound.update(CstMechanics._hoist_target_names(node.target))

        @override
        def visit_For(self, node) -> None:
            self.bound.update(CstMechanics._hoist_target_names(node.target))

        @override
        def visit_CompFor(self, node) -> None:
            self.bound.update(CstMechanics._hoist_target_names(node.target))

        @override
        def visit_With(self, node) -> None:
            for item in node.items:
                if item.asname is not None:
                    self.bound.update(CstMechanics._hoist_target_names(item.asname.name))

        @override
        def visit_ExceptHandler(self, node) -> None:
            # libcst's ExceptHandler.name is an AsName — the alias is `.name`,
            # not `.value` (review finding: a crash on any `except ... as e:`)
            if isinstance(node.name, cst.AsName) and isinstance(node.name.name, cst.Name):
                self.bound.add(node.name.name.value)

    @staticmethod
    def _moved_imports(module: cst.Module, referenced: set[str]) -> list:
        """The origin's module-level imports binding a referenced name — what the
        new module needs (libcst wraps imports in SimpleStatementLine)."""
        moved: list = []
        for stmt in module.body:
            if not isinstance(stmt, cst.SimpleStatementLine):
                continue
            # a line can hold several imports (`import os; import sys`) — emit
            # each matching import as its own statement, or the new module
            # misses a name the moved def needs and NameErrors (review bot)
            for inner in stmt.body:
                if isinstance(inner, cst.Import):
                    for alias in inner.names:
                        bound = (
                            CstMechanics._hoist_target_names(alias.asname.name)
                            if alias.asname is not None
                            else {CstMechanics._import_base_name(alias.name)}
                        )
                        if bound & referenced:
                            moved.append(cst.SimpleStatementLine(body=[inner]))
                            break
                elif isinstance(inner, cst.ImportFrom):
                    if isinstance(inner.names, cst.ImportStar):
                        continue  # `from x import *` binds no importable name
                    for alias in inner.names:
                        if not isinstance(alias.name, cst.Name):
                            continue
                        bound = (
                            CstMechanics._hoist_target_names(alias.asname.name)
                            if alias.asname is not None
                            else {alias.name.value}
                        )
                        if bound & referenced:
                            moved.append(cst.SimpleStatementLine(body=[inner]))
                            break
        return moved

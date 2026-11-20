"""What a scope binds and reads.

A moved function must not reach a name it does not take from an enclosing
scope, and a moved class must not read a module binding the new module lacks:
both questions are the same one, asked of a node. `Bindings(scope)` is that
scope.
"""

from __future__ import annotations

import builtins
from typing import override

import libcst as cst

from fix_engine.module_outline import ModuleOutline


class Bindings:
    """The bindings of one scope (a function, a class, or a statement)."""

    def __init__(self, scope: cst.CSTNode) -> None:
        self.scope = scope

    @staticmethod
    @staticmethod
    def import_base_name(name: cst.Attribute | cst.Name) -> str:
        """The first component of a dotted module path: `a.b.c` binds `a`."""
        base: cst.BaseExpression = name
        while isinstance(base, cst.Attribute):
            base = base.value
        assert isinstance(base, cst.Name)  # an import's dotted path bottoms at a Name
        return base.value

    # the builtins are ambient: a free variable that is a builtin is not an
    # out-variable, and a class body reading one reads nothing it owns
    # the builtins are ambient names: a free variable that is a builtin is not
    # an out-variable, and a class body reading one reads nothing it owns
    BUILTINS = frozenset(dir(builtins))

    @staticmethod
    @staticmethod
    def expr_names(e) -> set[str]:
        return {n.value for n in ModuleOutline.iter_nodes([e]) if isinstance(n, cst.Name)}

    @staticmethod
    @staticmethod
    def collect_alias_names(names, stmt):
        for a in stmt.names:
            if a.asname is not None:
                names.update(Bindings.hoist_target_names(a.asname.name))
            else:
                names.add(Bindings.import_base_name(a.name))

    def free_names(self) -> set[str]:
        """The names the scope READS from enclosing scopes: every Name in its
        tree minus the names bound anywhere inside it (parameters, locals,
        for/comprehension/with targets, nested def names)."""
        probe = Bindings._FreeNames()
        self.scope.visit(probe)
        return probe.names - probe.bound

    @staticmethod
    @staticmethod
    def hoist_target_names(t) -> set[str]:
        """The names a binding TARGET binds — Name/Tuple/List/Starred (a
        subscript/attribute target is a WRITE, not a binding)."""
        if isinstance(t, cst.AssignTarget):
            return Bindings.hoist_target_names(t.target)
        if isinstance(t, cst.Name):
            return {t.value}
        if isinstance(t, (cst.Tuple, cst.List)):
            out: set[str] = set()
            for el in t.elements:
                out.update(Bindings.hoist_target_names(el.value))
            return out
        if isinstance(t, cst.StarredElement):
            return Bindings.hoist_target_names(t.value)
        return set()

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
                self.bound.update(Bindings.hoist_target_names(t.target))

        @override
        def visit_AnnAssign(self, node) -> None:
            if node.target:
                self.bound.update(Bindings.hoist_target_names(node.target))

        @override
        def visit_For(self, node) -> None:
            self.bound.update(Bindings.hoist_target_names(node.target))

        @override
        def visit_CompFor(self, node) -> None:
            self.bound.update(Bindings.hoist_target_names(node.target))

        @override
        def visit_With(self, node) -> None:
            for item in node.items:
                if item.asname is not None:
                    self.bound.update(Bindings.hoist_target_names(item.asname.name))

        @override
        def visit_ExceptHandler(self, node) -> None:
            # libcst's ExceptHandler.name is an AsName — the alias is `.name`,
            # not `.value` (review finding: a crash on any `except ... as e:`)
            if isinstance(node.name, cst.AsName) and isinstance(node.name.name, cst.Name):
                self.bound.add(node.name.name.value)

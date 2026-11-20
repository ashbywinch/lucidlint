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
from libcst.metadata import ExpressionContextProvider

from fix_engine.module_outline import ModuleOutline

# lucidlint: ignore-file record-shape the libcst layer's tuple/dict shorthands ARE its wire records —
# lucidlint: ignore-file record-shape a class per helper hop is ceremony


StoredNames = dict[str, set[str]]
"""Each function's body-stored names — what a receiver rename asks before it
rewrites a load: a name stored anywhere in the body is a local, not the param."""


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

    @staticmethod
    def module_name_nodes(module: cst.Module) -> list[cst.Name]:
        """Every Name node at module level (a visitor probe)."""
        class _AllNames(cst.CSTVisitor):
            def __init__(self):
                self.names: list[cst.Name] = []

            @override
            def visit_Name(self, node):
                self.names.append(node)

        probe = _AllNames()
        module.visit(probe)
        return probe.names

    @staticmethod
    def class_body_binds(cls: cst.ClassDef, name: str) -> bool:
        """Does the class's body bind `name` at class-body level (an attribute
        with the same name as the constant about to move in)?"""
        for stmt in cls.body.body:
            if isinstance(stmt, (cst.FunctionDef, cst.ClassDef)):
                if stmt.name.value == name:
                    return True
            elif isinstance(stmt, cst.SimpleStatementLine):
                for inner in stmt.body:
                    if isinstance(inner, cst.Assign) and any(
                        name in Bindings.hoist_target_names(t.target) for t in inner.targets
                    ):
                        return True
                    if _binds_hoisted_name(inner, name):
                        return True
                    if isinstance(inner, (cst.Import, cst.ImportFrom)):
                        bound: set[str] = set()
                        Bindings.collect_alias_names(bound, inner)
                        if name in bound:
                            return True
        return False

    @staticmethod
    def module_bindings(module: cst.Module) -> set[str]:
        """The names module-level assignments/constants bind — the
        extract-module split refuses when moved code reads one (the new module
        would NameError; moving the binding would break the origin)."""
        bound: set[str] = set()
        for stmt in module.body:
            if not (isinstance(stmt, cst.SimpleStatementLine) and len(stmt.body) == 1):
                continue
            inner = stmt.body[0]
            if isinstance(inner, cst.Assign):
                for t in inner.targets:
                    bound |= Bindings.hoist_target_names(t.target)
            elif isinstance(inner, cst.AnnAssign) and inner.target is not None:
                bound |= Bindings.hoist_target_names(inner.target)
        return bound

    def free_names(self) -> set[str]:
        """The names the scope READS from enclosing scopes: every Name in its
        tree minus the names bound anywhere inside it (parameters, locals,
        for/comprehension/with targets, nested def names)."""
        probe = Bindings._FreeNames()
        self.scope.visit(probe)
        return probe.names - probe.bound

    @staticmethod
    def binds_name(module: cst.Module, name: str) -> bool:
        """Does the module already bind `name` (class, def, or assignment
        target)? A generated class of that name would shadow or duplicate it."""
        finder = Bindings._BindsName(name)
        module.visit(finder)
        return finder.found

    @staticmethod
    def stores_by_function(wrapper: cst.MetadataWrapper) -> StoredNames:
        """The names each function assigns anywhere in its body — those are
        locals, never the receiver (a shadowing `state = state.line` must not
        have its loads renamed to `self`). One pass over the whole module,
        tracking the current function."""
        probe = Bindings._Stores()
        wrapper.visit(probe)
        return probe.per_fn

    class _BindsName(cst.CSTVisitor):
        """Does the module bind the name as a class, a def, or an assignment
        target?"""

        def __init__(self, name: str) -> None:
            self.name: str = name
            self.found: bool = False

        @override
        def visit_ClassDef(self, node) -> None:
            if node.name.value == self.name:
                self.found = True

        @override
        def visit_FunctionDef(self, node) -> None:
            if node.name.value == self.name:
                self.found = True

        @override
        def visit_Assign(self, node) -> None:
            for target in node.targets:
                if isinstance(target.target, cst.Name) and target.target.value == self.name:
                    self.found = True

    class _Stores(cst.CSTVisitor):
        """The names each function stores in its body, attributed to the
        innermost enclosing def (a nested def must not clobber the outer one)."""

        METADATA_DEPENDENCIES = (ExpressionContextProvider,)

        def __init__(self) -> None:
            self.per_fn: StoredNames = {}
            self._current: str | None = None
            self._in_param: bool = False
            self._fn_stack: list[str | None] = []

        @override
        def visit_FunctionDef(self, node) -> None:
            # remember the enclosing fn so leave_FunctionDef can restore it —
            # a nested def must not clobber the strewing fn's attribution
            self._fn_stack.append(self._current)
            self._current = node.name.value
            self.per_fn.setdefault(self._current, set())

        @override
        def leave_FunctionDef(self, original_node) -> None:
            self._current = self._fn_stack.pop()

        @override
        def visit_Param(self, node) -> None:
            # parameter names are STORE-context Names — they are the signature,
            # not body locals; skip them
            self._in_param = True

        @override
        def leave_Param(self, original_node) -> None:
            self._in_param = False

        def _is_body_store(self, ctx) -> bool:
            """A name stored by the function body — not a parameter (the
            signature) and not outside any function."""
            return (
                ctx == cst.metadata.ExpressionContext.STORE and self._current is not None and not self._in_param
            )

        @override
        def visit_Name(self, node) -> None:
            try:
                ctx = self.get_metadata(ExpressionContextProvider, node)
            except KeyError:
                return  # attribute names (obj.state) are not refs
            if self._is_body_store(ctx):
                assert self._current is not None
                self.per_fn[self._current].add(node.value)

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

def _binds_hoisted_name(stmt: cst.BaseSmallStatement, name: str) -> bool:
    """The annotated assignment binds `name` (directly or through a target
    tuple)."""
    return (
        isinstance(stmt, cst.AnnAssign)
        and stmt.target is not None
        and name in Bindings.hoist_target_names(stmt.target)
    )


"""The The feature-envy fix: the envied receiver's reads become its method.

One fix family, one class; the class holds the request, its helpers are
members, and its libcst visitors are nested classes.
"""

from __future__ import annotations

from typing import Any, NamedTuple, override

import libcst as cst

from fix_engine.bindings import Bindings
from fix_engine.module_outline import ModuleOutline


class FeatureEnvy:
    """The feature envy fix for one request."""

    def __init__(self, request: Any) -> None:
        self.request = request

    class _Collaborator(NamedTuple):
        """One `graph = self.graph` alias: the local name and the owner field
        it aliases."""

        alias: str
        attr: str

    class _StmtReads(NamedTuple):
        """One statement's analysis for the move: the names it reads, the
        names it binds, and whether it touches self."""

        reads: set[str]
        bound: set[str]
        has_self: bool

    def run(self) -> str | None:
        """Move the envied-receiver reads out of the method into a new method
        on the envied class (--name), replacing them with a call. The receiver
        is the local aliased from self.<attr> with the most field reads; the
        envied class comes from the owner's annotated `self.<attr>: <Class>` —
        the fix refuses when the class cannot be named (no annotation)."""
        source = self.request.loaded_source()
        method_name = self.request.opts.name
        if not method_name:
            return None
        module = cst.parse_module(source)
        wrapper = cst.MetadataWrapper(module)
        outline = ModuleOutline(wrapper)
        method = outline.enclosing_function(self.request.line)
        if method is None or not isinstance(method, cst.FunctionDef):
            return None
        owner = outline.enclosing_class(self.request.line)
        if owner is None:
            return None
        aliases = FeatureEnvy._collaborator_aliases(method)
        if not aliases:
            return None
        alias, attr = max(aliases, key=lambda a: FeatureEnvy._alias_field_reads(method, a.alias))
        if FeatureEnvy._alias_field_reads(method, alias) < 4:
            return None
        envied = FeatureEnvy._find_envied_class(owner, attr)
        if envied is None:
            return None
        derived: set[str] = {alias}
        derived_order: list[str] = []
        movable: list = []
        movable_idx: list[int] = []
        remaining: list = []
        stmts = list(method.body.body)
        for i, stmt in enumerate(stmts):
            reads, bound, has_self = FeatureEnvy._stmt_analysis(stmt)
            external = reads - derived
            if FeatureEnvy._derives_its_own_names(bound, external, has_self):
                derived |= bound
                for n in bound:
                    if n not in derived_order:
                        derived_order.append(n)
                movable.append(stmt)
                movable_idx.append(i)
            else:
                remaining.append(stmt)
        if len(movable) < 1:
            return None
        remaining_reads: set[str] = set()
        for stmt in remaining:
            reads, _, _ = FeatureEnvy._stmt_analysis(stmt)
            remaining_reads |= reads
        outputs = [n for n in derived_order if n in remaining_reads]
        new_body: list = []
        for stmt in movable:
            new_body.append(FeatureEnvy._rewrite_receiver_to_self(stmt, alias))
        ret = cst.Return(value=cst.Tuple(elements=[cst.Element(cst.Name(n)) for n in outputs]) if outputs else None)
        new_body.append(cst.SimpleStatementLine(body=[ret]))
        new_method = cst.FunctionDef(
            name=cst.Name(method_name),
            params=cst.Parameters(params=[cst.Param(cst.Name("self"))]),
            body=cst.IndentedBlock(body=new_body),
        )
        wrapper_module = wrapper.module
        envied_cls = next(
            (c for c in wrapper_module.body if isinstance(c, cst.ClassDef) and c.name.value == envied),
            None,
        )
        if envied_cls is None:
            return None
        new_module_body = list(wrapper_module.body)
        cls_i = new_module_body.index(envied_cls)
        new_module_body[cls_i] = envied_cls.with_changes(
            body=envied_cls.body.with_changes(body=list(envied_cls.body.body) + [new_method])
        )
        call = cst.Call(func=cst.Attribute(value=cst.Name(alias), attr=cst.Name(method_name)))
        if outputs:
            call_stmt: cst.BaseStatement = cst.SimpleStatementLine(
                body=[
                    cst.Assign(
                        targets=[cst.AssignTarget(cst.Tuple(elements=[cst.Element(cst.Name(n)) for n in outputs]))],
                        value=call,
                    )
                ],
                leading_lines=[cst.EmptyLine()],
            )
        else:
            call_stmt = cst.SimpleStatementLine(body=[cst.Expr(call)])
        new_method_body: list = []
        for i, stmt in enumerate(stmts):
            if i in movable_idx:
                if i == movable_idx[0]:
                    new_method_body.append(call_stmt)
                continue
            new_method_body.append(stmt)
        new_method_def = method.with_changes(body=cst.IndentedBlock(body=new_method_body))
        owner_i = new_module_body.index(owner)
        new_module_body[owner_i] = owner.with_changes(
            body=owner.body.with_changes(body=[new_method_def if s is method else s for s in owner.body.body])
        )
        return cst.Module(body=new_module_body).code

    @staticmethod
    def _alias_field_reads(method: cst.FunctionDef, alias: str) -> int:
        """The `alias.field` reads in the method."""
        count = 0

        class _V(cst.CSTVisitor):
            @override
            def visit_Attribute(self, node) -> None:
                nonlocal count
                if isinstance(node.value, cst.Name) and node.value.value == alias:
                    count += 1

        method.visit(_V())
        return count

    @staticmethod
    def _annotates_self_attr(assign: cst.AnnAssign, target: cst.BaseExpression, attr: str) -> bool:
        """The annotated assignment is `self.<attr>: SomeType = ...`."""
        return (
            isinstance(target, cst.Attribute)
            and isinstance(target.value, cst.Name)
            and target.value.value == "self"
            and target.attr.value == attr
            and isinstance(assign.annotation.annotation, cst.Name)
        )

    @staticmethod
    def _collaborator_aliases(method: cst.FunctionDef) -> list[FeatureEnvy._Collaborator]:
        """The (alias, attr) pairs: `graph = self.graph` assignments in the
        method — the collaborators the method may envy."""
        out: list[FeatureEnvy._Collaborator] = []
        for stmt in method.body.body:
            if not isinstance(stmt, cst.SimpleStatementLine) or len(stmt.body) != 1:
                continue
            assign = stmt.body[0]
            if not isinstance(assign, cst.Assign) or len(assign.targets) != 1:
                continue
            target = assign.targets[0].target
            if not isinstance(target, cst.Name):
                continue
            value = assign.value
            if ModuleOutline.is_self_attribute(value):
                assert isinstance(value, cst.Attribute)
                out.append(FeatureEnvy._Collaborator(target.value, value.attr.value))
        return out

    @staticmethod
    def _derives_its_own_names(bound: set[str], external: set[str], has_self: bool) -> bool:
        """The statement binds names it derives itself: it reads nothing from the
        enclosing scope and does not touch the receiver, so it can move whole."""
        return bool(bound) and not has_self and not external

    @staticmethod
    def _find_envied_class(owner: cst.ClassDef, attr: str) -> str | None:
        """The annotated type of `self.<attr>` in the owner's __init__."""
        for stmt in owner.body.body:
            if not isinstance(stmt, cst.FunctionDef) or stmt.name.value != "__init__":
                continue
            for s in stmt.body.body:
                if not isinstance(s, cst.SimpleStatementLine) or len(s.body) != 1:
                    continue
                assign = s.body[0]
                if not isinstance(assign, cst.AnnAssign) or assign.target is None:
                    continue
                target = assign.target
                if FeatureEnvy._annotates_self_attr(assign, target, attr):
                    assert isinstance(assign.annotation.annotation, cst.Name)
                    return assign.annotation.annotation.value
        return None

    @staticmethod
    def _rewrite_receiver_to_self(
        stmt: cst.BaseStatement | cst.BaseSmallStatement, alias: str
    ) -> cst.BaseStatement | cst.BaseSmallStatement | cst.RemovalSentinel:
        """receiver.field -> self.field in the statement (attribute reads on the
        alias become self reads)."""

        class _T(cst.CSTTransformer):
            @override
            def leave_Attribute(self, original_node, updated_node):
                if isinstance(updated_node.value, cst.Name) and updated_node.value.value == alias:
                    return updated_node.with_changes(value=cst.Name("self"))
                return updated_node

        result = stmt.visit(_T())
        # only Attribute reads are rewritten — no sentinel can come back
        assert not isinstance(result, (cst.RemovalSentinel, cst.FlattenSentinel))
        return result

    @staticmethod
    def _stmt_analysis(stmt: cst.BaseStatement | cst.BaseSmallStatement) -> FeatureEnvy._StmtReads:
        reads: set[str] = set()
        bound: set[str] = set()
        attr_names: set[str] = set()
        has_self = False

        class _V(cst.CSTVisitor):
            @override
            def visit_Name(self, node) -> None:
                reads.add(node.value)

            @override
            def visit_Attribute(self, node) -> None:
                nonlocal has_self
                attr_names.add(node.attr.value)
                if isinstance(node.value, cst.Name) and node.value.value == "self":
                    has_self = True

            @override
            def visit_Assign(self, node) -> None:
                for t in node.targets:
                    bound.update(Bindings.hoist_target_names(t.target))

            @override
            def visit_AnnAssign(self, node) -> None:
                if node.target:
                    bound.update(Bindings.hoist_target_names(node.target))

            @override
            def visit_For(self, node) -> None:
                bound.update(Bindings.hoist_target_names(node.target))

            @override
            def visit_CompFor(self, node) -> None:
                bound.update(Bindings.hoist_target_names(node.target))

            @override
            def visit_With(self, node) -> None:
                for item in node.items:
                    if item.asname is not None:
                        bound.update(Bindings.hoist_target_names(item.asname.name))

        stmt.visit(_V())
        return FeatureEnvy._StmtReads(reads - bound - attr_names, bound, has_self)

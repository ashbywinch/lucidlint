"""The libcst mechanics every fixer shares.

A fixer answers one finding's shape; these are the moves every shape makes —
where a node sits, what a class body binds, how a module's defs are removed or
replaced, which Python constructs are the compiler's shorthand rather than the
domain's. One class, so the fixers speak about CST shape in one vocabulary
instead of restating it per family.
"""

from __future__ import annotations

from typing import override

import libcst as cst

from fix_engine.bindings import Bindings


class CstMechanics:
    """The shared CST shape helpers."""




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
                            Bindings.hoist_target_names(alias.asname.name)
                            if alias.asname is not None
                            else {Bindings.import_base_name(alias.name)}
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
                            Bindings.hoist_target_names(alias.asname.name)
                            if alias.asname is not None
                            else {alias.name.value}
                        )
                        if bound & referenced:
                            moved.append(cst.SimpleStatementLine(body=[inner]))
                            break
        return moved

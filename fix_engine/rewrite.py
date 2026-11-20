"""The edits every fixer makes on one module.

A fixer answers one finding's shape; these are the moves every shape makes —
remove exactly the flagged nodes, synthesize the record class a new type needs,
and name what an extraction introduces. `Rewrite(module)` is the tree being
edited, so the fixers speak about rewriting in one vocabulary instead of
restating it per family.
"""

from __future__ import annotations

import re
from typing import override

import libcst as cst


class Rewrite:
    """One module under edit, and the edits shared by every fixer."""

    def __init__(self, module: cst.Module) -> None:
        self.module: cst.Module = module

    def without(self, targets) -> cst.Module:
        """The module with exactly the given nodes removed (identity)."""
        return self.module.visit(Rewrite._RemoveNodes(targets))

    class _RemoveNodes(cst.CSTTransformer):
        """Remove exactly the given nodes from the tree (identity)."""

        def __init__(self, targets) -> None:
            self.targets: set[cst.CSTNode] = set(targets)

        @override
        def on_leave(self, original_node, updated_node):
            if original_node in self.targets:
                return cst.RemoveFromParent()
            return updated_node

    def without_class(self, name: str) -> cst.Module:
        """The module with the class of that name removed."""
        return self.module.visit(Rewrite._RemoveClass(name))

    class _RemoveClass(cst.CSTTransformer):
        """Delete the class of that name — the husk and the chain both dissolve
        a class once its callers are rewired."""

        def __init__(self, name: str) -> None:
            self.name: str = name

        @override
        def leave_ClassDef(self, original_node, updated_node):
            if original_node.name.value == self.name:
                return cst.RemoveFromParent()
            return updated_node

    @staticmethod
    def renamed(node: cst.Name, old: str, new: str) -> cst.Name:
        """The name with its binding replaced — a rename is one edit, whether the
        agent asked for it (vague-name) or a hoist retargets an accumulator."""
        return node.with_changes(value=new) if node.value == old else node

    @staticmethod
    def self_attr_target(name: str) -> list[cst.AssignTarget]:
        """The `self.<name>` assignment target a record constructor stores."""
        return [cst.AssignTarget(cst.Attribute(cst.Name("self"), cst.Name(name)))]

    @staticmethod
    def record_class_def(class_name: str, field_names: list[str]) -> cst.ClassDef:
        """The record class: __init__ taking the fields, storing them. A class,
        not a NamedTuple — it can grow behavior without a migration."""
        body = [
            cst.SimpleStatementLine(
                body=[
                    cst.Assign(
                        targets=Rewrite.self_attr_target(f),
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
    def pascal_case(text: str) -> str:
        """`folder_fingerprint` -> `FolderFingerprint` (the name a generated
        class takes from the phrase that describes it)."""
        return "".join(part[:1].upper() + part[1:] for part in re.split(r"[^0-9A-Za-z]+", text) if part)

    @staticmethod
    def extraction_is_private() -> bool:
        """Is an extracted function private (leading underscore)? Yes — a fresh
        helper cannot have external callers (it did not exist before the fix),
        so by construction any extraction is an implementation detail of its
        source function. The convention propagates automatically: the seam's
        name gets the underscore whether the agent supplies it or not."""

        return True

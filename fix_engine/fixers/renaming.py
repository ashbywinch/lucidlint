"""The The vague-name fix: the class is renamed where it is declared and read.

One fix family, one class; the class holds the request, its helpers are
members, and its libcst visitors are nested classes.
"""

from __future__ import annotations

from typing import Any, override

import libcst as cst
from libcst.metadata import PositionProvider

from fix_engine.module_outline import ModuleOutline


class Renaming:
    """The rename fix for one request."""

    def __init__(self, request: Any) -> None:
        self.request = request

    def run(self) -> str | None:
        source, line = self.request.loaded_source(), self.request.line
        name = self.request.opts.name
        # the dispatch gate (_fix_structural) refuses name-required kinds without one
        assert isinstance(name, str)
        """Rename (vague-name): the class at `line` plus every same-file Name
        reference. Cross-file call sites need FullRepoManager — same-file v1."""
        module = cst.parse_module(source)
        wrapper = cst.MetadataWrapper(module)
        old: str | None = None

        class _Find(cst.CSTVisitor):
            METADATA_DEPENDENCIES = (PositionProvider,)

            @override
            def visit_ClassDef(self, node) -> None:
                nonlocal old
                if old is None and ModuleOutline.span(self.get_metadata(PositionProvider, node)).start.line == line:
                    old = node.name.value

        wrapper.visit(_Find())
        if old is None or old == name:
            return None
        renamed = wrapper.visit(Renaming._RenameClass(line, old, name)).code
        return None if renamed == source else renamed

    class _RenameClass(cst.CSTTransformer):
        """Rename the class at the line + every Name reference in the file."""

        METADATA_DEPENDENCIES = (PositionProvider,)

        def __init__(self, target_line: int, old: str, new: str) -> None:
            self.target_line: int = target_line
            self.old: str = old
            self.new: str = new

        @override
        def leave_ClassDef(self, original_node, updated_node):
            pos = ModuleOutline.span(self.get_metadata(PositionProvider, original_node))
            if pos.start.line == self.target_line and updated_node.name.value == self.old:
                return updated_node.with_changes(name=cst.Name(self.new))
            return updated_node

        @override
        def leave_Name(self, original_node, updated_node):
            if updated_node.value == self.old:
                return updated_node.with_changes(value=self.new)
            return updated_node

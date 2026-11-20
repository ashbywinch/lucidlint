"""The The magic-number fix: one literal becomes a named class constant.

One fix family, one class; the class holds the request, its helpers are
members, and its libcst visitors are nested classes.
"""

from __future__ import annotations

from typing import Any, override

import libcst as cst
from libcst.metadata import PositionProvider

from fix_engine.module_outline import ModuleOutline


class MagicLiteral:
    """The magic literal fix for one request."""

    def __init__(self, request: Any) -> None:
        self.request = request

    def run(self) -> str | None:
        source, line = self.request._loaded_source(), self.request.line
        name = self.request.opts.name
        # the dispatch gate (_fix_structural) refuses name-required kinds without one
        assert isinstance(name, str)
        """Replace Magic Literal: `f(10, ...)` -> `f(MAX_RETRIES, ...)` with
        `MAX_RETRIES = 10` inserted as a CLASS ATTRIBUTE of the enclosing
        class (third-pass 4: a module-top constant would be its own
        global-state finding — values are a class's private internals).
        A literal with no class to own its constant (module-level function)
        is REFUSED, never parked at module top."""
        module = cst.parse_module(source)
        wrapper = cst.MetadataWrapper(module)
        value: str | None = None

        finder = MagicLiteral._FindLiteral(line, self.request.col)
        wrapper.visit(finder)
        if finder.value is None:
            return None
        value = finder.value
        replaced = wrapper.visit(MagicLiteral._ReplaceLiteral(line, self.request.col, name)).code
        if replaced == source:
            return None
        module2 = cst.parse_module(replaced)
        # the refusal probe must run on a class OWNING the literal — the
        # replaced tree has the same line map, so one probe serves both
        probe = MagicLiteral._ClassOwningLine(line)
        cst.MetadataWrapper(module2).visit(probe)
        if probe.owner is None:
            # no class to own the constant — refuse loudly (never module top)
            self.request.decline = (
                f"the literal at {self.request.rel}:{line} has no class to own its constant — "
                "move the function into a class first"
            )
            return None
        assignment = cst.SimpleStatementLine(
            body=[
                cst.Assign(
                    targets=[cst.AssignTarget(cst.Name(name))],
                    value=cst.parse_expression(value),
                )
            ]
        )
        wrapped = cst.MetadataWrapper(module2)
        result = wrapped.visit(MagicLiteral._InsertClassConstant(line, assignment))
        return result.code

    class _ClassOwningLine(cst.CSTVisitor):
        """The innermost ClassDef whose span contains `line` — magic-number's
        constant home (third-pass 4). None when the literal lives outside any
        class (module-level function): the fixer refuses instead of parking the
        constant at module top (its own global-state finding)."""

        METADATA_DEPENDENCIES = (PositionProvider,)

        def __init__(self, line: int) -> None:
            self.line: int = line
            self.depth: int = 0
            self.owner: cst.ClassDef | None = None
            self.owner_depth: int = -1

        @override
        def visit_ClassDef(self, node) -> None:
            self.depth += 1
            pos = ModuleOutline.span(self.get_metadata(PositionProvider, node))
            if pos.start.line <= self.line <= pos.end.line and self.depth > self.owner_depth:
                self.owner = node
                self.owner_depth = self.depth

        @override
        def leave_ClassDef(self, original_node) -> None:
            self.depth -= 1

    class _FindLiteral(cst.CSTVisitor):
        """The numeric literal anchored at (line, col) — the schema-3 col pins
        same-line twins so the fix never rewrites the wrong operand."""

        METADATA_DEPENDENCIES = (PositionProvider,)

        def __init__(self, line: int, col: int) -> None:
            self.line: int = line
            self.col: int = col
            self.value: str | None = None

        def _match(self, node) -> bool:
            if self.value is not None:
                return False
            pos = ModuleOutline.span(self.get_metadata(PositionProvider, node)).start
            if pos.line != self.line:
                return False
            if self.col > 0 and pos.column + 1 != self.col:
                return False
            self.value = node.value
            return True

        @override
        def visit_Integer(self, node) -> None:
            self._match(node)

        @override
        def visit_Float(self, node) -> None:
            self._match(node)

    class _InsertClassConstant(cst.CSTTransformer):
        """Insert the magic-number constant as a CLASS ATTRIBUTE of the
        innermost class owning the literal — after the docstring, opens the real
        body. Leaves the innermost owner only (leave order is innermost-first),
        so nested classes do not double-receive."""

        METADATA_DEPENDENCIES = (PositionProvider,)

        def __init__(self, line: int, assignment: cst.SimpleStatementLine) -> None:
            self.line: int = line
            self.assignment: cst.SimpleStatementLine = assignment
            self.done: bool = False

        @override
        def leave_ClassDef(self, original_node, updated_node):
            if self.done:
                return updated_node
            pos = ModuleOutline.span(self.get_metadata(PositionProvider, original_node))
            if not (pos.start.line <= self.line <= pos.end.line):
                return updated_node
            self.done = True
            class_body = list(updated_node.body.body)
            insert_at = 0
            if MagicLiteral._is_docstring_only_body(class_body):
                insert_at = 1
            class_body.insert(insert_at, self.assignment)
            return updated_node.with_changes(body=updated_node.body.with_changes(body=class_body))

    class _ReplaceLiteral(cst.CSTTransformer):
        """Replace the numeric literal on the target line with a name."""

        METADATA_DEPENDENCIES = (PositionProvider,)

        def __init__(self, target_line: int, col: int, name: str) -> None:
            self.target_line: int = target_line
            self.col: int = col
            self.name: str = name
            self.replaced: bool = False

        def _match(self, original_node) -> bool:
            if self.replaced:
                return False
            pos = ModuleOutline.span(self.get_metadata(PositionProvider, original_node)).start
            return pos.line == self.target_line and (self.col == 0 or pos.column + 1 == self.col)

        @override
        def leave_Integer(self, original_node, updated_node):
            if self._match(original_node):
                self.replaced = True
                return cst.Name(self.name)
            return updated_node

        @override
        def leave_Float(self, original_node, updated_node):
            if self._match(original_node):
                self.replaced = True
                return cst.Name(self.name)
            return updated_node

    @staticmethod
    def _is_docstring_only_body(class_body: list) -> bool:
        """The class body is a single docstring — no members to carry a record."""
        return (
            bool(class_body)
            and isinstance(class_body[0], cst.SimpleStatementLine)
            and len(class_body[0].body) == 1
            and isinstance(class_body[0].body[0], cst.Expr)
            and isinstance(class_body[0].body[0].value, cst.SimpleString)
        )

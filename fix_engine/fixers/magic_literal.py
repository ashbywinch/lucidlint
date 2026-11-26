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
        self.request: Any = request

    def run(self) -> str | None:
        source, line = self.request.loaded_source(), self.request.line
        name = self.request.opts.name
        # the dispatch gate (_fix_structural) refuses name-required kinds without one
        assert isinstance(name, str)
        """Replace Magic Literal: `f(10, ...)` -> `f(MAX_RETRIES, ...)` with
        `MAX_RETRIES = 10` inserted as a CLASS ATTRIBUTE of the enclosing
        class (third-pass 4: a module-top constant would be its own
        global-state finding — values are a class's private internals).
        A literal with no class to own its constant (module-level function)
        is REFUSED, never parked at module top. The reference is qualified for
        the scope it sits in — self/cls/the class name — because a bare name
        inside a method is a NameError."""
        module = cst.parse_module(source)
        wrapper = cst.MetadataWrapper(module)

        finder = MagicLiteral._FindLiteral(line, self.request.col)
        wrapper.visit(finder)
        if finder.value is None:
            return None
        value = finder.value
        # the refusal probe must run on a class OWNING the literal — the
        # replaced tree has the same line map, so one probe serves both
        probe = MagicLiteral._ClassOwningLine(line)
        wrapper.visit(probe)
        if probe.owner is None:
            # no class to own the constant — refuse loudly (never module top)
            self.request.decline = (
                f"the literal at {self.request.rel}:{line} has no class to own its constant — "
                "move the function into a class first"
            )
            return None
        if finder.literal_line < probe.owner_name_line:
            self.request.decline = (
                "runs in the class decorator, before the class body defines the constant — "
                "name the value inside the decorator instead"
            )
            return None
        scope = MagicLiteral._ReferenceScope(finder.literal_line, finder.literal_col, probe.owner_depth)
        wrapper.visit(scope)
        qualifier = scope.qualifier(probe.owner.name.value)
        if scope.decline:
            self.request.decline = f"the literal at {self.request.rel}:{line} {scope.decline}"
            return None
        replaced = wrapper.visit(MagicLiteral._ReplaceLiteral(line, self.request.col, name, qualifier)).code
        if replaced == source:
            return None
        module2 = cst.parse_module(replaced)
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
            self.owner_name_line: int = 0

        @override
        def visit_ClassDef(self, node) -> None:
            self.depth += 1
            pos = ModuleOutline.span(self.get_metadata(PositionProvider, node))
            if pos.start.line <= self.line <= pos.end.line and self.depth > self.owner_depth:
                self.owner = node
                self.owner_depth = self.depth
                self.owner_name_line = ModuleOutline.span(self.get_metadata(PositionProvider, node.name)).start.line

        @override
        def leave_ClassDef(self, original_node) -> None:
            self.depth -= 1

    class _ReferenceScope(cst.CSTVisitor):
        """The receiver that reaches the class constant: the chain of functions
        and lambdas whose BODY contains the literal, inside the owning class. A
        classmethod reads it through its first parameter (cls by convention), an
        instance method through self, and a static method or lambda through the
        class name. A literal in the class body itself (a default argument, a
        decorator) already runs in class scope and keeps the bare name."""

        METADATA_DEPENDENCIES = (PositionProvider,)

        def __init__(self, line: int, col: int, owner_depth: int) -> None:
            self.line: int = line
            self.col: int = col  # libcst's 0-based column of the literal
            self.owner_depth: int = owner_depth
            self.chain: list[cst.FunctionDef | cst.Lambda] = []
            self.decline: str = ""

        @override
        def visit_ClassDef(self, node) -> None:
            # a nested class (even one defined inside a method) starts a new
            # scope: only functions inside THIS class can qualify the constant
            if self._contains(node):
                self.chain = []

        @override
        def visit_FunctionDef(self, node) -> None:
            if self._in_body(node, node.body):
                self.chain.append(node)

        @override
        def visit_Lambda(self, node) -> None:
            if self._in_body(node, node.body):
                self.chain.append(node)

        def qualifier(self, owner_name: str) -> str | None:
            """The expression prefix for the constant, or None for a bare name."""
            if not self.chain:
                return None
            head = self.chain[0]
            if isinstance(head, cst.FunctionDef):
                params = list(head.params.posonly_params) + list(head.params.params)
                receiver = params[0].name.value if params else ""
                if self._decorated(head, "classmethod"):
                    return receiver or "cls"
                if receiver == "self" and not self._decorated(head, "staticmethod"):
                    return "self"
            if self.owner_depth > 1:
                self.decline = (
                    "sits in a static method of a class nested inside another class — the class name "
                    "is not in scope there, so the constant needs an explicit qualified name this "
                    "fixer cannot choose; name it where the computation uses it"
                )
                return None
            return owner_name

        def _contains(self, node) -> bool:
            pos = ModuleOutline.span(self.get_metadata(PositionProvider, node))
            return (pos.start.line, pos.start.column) <= (self.line, self.col) <= (pos.end.line, pos.end.column)

        def _in_body(self, node, body) -> bool:
            start = ModuleOutline.span(self.get_metadata(PositionProvider, body)).start
            end = ModuleOutline.span(self.get_metadata(PositionProvider, node)).end
            return (start.line, start.column) <= (self.line, self.col) <= (end.line, end.column)

        @staticmethod
        def _decorated(node: cst.FunctionDef, name: str) -> bool:
            return any(
                (isinstance(d.decorator, cst.Name) and d.decorator.value == name)
                or (isinstance(d.decorator, cst.Attribute) and d.decorator.attr.value == name)
                for d in node.decorators
            )

    class _FindLiteral(cst.CSTVisitor):
        """The numeric literal anchored at (line, col) — the schema-3 col pins
        same-line twins so the fix never rewrites the wrong operand."""

        METADATA_DEPENDENCIES = (PositionProvider,)

        def __init__(self, line: int, col: int) -> None:
            self.line: int = line
            self.col: int = col
            self.value: str | None = None
            self.literal_line: int = 0
            self.literal_col: int = 0

        def _match(self, node) -> bool:
            if self.value is not None:
                return False
            pos = ModuleOutline.span(self.get_metadata(PositionProvider, node)).start
            if pos.line != self.line:
                return False
            if self.col > 0 and pos.column + 1 != self.col:
                return False
            self.value = node.value
            self.literal_line = pos.line
            self.literal_col = pos.column
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
        """Replace the numeric literal on the target line with the constant's
        name, qualified by the receiver that reaches it (self.LIMIT, cls.LIMIT,
        Step.LIMIT) or bare when the use already runs in class scope."""

        METADATA_DEPENDENCIES = (PositionProvider,)

        def __init__(self, target_line: int, col: int, name: str, qualifier: str | None) -> None:
            self.target_line: int = target_line
            self.col: int = col
            self.name: str = name
            self.qualifier: str | None = qualifier
            self.replaced: bool = False

        def _match(self, original_node) -> bool:
            if self.replaced:
                return False
            pos = ModuleOutline.span(self.get_metadata(PositionProvider, original_node)).start
            return pos.line == self.target_line and (self.col == 0 or pos.column + 1 == self.col)

        def _reference(self) -> cst.BaseExpression:
            if self.qualifier is None:
                return cst.Name(self.name)
            return cst.Attribute(value=cst.Name(self.qualifier), attr=cst.Name(self.name))

        @override
        def leave_Integer(self, original_node, updated_node):
            if not self._match(original_node):
                return updated_node
            self.replaced = True
            return self._reference()

        # libcst's protocol has no shared `leave_BaseNumber`: the two literal
        # kinds each get their own hook with the same one-line body
        @override
        def leave_Float(self, original_node, updated_node):
            if not self._match(original_node):
                return updated_node
            self.replaced = True
            return self._reference()

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

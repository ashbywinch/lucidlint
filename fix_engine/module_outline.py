"""Where things are in one parsed module.

The lookups every fixer makes: a node's span, the class or function that
encloses a line, the function defined at a line, whether a node is a keyword
argument or a docstring, and a walk over the nodes a statement introduces. The
outline is one module's wrapper, so the visitors are its own.
"""

from __future__ import annotations

from typing import TypeGuard, override

import libcst as cst
from libcst.metadata import CodeRange, PositionProvider


class ModuleOutline:
    """One parsed module, queried by position."""

    def __init__(self, wrapper: cst.MetadataWrapper) -> None:
        self.wrapper = wrapper
        self.module = wrapper.module

    def _find(self, probe):
        """Run one probe visitor over the tree and return what it found."""
        self.wrapper.visit(probe)
        return probe.found

    def enclosing_class(self, line: int) -> cst.ClassDef | None:
        """The innermost class whose span contains `line`."""
        return self._find(ModuleOutline._Enclosing(line, cst.ClassDef))

    def enclosing_function(self, line: int) -> cst.FunctionDef | None:
        """The innermost function whose span contains `line`."""
        return self._find(ModuleOutline._Enclosing(line, cst.FunctionDef))

    def function_at(self, line: int) -> cst.FunctionDef | None:
        """The module-level function whose def line is `line`."""
        return self._find(ModuleOutline._FindFnLine(line))

    def contains(self, fn: cst.FunctionDef | None) -> bool:
        """Is the function still a direct child of the module?"""
        return fn is not None and any(statement is fn for statement in self.module.body)

    @staticmethod
    def span(pos: object) -> CodeRange:
        """The resolved metadata position. libcst's get_metadata types its result
        through an UNDEFINED sentinel default — narrow to the CodeRange that is
        always present for nodes visited inside a MetadataWrapper."""
        assert isinstance(pos, CodeRange)
        return pos

    @staticmethod
    def resolve_node(node: object) -> cst.CSTNode:
        """The resolved metadata node — the same UNDEFINED-sentinel widening as
        _as_range, for providers that return nodes (ParentNodeProvider)."""
        assert isinstance(node, cst.CSTNode)
        return node

    @staticmethod
    def is_keyword_name(node, parent) -> bool:
        """A Name that is a keyword-argument's keyword (`f(a=1)` — the `a`) is
        not a variable reference; counting it made the seam analysis fabricate
        phantom parameters (the `leading_lines` in `with_changes(leading_lines=[])`
        would appear as a free var and the rewritten call would NameError)."""
        return isinstance(parent, cst.Arg) and parent.keyword is node

    @staticmethod
    def is_self_attribute(value: cst.BaseExpression) -> bool:
        """`self.<attr>` — an attribute read on the receiver."""
        return isinstance(value, cst.Attribute) and isinstance(value.value, cst.Name) and value.value.value == "self"

    @staticmethod
    def is_empty(source: str) -> bool:
        """Does the module source hold nothing a reader would keep — no def, no
        class, no statement, only comments and blank lines? The emptied file a
        split deletes."""
        module = cst.parse_module(source)
        return all(not isinstance(s, (cst.FunctionDef, cst.ClassDef, cst.SimpleStatementLine)) for s in module.body)

    @staticmethod
    def is_docstring_stmt(
        stmt: cst.BaseSmallStatement | cst.BaseStatement,
    ) -> TypeGuard[cst.SimpleStatementLine]:
        return (
            isinstance(stmt, cst.SimpleStatementLine)
            and len(stmt.body) == 1
            and isinstance(stmt.body[0], cst.Expr)
            and isinstance(stmt.body[0].value, (cst.SimpleString, cst.ConcatenatedString))
        )

    @staticmethod
    def iter_nodes(nodes):
        """Every node in the subtree, parents before children; accepts a node
        or an iterable of nodes."""
        if isinstance(nodes, cst.CSTNode):
            nodes = [nodes]
        for n in nodes:
            yield n
            for c in n.children:
                yield from ModuleOutline.iter_nodes(c)

    @staticmethod
    def iter_nodes_pruned(nodes):
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
                yield from ModuleOutline.iter_nodes_pruned(c)

    class _Enclosing(cst.CSTVisitor):
        """The innermost def or class of the asked-for kind whose span contains
        the line — the method's owner, or the member assignment's constructor."""

        METADATA_DEPENDENCIES = (PositionProvider,)

        def __init__(self, line: int, node_type: type) -> None:
            self.line: int = line
            self.node_type: type = node_type
            self.found: cst.ClassDef | cst.FunctionDef | None = None

        @override
        def visit_ClassDef(self, node) -> None:
            self._consider(node)

        @override
        def visit_FunctionDef(self, node) -> None:
            self._consider(node)

        def _consider(self, node) -> None:
            if not isinstance(node, self.node_type):
                return
            pos = ModuleOutline.span(self.get_metadata(PositionProvider, node))
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
                and ModuleOutline.span(self.get_metadata(PositionProvider, node)).start.line == self.line
            ):
                self.found = node

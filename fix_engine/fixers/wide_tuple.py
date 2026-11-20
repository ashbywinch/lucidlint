"""The wide-tuple shape arm: a fixed-arity tuple annotation becomes a record.

One fix family, one class; the class holds the request, its helpers are
members, and its libcst visitors are nested classes.
"""

from __future__ import annotations

import keyword
from typing import NamedTuple, override

import libcst as cst
from libcst.metadata import ParentNodeProvider, PositionProvider

from fix_engine.bindings import Bindings
from fix_engine.module_outline import ModuleOutline
from fix_engine.rewrite import Rewrite


class WideTuple:
    # a tuple needs this many positions before a record names anything: a pair
    # reads fine positionally
    _MIN_RECORD_ARITY = 3

    """The wide-tuple fix for one request: the annotation and its build sites
    become a named record class."""

    class _Site(NamedTuple):
        """One fixed-arity tuple annotation on the anchor line: where it lives,
        the annotated name, its arity, and the build site's element expressions
        (None when the annotation has no matching tuple literal)."""

        where: str
        var: str
        arity: int
        annotation: cst.BaseExpression
        elements: tuple[cst.BaseExpression, ...] | None

    def __init__(self, request) -> None:
        self.request = request

    def run(self) -> str | None:
        """Introduce the record class for the flagged fixed-arity tuple: the
        annotation (and every same-shape annotation in the file) becomes the
        class, matching tuple build sites construct it, and the class is
        prepended. Declines on an ambiguous anchor, an unusable name, or a name
        the file already binds."""
        source, line = self.request.loaded_source(), self.request.line
        wrapper = cst.MetadataWrapper(cst.parse_module(source))
        finder = WideTuple._FindWideTuples(line)
        wrapper.visit(finder)
        if not finder.sites:
            self.request.decline = (
                f"no fixed-arity tuple annotation (3+ named positions) at {self.request.rel}:{line} — "
                "the anchor is stale, or the annotation is variadic (tuple[T, ...])"
            )
            return None
        if len(finder.sites) > 1:
            self.request.decline = (
                f"{len(finder.sites)} wide-tuple annotations anchor at {self.request.rel}:{line} — the "
                "anchor is ambiguous; put each on its own line, or retarget one by hand"
            )
            return None
        site = finder.sites[0]
        # a bare ANNOTATION carries no variable to name the record after: the
        # parameter's own name, or `<Fn>Result` for a return annotation
        derived = Rewrite.pascal_case(site.var) + ("" if site.where != "return" else "Result")
        class_name = self.request.opts.name or derived
        if not class_name.isidentifier() or keyword.iskeyword(class_name):
            self.request.decline = f"'{class_name}' is not a usable record class name — pass --name <Name>"
            return None
        if Bindings.binds_name(wrapper.module, class_name):
            self.request.decline = f"'{class_name}' is already bound in {self.request.rel} — pass another --name"
            return None
        fields = WideTuple._record_field_names(site.elements, site.arity)
        transformed = wrapper.visit(
            WideTuple._WideTupleToRecord(WideTuple._annotation_text(site.annotation), site.arity, class_name)
        )
        body: list = [Rewrite.record_class_def(class_name, fields), cst.EmptyLine(), *transformed.body]
        return cst.Module(body=body).code

    class _FindWideTuples(cst.CSTVisitor):
        """Every wide-tuple annotation anchored on the finding's line — the fixer
        declines when the line carries more than one (it cannot pick)."""

        METADATA_DEPENDENCIES = (PositionProvider,)

        def __init__(self, line: int) -> None:
            self.line: int = line
            self.sites: list[WideTuple._Site] = []

        @override
        def visit_AnnAssign(self, node) -> None:
            if ModuleOutline.span(self.get_metadata(PositionProvider, node)).start.line != self.line:
                return
            if not isinstance(node.target, cst.Name):
                return
            arity = WideTuple._fixed_tuple_arity(node.annotation.annotation)
            if arity is None or arity < WideTuple._MIN_RECORD_ARITY:
                return
            elements = None
            if WideTuple._is_arity_tuple(node.value, arity):
                assert isinstance(node.value, cst.Tuple)
                elements = tuple(element.value for element in node.value.elements)
            self.sites.append(WideTuple._Site("assign", node.target.value, arity, node.annotation.annotation, elements))

        @override
        def visit_FunctionDef(self, node) -> None:
            if ModuleOutline.span(self.get_metadata(PositionProvider, node)).start.line != self.line:
                return
            for param in [*node.params.posonly_params, *node.params.params, *node.params.kwonly_params]:
                if param.annotation is None:
                    continue
                arity = WideTuple._fixed_tuple_arity(param.annotation.annotation)
                if arity is not None and arity >= WideTuple._MIN_RECORD_ARITY:
                    self.sites.append(
                        WideTuple._Site("param", param.name.value, arity, param.annotation.annotation, None)
                    )
            if node.returns is not None:
                arity = WideTuple._fixed_tuple_arity(node.returns.annotation)
                if arity is not None and arity >= WideTuple._MIN_RECORD_ARITY:
                    self.sites.append(WideTuple._Site("return", node.name.value, arity, node.returns.annotation, None))

    @staticmethod
    def _fixed_tuple_arity(node: cst.BaseExpression | None) -> int | None:
        """The arity of a fixed-arity `tuple[...]`/`Tuple[...]` annotation; None
        for anything else (including the variadic `tuple[int, ...]`)."""
        if not WideTuple._is_tuple_subscript(node):
            return None
        assert isinstance(node, cst.Subscript)
        elements = node.slice if isinstance(node.slice, tuple) else (node.slice,)
        for element in elements:
            if not isinstance(element, cst.SubscriptElement):
                return None
            index = element.slice
            if not isinstance(index, cst.Index) or isinstance(index.value, cst.Ellipsis):
                return None
        return len(elements)

    @staticmethod
    def _record_field_names(elements: tuple[cst.BaseExpression, ...] | None, arity: int) -> list[str]:
        """The record's field names: a Name element keeps its name, an attribute
        access takes the attribute (the value IS that field), and a literal, a
        keyword, or a duplicate falls back to `f<i>` — the agent renames what the
        shape alone cannot name."""
        names: list[str] = []
        for i in range(arity):
            element = elements[i] if elements is not None and i < len(elements) else None
            name = ""
            if isinstance(element, cst.Name):
                name = element.value
            elif isinstance(element, cst.Attribute):
                name = element.attr.value
            if WideTuple._field_name_taken_by_python(name, names):
                name = f"f{i}"
            names.append(name)
        return names

    class _WideTupleToRecord(cst.CSTTransformer):
        """Retarget every annotation in the file spelling the same tuple shape at
        the record class, and every matching tuple build site at its constructor
        — the shape (arity + element types) is the proof of sameness. A function
        annotated as returning the tuple has its own matching return/assignment
        literals retargeted too (position proves them); elsewhere nothing
        changes."""

        METADATA_DEPENDENCIES = (ParentNodeProvider,)

        def __init__(self, ann_text: str, arity: int, class_name: str) -> None:
            self.ann_text: str = ann_text
            self.arity: int = arity
            self.class_name: str = class_name
            self._returns_shape: list[bool] = []

        def _matches(self, annotation: cst.Annotation) -> bool:
            return WideTuple._annotation_text(annotation.annotation) == self.ann_text

        def _retarget(self, annotation: cst.Annotation) -> cst.Annotation:
            return annotation.with_changes(annotation=cst.Name(self.class_name))

        def _constructor(self, value: cst.BaseExpression) -> cst.BaseExpression | None:
            if not isinstance(value, cst.Tuple) or len(value.elements) != self.arity:
                return None
            args = [cst.Arg(element.value) for element in value.elements if isinstance(element, cst.Element)]
            if len(args) != self.arity:
                return None
            return cst.Call(func=cst.Name(self.class_name), args=args)

        @override
        def leave_AnnAssign(self, original_node, updated_node):
            if not self._matches(updated_node.annotation):
                return updated_node
            annotation = self._retarget(updated_node.annotation)
            if updated_node.value is None:
                return updated_node.with_changes(annotation=annotation)
            value = self._constructor(updated_node.value)
            if value is None:
                return updated_node.with_changes(annotation=annotation)
            return updated_node.with_changes(annotation=annotation, value=value)

        @override
        def leave_Param(self, original_node, updated_node):
            if updated_node.annotation is None or not self._matches(updated_node.annotation):
                return updated_node
            return updated_node.with_changes(annotation=self._retarget(updated_node.annotation))

        @override
        def visit_FunctionDef(self, node) -> None:
            self._returns_shape.append(node.returns is not None and self._matches(node.returns))

        @override
        def leave_FunctionDef(self, original_node, updated_node):
            self._returns_shape.pop()
            if updated_node.returns is None or not self._matches(updated_node.returns):
                return updated_node
            return updated_node.with_changes(returns=self._retarget(updated_node.returns))

        @override
        def leave_Tuple(self, original_node, updated_node):
            if not self._returns_shape or not self._returns_shape[-1]:
                return updated_node
            parent = self.get_metadata(ParentNodeProvider, original_node)
            if not isinstance(parent, (cst.Return, cst.Assign, cst.AnnAssign)):
                return updated_node
            return self._constructor(updated_node) or updated_node

    @staticmethod
    def _annotation_text(expr: cst.BaseExpression) -> str:
        """The annotation's generated text, so two spellings of one shape compare
        equal."""
        return cst.Module(body=[]).code_for_node(expr)

    @staticmethod
    def _is_arity_tuple(value: cst.BaseExpression | None, arity: int) -> bool:
        """A tuple literal with exactly `arity` plain elements."""
        return (
            isinstance(value, cst.Tuple)
            and len(value.elements) == arity
            and all(isinstance(element, cst.Element) for element in value.elements)
        )

    @staticmethod
    def _is_tuple_subscript(node: cst.BaseExpression | None) -> bool:
        """`tuple[...]` / `Tuple[...]` — the annotation that hides a record."""
        return (
            isinstance(node, cst.Subscript)
            and isinstance(node.value, cst.Name)
            and node.value.value in ("tuple", "Tuple")
        )

    @staticmethod
    def _field_name_taken_by_python(name: str, names: list[str]) -> bool:
        """The field name is not a usable Python identifier, or is already used."""
        return not name.isidentifier() or keyword.iskeyword(name) or name in names

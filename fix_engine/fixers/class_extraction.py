"""The strewing fix: free functions that share a leading receiver become a class.

One fix family, one class; the class holds the request, its helpers are
members, and its libcst visitors are nested classes.
"""

from __future__ import annotations

from typing import override

import libcst as cst
from libcst.metadata import ExpressionContextProvider, ParentNodeProvider, PositionProvider

from fix_engine.bindings import Bindings, StoredNames
from fix_engine.module_outline import ModuleOutline
from fix_engine.strewing_group import StrewingGroup


class ClassExtraction:
    """The strewing fix for one request: the group's functions move into the
    class named after their shared receiver."""

    def __init__(self, request) -> None:
        self.request = request

    def run(self) -> str | None:
        """Move the strewing group into a class named `name` (default: the shared
        leading type). Returns the new source, or None when the finding is stale
        or the group is not auto-fixable."""
        source, line = self.request._loaded_source(), self.request.line
        name = self.request.opts.name
        module = cst.parse_module(source)
        wrapper = cst.MetadataWrapper(module)
        found = ClassExtraction._strewing_group(source, line)
        if found is None:
            return None
        shared = found.shared
        fns = found.fns
        class_name = name or shared

        # pass 1: rewrite the call sites WITHOUT deleting — the moved bodies must
        # be collected from this pass so their inter-fn calls are method calls
        # (`_window_score(state, ...)` -> `state._window_score(...)`)
        call_rewritten = wrapper.visit(ClassExtraction._MoveIntoClass(set(fns), delete=False)).code

        # pass 2: per-function receiver rename (each fn's OWN first param, LOAD
        # references only), then swap the first param to `self`
        methods = ClassExtraction._collect_defs(cst.parse_module(call_rewritten), set(fns))
        receivers = {m.name.value: (m.params.params[0].name.value if m.params.params else None) for m in methods}
        mod2 = cst.parse_module(call_rewritten)
        wrap2 = cst.MetadataWrapper(mod2)
        renamed = wrap2.visit(ClassExtraction._ReceiverToSelf(receivers, Bindings.stores_by_function(wrap2)))
        new_methods = []
        for method in ClassExtraction._collect_defs(renamed, set(fns)):
            params = method.params.params
            new_methods.append(
                method.with_changes(
                    params=method.params.with_changes(
                        params=[cst.Param(name=cst.Name("self"), annotation=None), *params[1:]]
                    ),
                    body=method.body,
                )
            )

        # pass 3: delete the free fns (call sites already rewritten)
        deleted = wrapper.visit(ClassExtraction._MoveIntoClass(set(fns))).code
        classdef = cst.ClassDef(
            name=cst.Name(class_name),
            bases=[],
            body=cst.IndentedBlock(body=list(new_methods)),
        )
        return cst.parse_module(deleted).visit(ClassExtraction._InsertClass(class_name, classdef, new_methods)).code

    class _MoveIntoClass(cst.CSTTransformer):
        """Delete the strewing free functions from module scope; rewrite
        same-file call sites `fn(recv, ...)` -> `recv.fn(...)`."""

        METADATA_DEPENDENCIES = (PositionProvider,)

        def __init__(self, fns: set[str], delete: bool = True) -> None:
            self.fns: set[str] = fns
            self.delete: bool = delete

        @override
        def leave_FunctionDef(self, original_node, updated_node):
            if self.delete and updated_node.name.value in self.fns:
                return cst.RemoveFromParent()
            return updated_node

        @override
        def leave_Call(self, original_node, updated_node):
            if not isinstance(updated_node.func, cst.Name):
                return updated_node
            name = updated_node.func.value
            if name not in self.fns or not updated_node.args:
                return updated_node
            receiver, *rest = updated_node.args
            return updated_node.with_changes(
                func=cst.Attribute(
                    value=receiver.value,
                    attr=cst.Name(name),
                ),
                args=rest,
            )

    class _ReceiverToSelf(cst.CSTTransformer):
        """The extract-class receiver rename: in each moved function, LOAD
        references to THAT function's own first parameter become `self`.
        Per-function (different strewing fns may name their receiver
        differently) and shadow-aware — a name stored anywhere in the body
        is a local and is never renamed, so `state = state.line` survives
        intact instead of becoming `self = self.line`."""

        METADATA_DEPENDENCIES = (ExpressionContextProvider, ParentNodeProvider)

        def __init__(self, receivers: dict[str, str | None], shadowed: StoredNames) -> None:
            self.receivers: dict[str, str | None] = receivers  # fn name -> its receiver param name
            self.shadowed: StoredNames = shadowed  # fn name -> names stored in its body
            self._current: str | None = None
            self._fn_stack: list[str | None] = []

        @override
        def visit_FunctionDef(self, node) -> None:
            self._fn_stack.append(self._current)
            self._current = node.name.value

        @override
        def leave_FunctionDef(self, original_node, updated_node):
            self._current = self._fn_stack.pop()
            return updated_node

        @override
        def leave_Name(self, original_node, updated_node):
            fn = self._current or ""
            old = self.receivers.get(fn)
            parent = self.get_metadata(ParentNodeProvider, original_node)
            if ModuleOutline.is_keyword_name(original_node, parent):
                return updated_node  # f(state=1): a keyword name, not the receiver
            if old is None or updated_node.value != old:
                return updated_node
            if updated_node.value in self.shadowed.get(fn, set()):
                return updated_node  # a local shadows the receiver — not the param
            try:
                ctx = self.get_metadata(ExpressionContextProvider, original_node)
            except KeyError:
                return updated_node  # attribute names (obj.state) are not refs
            if ctx == cst.metadata.ExpressionContext.LOAD:
                return updated_node.with_changes(value="self")
            return updated_node

    class _InsertClass(cst.CSTTransformer):
        """Append the methods to the existing class, or insert a new class after
        the last one."""

        def __init__(self, class_name, classdef, methods) -> None:
            self.class_name: str = class_name
            self.classdef: cst.ClassDef = classdef
            self.methods: list[cst.FunctionDef] = methods

        @override
        def leave_ClassDef(self, original_node, updated_node):
            if updated_node.name.value == self.class_name:
                existing = list(updated_node.body.body)
                existing.extend(self.methods)
                return updated_node.with_changes(body=updated_node.body.with_changes(body=existing))
            return updated_node

        @override
        def leave_Module(self, original_node, updated_node):
            if self.class_name in {s.name.value for s in updated_node.body if isinstance(s, cst.ClassDef)}:
                return updated_node
            body = list(updated_node.body)
            idx = max(
                (i for i, s in enumerate(body) if isinstance(s, cst.ClassDef)),
                default=-1,
            )
            body.insert(idx + 1, self.classdef)
            return updated_node.with_changes(body=body)

    @staticmethod
    def _annotation_base(node) -> str | None:
        """The base name of a first-param annotation: `contract: GraphContract`
        -> "GraphContract"; unannotated or non-name -> None."""
        if node.annotation is None:
            return None
        ann = node.annotation.annotation
        if isinstance(ann, cst.Name):
            return ann.value
        return None

    @staticmethod
    def _strewing_group(source: str, anchor_line: int) -> StrewingGroup | None:
        """The (shared type, fn names, fn anchor line) for the strewing group
        anchored at `anchor_line` — mirroring the scanner's rule: >=3 module-level
        defs sharing a leading param annotated with a file-local class. None when
        the finding is stale or not auto-fixable."""
        module = cst.parse_module(source)
        wrapper = cst.MetadataWrapper(module)
        local_classes: set[str] = set()
        anchor_ann: str | None = None

        class _Scan(cst.CSTVisitor):
            METADATA_DEPENDENCIES = (PositionProvider,)

            @override
            def visit_ClassDef(self, node) -> None:
                local_classes.add(node.name.value)

            @override
            def visit_FunctionDef(self, node) -> None:
                nonlocal anchor_ann
                if node.name.value == "__init__":
                    return
                pos = ModuleOutline.span(self.get_metadata(PositionProvider, node))
                if pos.start.line == anchor_line and node.params.params:
                    anchor_ann = ClassExtraction._annotation_base(node.params.params[0])

        wrapper.visit(_Scan())
        if anchor_ann is None or anchor_ann not in local_classes:
            return None

        fns: list[str] = []

        class _Group(cst.CSTVisitor):
            @override
            def visit_FunctionDef(self, node) -> None:
                if node.name.value == "__init__":
                    return
                if not node.params.params:
                    return
                if ClassExtraction._annotation_base(node.params.params[0]) == anchor_ann:
                    fns.append(node.name.value)

        module.visit(_Group())
        if len(fns) < 3:
            return None
        return StrewingGroup(shared=anchor_ann, fns=fns, anchor=anchor_line)

    @staticmethod
    def _collect_defs(module: cst.Module, fns: set[str]) -> list[cst.FunctionDef]:
        """The group's def nodes, in source order."""
        methods: list[cst.FunctionDef] = []

        class _Collect(cst.CSTVisitor):
            @override
            def visit_FunctionDef(self, node) -> None:
                if node.name.value in fns:
                    methods.append(node)

        module.visit(_Collect())
        return methods

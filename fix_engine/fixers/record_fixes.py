"""The record fixers: the anonymous tuple and the constant-key dict.

Both turn an implicit record into a named class. The class holds the request;
the finders and visitors are its members, because they are how this one fix
sees the file.
"""

from __future__ import annotations

from dataclasses import dataclass
from typing import Any, NamedTuple, override

import libcst as cst
from libcst.metadata import PositionProvider

from fix_engine.module_outline import ModuleOutline
from fix_engine.rewrite import Rewrite


class RecordFixes:
    """The two record fixes for one request: tuple-record and
    extract-record-class."""

    @dataclass
    class _RecordBuild:
        """The record's dict names + the first build's elements (for the field
        names) — a named return instead of a bare tuple."""

        names: set[str]
        elements: list[cst.BaseExpression] | None

    class _RecordDictHit(NamedTuple):
        """One candidate literal: anchor position + the node + its const keys."""

        line: int
        col: int
        node: cst.Dict
        keys: list[str]

    def __init__(self, request: Any) -> None:
        self.request = request

    def tuple_record(self) -> str | None:
        """The anonymous tuple record becomes a class: the build sites
        construct it, the constant-index reads and destructures become
        attribute reads, and the class definition is prepended to the
        module."""
        source = self.request.loaded_source()
        class_name = self.request.opts.name
        if not class_name:
            return None
        if not class_name.startswith("_"):
            class_name = "_" + class_name
        module = cst.parse_module(source)
        wrapper = cst.MetadataWrapper(module)
        # the record dict + the tuple arity from the build site
        build = RecordFixes._find_record_builds(module)
        record_dicts, elements = build.names, build.elements
        if elements is None or len(elements) < 2:
            return None
        field_names = []
        seen: set[str] = set()
        for i, el in enumerate(elements):
            base = RecordFixes._tuple_element_name(el)
            if RecordFixes._field_name_taken(base, seen, elements, el):
                base = f"field_{i}"
            seen.add(base)
            field_names.append(base)
        transformed = wrapper.visit(RecordFixes._RecordToClass(self.request.rel, record_dicts, class_name, field_names))
        # the class def plus an EmptyLine — byte-identical to the old
        # Expr(SimpleStatementLine) wrap, which codegen'd a trailing newline
        new_body: list = [Rewrite.record_class_def(class_name, field_names), cst.EmptyLine()]
        new_body.extend(transformed.body)
        return cst.Module(body=new_body).code

    def extract_record_class(self) -> str | None:
        """The constant-key dict literal becomes a class instance: the keys
        are the fields, string-subscript reads of the bound names become
        attribute reads, and the class definition is prepended to the module.
        The anchor (line, col) selects WHICH literal when several share a
        line; without col, the line must carry exactly one candidate."""
        source = self.request.loaded_source()
        raw_name = self.request.opts.name
        # a placeholder or non-identifier would generate `class _<Record>:`
        # — invalid Python; refuse before generating (review bot)
        if not raw_name or not raw_name.isidentifier():
            return None
        class_name = raw_name if raw_name.startswith("_") else "_" + raw_name
        wrapper = cst.MetadataWrapper(cst.parse_module(source))
        candidates = RecordFixes._find_record_dicts(wrapper, self.request.line)
        if self.request.col > 0:
            hits = [c for c in candidates if c[0] == self.request.line and c[1] == self.request.col]
        else:
            hits = [c for c in candidates if c[0] == self.request.line]
        if len(hits) != 1:
            # ambiguous without an anchor column — refusing beats guessing
            # which record becomes the class
            return None
        hit = hits[0]
        target_dict = hit.node
        raw_keys = RecordFixes._dict_constant_keys(target_dict)
        key_map: dict[str, str] = {}
        for i, k in enumerate(raw_keys):
            if not k.isidentifier():
                # a non-identifier key would change the wire format on
                # conversion — out of scope for a mechanical fix
                return None
            field = k
            while field in key_map.values():
                field = f"{field}_{i}"
            key_map[k] = field
        bind_names = RecordFixes._record_bind_names(wrapper, target_dict)
        rewritten = wrapper.visit(
            RecordFixes._DictToClass(self.request.line, self.request.col, class_name, key_map, frozenset(bind_names))
        )
        new_body: list = [Rewrite.record_class_def(class_name, list(key_map.values())), cst.EmptyLine()]
        new_body.extend(rewritten.body)
        return cst.Module(body=new_body).code

    class _DictToClass(cst.CSTTransformer):
        """Replace the flagged constant-key dict with the record constructor and
        rewrite string-subscript reads of its bound names to attribute reads."""

        METADATA_DEPENDENCIES = (PositionProvider,)

        def __init__(
            self,
            line: int,
            col: int,
            class_name: str,
            key_map: dict[str, str],
            bind_names: frozenset[str],
        ) -> None:
            self.line: int = line
            self.col: int = col
            self.class_name: str = class_name
            self.key_map: dict[str, str] = key_map
            self.bind_names: frozenset[str] = bind_names

        def _at_finding_position(self, pos) -> bool:
            """The node is the literal the finding cites. col==0 keeps the legacy
            line-only contract; a transported col pins the exact literal among
            same-line twins."""
            if pos.start.line != self.line:
                return False
            return self.col == 0 or pos.start.column + 1 == self.col

        def _reads_one_bound_name(self, value: cst.BaseExpression, slices) -> bool:
            """The subscript indexes one name bound by the loop target."""
            return isinstance(value, cst.Name) and value.value in self.bind_names and len(slices) == 1

        @override
        def leave_Dict(self, original_node, updated_node):
            pos = ModuleOutline.span(self.get_metadata(PositionProvider, original_node))
            if not self._at_finding_position(pos):
                return updated_node
            keywords = []
            for el in updated_node.elements:
                if not isinstance(el, cst.DictElement):
                    continue
                k = el.key
                if not isinstance(k, cst.SimpleString):
                    continue
                raw = k.evaluated_value
                field = self.key_map.get(raw if isinstance(raw, str) else str(raw))
                if field is None:
                    continue
                keywords.append(
                    cst.Arg(
                        keyword=cst.Name(field),
                        value=el.value,
                        equal=cst.AssignEqual(
                            whitespace_before=cst.SimpleWhitespace(""),
                            whitespace_after=cst.SimpleWhitespace(""),
                        ),
                    )
                )
            return cst.Call(func=cst.Name(self.class_name), args=keywords)

        @override
        def leave_Subscript(self, original_node, updated_node):
            slices = updated_node.slice
            if self._reads_one_bound_name(updated_node.value, slices):
                idx = slices[0].slice
                if not isinstance(idx, cst.Index) or not isinstance(idx.value, cst.SimpleString):
                    return updated_node
                raw = idx.value.evaluated_value
                field = self.key_map.get(raw if isinstance(raw, str) else str(raw))
                if field:
                    return cst.Attribute(value=updated_node.value, attr=cst.Name(field))
            return updated_node

    class _FindRecordDicts(cst.CSTVisitor):
        """Constant-key dict literals anchored on one line — cols disambiguate
        same-line twins."""

        METADATA_DEPENDENCIES = (PositionProvider,)

        def __init__(self, line: int) -> None:
            self.line: int = line
            self.hits: list[RecordFixes._RecordDictHit] = []

        @override
        def visit_Dict(self, node) -> None:
            pos = ModuleOutline.span(self.get_metadata(PositionProvider, node))
            if pos.start.line != self.line:
                return
            keys: list[str] = []
            for el in node.elements:
                if not isinstance(el, cst.DictElement):
                    continue
                k = el.key
                if isinstance(k, cst.SimpleString):
                    val = k.evaluated_value
                    keys.append(val if isinstance(val, str) else str(val))
            if len(node.elements) >= 2 and len(keys) == len(node.elements):
                self.hits.append(RecordFixes._RecordDictHit(pos.start.line, pos.start.column + 1, node, keys))

    class _RecordToClass(cst.CSTTransformer):
        """Turn an anonymous tuple record into a class: the build sites construct
        the class, the positional reads become attribute reads, the destructures
        read the fields explicitly. The class definition is prepended."""

        METADATA_DEPENDENCIES = (PositionProvider,)

        def __init__(self, record_name: str, record_dicts: set[str], class_name: str, field_names: list[str]):
            self.record_name: str = record_name
            self.record_dicts: set[str] = record_dicts
            self.class_name: str = class_name
            self.field_names: list[str] = field_names

        def _is_record_base(self, e: cst.BaseExpression) -> bool:
            """The base name of an attribute/subscript chain — one of the record
            dicts?"""
            v = e
            while isinstance(v, (cst.Attribute, cst.Subscript)):
                v = v.value
            return isinstance(v, cst.Name) and v.value in self.record_dicts

        @override
        def leave_Subscript(self, original_node, updated_node):
            # X[k][N] -> X[k].field_N when the outer index is a constant position
            if not isinstance(updated_node.slice, tuple) or len(updated_node.slice) != 1:
                return updated_node
            el = updated_node.slice[0]
            if not isinstance(el, cst.SubscriptElement) or not isinstance(el.slice, cst.Index):
                return updated_node
            idx_node = el.slice.value
            if not isinstance(idx_node, cst.Integer):
                return updated_node
            idx = int(idx_node.value)
            if idx >= len(self.field_names) or not isinstance(updated_node.value, cst.Subscript):
                return updated_node
            if not self._is_record_base(updated_node.value.value):
                return updated_node
            return cst.Attribute(value=updated_node.value, attr=cst.Name(self.field_names[idx]))

        @override
        def leave_Dict(self, original_node, updated_node):
            return updated_node

        @override
        def leave_DictComp(self, original_node, updated_node):
            if isinstance(updated_node.value, cst.Tuple) and len(updated_node.value.elements) == len(self.field_names):
                return updated_node.with_changes(
                    value=cst.Call(
                        func=cst.Name(self.class_name),
                        args=[cst.Arg(e.value) for e in updated_node.value.elements],
                    )
                )
            return updated_node

        @override
        def leave_Assign(self, original_node, updated_node):
            # a, b = X[k] -> a, b = X[k].f0, X[k].f1
            if not isinstance(updated_node.value, cst.Subscript):
                return updated_node
            if not self._is_record_base(updated_node.value.value):
                return updated_node
            reads = [cst.Attribute(value=updated_node.value, attr=cst.Name(n)) for n in self.field_names]
            return updated_node.with_changes(value=cst.Tuple(elements=[cst.Element(r) for r in reads]))

        @override
        def leave_For(self, original_node, updated_node):
            # for k, (a, b) in X.items() -> for k, v in X.items(): a, b = v.f0, v.f1
            if not isinstance(updated_node.iter, cst.Call) or not isinstance(updated_node.iter.func, cst.Attribute):
                return updated_node
            if updated_node.iter.func.attr.value != "items":
                return updated_node
            if not self._is_record_base(updated_node.iter.func.value):
                return updated_node
            t = updated_node.target
            if not RecordFixes._is_field_pair_target(t):
                return updated_node
            assert isinstance(t, cst.Tuple) and isinstance(t.elements[1].value, cst.Tuple)
            outer = t.elements[0].value
            inner = t.elements[1].value
            if len(inner.elements) != len(self.field_names):
                return updated_node
            binds = [e.value for e in inner.elements]
            reads = [cst.Attribute(value=cst.Name("v"), attr=cst.Name(n)) for n in self.field_names]
            unpack = cst.SimpleStatementLine(
                body=[
                    cst.Assign(
                        targets=[cst.AssignTarget(cst.Tuple(elements=[cst.Element(b) for b in binds]))],
                        value=cst.Tuple(elements=[cst.Element(r) for r in reads]),
                    )
                ],
                leading_lines=[cst.EmptyLine()],
            )
            new_body = list(updated_node.body.body)
            new_body.insert(0, unpack)
            return updated_node.with_changes(
                target=cst.Tuple(elements=[cst.Element(outer), cst.Element(cst.Name("v"))]),
                body=updated_node.body.with_changes(body=new_body),
            )

    @staticmethod
    def _dict_constant_keys(d: cst.Dict) -> list[str]:
        keys: list[str] = []
        for el in d.elements:
            if not isinstance(el, cst.DictElement):
                continue
            k = el.key
            if isinstance(k, cst.SimpleString):
                val = k.evaluated_value
                keys.append(val if isinstance(val, str) else str(val))
        return keys

    @staticmethod
    def _field_name_taken(
        base: str,
        seen: set[str],
        elements: list[cst.BaseExpression],
        element: cst.BaseExpression,
    ) -> bool:
        """The field name is already used, or it is the wrapper's own `value` while
        the tuple elements differ from each other (one field cannot hold them)."""
        if base in seen:
            return True
        return base == "value" and any(other != element for other in elements)

    @staticmethod
    def _find_record_builds(module: cst.Module) -> RecordFixes._RecordBuild:
        """The dict names built with tuple values + the first build's elements
        (for the field-name inference)."""
        names: set[str] = set()
        elements: list[cst.BaseExpression] | None = None
        for stmt in module.body:
            if not isinstance(stmt, cst.SimpleStatementLine) or len(stmt.body) != 1:
                continue
            assign = stmt.body[0]
            if not isinstance(assign, cst.Assign) or len(assign.targets) != 1:
                continue
            target = assign.targets[0].target
            if not isinstance(target, cst.Name):
                continue
            value = assign.value
            if isinstance(value, cst.DictComp):
                value = value.value
            if isinstance(value, cst.Tuple) and len(value.elements) >= 2:
                names.add(target.value)
                if elements is None:
                    elements = [e.value for e in value.elements]
                elif len(value.elements) != len(elements):
                    return RecordFixes._RecordBuild(set(), None)
        return RecordFixes._RecordBuild(names, elements)

    @staticmethod
    def _find_record_dicts(wrapper: cst.MetadataWrapper, line: int) -> list[RecordFixes._RecordDictHit]:
        finder = RecordFixes._FindRecordDicts(line)
        wrapper.visit(finder)
        return finder.hits

    @staticmethod
    def _is_field_pair_target(target: cst.BaseExpression) -> bool:
        """The loop target destructures a key and a tuple of fields:
        `for k, (a, b) in ...`."""
        return (
            isinstance(target, cst.Tuple)
            and len(target.elements) == 2
            and isinstance(target.elements[1].value, cst.Tuple)
        )

    @staticmethod
    def _record_bind_names(wrapper: cst.MetadataWrapper, target_dict: cst.Dict) -> set[str]:
        """Names bound to THIS dict literal by assignment (`payload = {...}`)."""
        names: set[str] = set()

        class _Binds(cst.CSTVisitor):
            @override
            def visit_Assign(self, node) -> None:
                if node.value is not target_dict:
                    return
                for tgt in node.targets:
                    inner = getattr(tgt, "target", None)
                    if isinstance(inner, cst.Name):
                        names.add(inner.value)

        wrapper.visit(_Binds())
        return names

    @staticmethod
    def _tuple_element_name(elem: cst.BaseExpression) -> str:
        """A field name for one tuple position: a plain Name element keeps its
        name; a title(...)-style extraction is the record's name; anything else
        falls back to the positional name (the agent renames the class it
        asked for)."""
        if isinstance(elem, cst.Name):
            return elem.value
        if isinstance(elem, cst.Call) and isinstance(elem.func, cst.Name):
            return {"title": "name"}.get(elem.func.value, elem.func.value)
        return "value"

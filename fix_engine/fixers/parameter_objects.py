"""The The parameter-object fix: a long signature becomes one object.

One fix family, one class; the class holds the request, its helpers are
members, and its libcst visitors are nested classes.
"""

from __future__ import annotations

from typing import Any, override

import libcst as cst
import libcst.matchers as m
from libcst.metadata import PositionProvider

from fix_engine.module_outline import ModuleOutline


class ParameterObjects:
    """The parameter object fix for one request."""

    def __init__(self, request: Any) -> None:
        self.request = request

    def run(self) -> str | None:
        source, line = self.request.loaded_source(), self.request.line
        name = self.request.opts.name
        # the dispatch gate (_fix_structural) refuses name-required kinds without one
        assert isinstance(name, str)
        """Introduce Parameter Object: bundle the function's params into a
        dataclass named `name`, change the signature to `options: name`, rewrite
        the body references and same-file call sites."""
        module = cst.parse_module(source)
        wrapper = cst.MetadataWrapper(module)
        target = ParameterObjects._find_fn_at(wrapper, line)
        if target is None:
            return None
        # the scanner's threshold counts posonly + args + kwonly (minus a
        # leading self/cls) — the fixer must count the SAME set, or flagged
        # /-separated parameter lists are silently refused (review bot)
        all_params = [
            *target.params.posonly_params,
            *target.params.params,
            *target.params.kwonly_params,
        ]
        total = len(all_params)
        if ParameterObjects._is_receiver_param(all_params):
            total -= 1
        if total <= 5:
            return None  # not the long-param-list shape (threshold is > 5)
        params = [p for p in all_params if p.name is not None and p.name.value not in ("self", "cls")]
        param_names = [p.name.value for p in params]
        renamed = wrapper.visit(ParameterObjects._FnBodyRewrite(target.name.value, line, param_names, name)).code
        call_fixed = (
            cst.parse_module(renamed)
            .visit(ParameterObjects._CallSiteRewrite(target.name.value, param_names, name))
            .code
        )
        module3 = cst.parse_module(call_fixed)
        body = list(module3.body)
        first = next(
            (i for i, s in enumerate(body) if isinstance(s, (cst.FunctionDef, cst.ClassDef))),
            len(body),
        )
        body.insert(first, ParameterObjects._dataclass_def(name, params))
        return module3.with_changes(body=ParameterObjects._ensure_dataclasses_import(body)).code

    class _BodyParamRewrite(cst.CSTTransformer):
        """Rewrite body references of the bundled params to options.<param>."""

        def __init__(self, params: list[str]) -> None:
            self.params: set[str] = set(params)

        @override
        def on_visit(self, node) -> bool:
            # nested functions and lambdas are their own scopes: a bundled name
            # there is a local/parameter of THAT scope, not the outer param
            # (def inner(a) must not become def inner(options.a))
            return not isinstance(node, (cst.FunctionDef, cst.Lambda))

        @override
        def leave_Name(self, original_node, updated_node):
            if updated_node.value in self.params:
                return cst.Attribute(
                    value=cst.Name("options"),
                    attr=cst.Name(updated_node.value),
                )
            return updated_node

    class _CallSiteRewrite(cst.CSTTransformer):
        """f(a, b, c) -> Name.f(a=a, b=b, c=c) for the renamed function."""

        def __init__(self, fn: str, params: list[str], class_name: str) -> None:
            self.fn: str = fn
            self.params: list[str] = params
            self.class_name: str = class_name

        @override
        def leave_Call(self, original_node, updated_node):
            if not m.matches(updated_node.func, m.Name(value=self.fn)):
                return updated_node
            args = list(updated_node.args)
            if len(args) != len(self.params):
                return updated_node  # positional mismatch — leave untouched
            new_args = [
                cst.Arg(
                    keyword=cst.Name(p),
                    value=a.value,
                    equal=cst.AssignEqual(
                        whitespace_before=cst.SimpleWhitespace(""),
                        whitespace_after=cst.SimpleWhitespace(""),
                    ),
                )
                for p, a in zip(self.params, args, strict=True)
            ]
            return updated_node.with_changes(
                func=cst.Name(self.fn),
                args=[
                    cst.Arg(
                        cst.Call(
                            func=cst.Name(self.class_name),
                            args=new_args,
                        )
                    )
                ],
            )

    class _FnBodyRewrite(cst.CSTTransformer):
        """The parameter-object fn: params -> options.<param> in the body, and the
        signature collapses to (receiver, options: Name)."""

        METADATA_DEPENDENCIES = (PositionProvider,)

        def __init__(self, fn_name: str, line: int, params: list[str], class_name: str) -> None:
            self.fn_name: str = fn_name
            self.line: int = line
            self.params: set[str] = set(params)
            self.class_name: str = class_name

        @override
        def leave_FunctionDef(self, original_node, updated_node):
            if updated_node.name.value != self.fn_name:
                return updated_node
            if ModuleOutline.span(self.get_metadata(PositionProvider, original_node)).start.line != self.line:
                return updated_node
            new_body = updated_node.body.visit(ParameterObjects._BodyParamRewrite(list(self.params)))
            receiver = [p for p in updated_node.params.params if p.name is not None and p.name.value in ("self", "cls")]
            options_param = cst.Param(
                name=cst.Name("options"),
                annotation=cst.Annotation(cst.Name(self.class_name)),
            )
            return updated_node.with_changes(
                params=updated_node.params.with_changes(params=[*receiver, options_param]),
                body=new_body,
            )

    @staticmethod
    def _dataclass_def(name: str, params: list[cst.Param]) -> cst.ClassDef:
        """The bundled-params dataclass — one AnnAssign field per param."""
        field_lines = []
        for p in params:
            ann = p.annotation.annotation if p.annotation is not None else cst.Name("object")
            field_lines.append(
                cst.SimpleStatementLine(
                    body=[cst.AnnAssign(target=cst.Name(p.name.value), annotation=cst.Annotation(ann))]
                )
            )
        return cst.ClassDef(
            name=cst.Name(name),
            bases=[],
            decorators=[cst.Decorator(cst.Name("dataclass"))],
            body=cst.IndentedBlock(body=field_lines),
        )

    @staticmethod
    def _ensure_dataclasses_import(body: list) -> list:
        """Prepend `from dataclasses import dataclass` when missing."""
        if any(ParameterObjects._is_dataclass_import(s) for s in body):
            return body
        imp = cst.SimpleStatementLine(
            body=[
                cst.ImportFrom(
                    module=cst.Name("dataclasses"),
                    names=[cst.ImportAlias(cst.Name("dataclass"))],
                )
            ]
        )
        return [imp, *body]

    @staticmethod
    def _find_fn_at(wrapper, line: int, name: str | None = None) -> cst.FunctionDef | None:
        """The module-level def at `line` (optionally by name)."""
        found: cst.FunctionDef | None = None

        class _Find(cst.CSTVisitor):
            METADATA_DEPENDENCIES = (PositionProvider,)

            def _is_target(self, node, found, line: int, name: str | None) -> bool:
                """The first def at the finding's line (optionally by name)."""
                if found is not None:
                    return False
                if ModuleOutline.span(self.get_metadata(PositionProvider, node)).start.line != line:
                    return False
                return name is None or node.name.value == name

            @override
            def visit_FunctionDef(self, node) -> None:
                nonlocal found
                if self._is_target(node, found, line, name):
                    found = node

        wrapper.visit(_Find())
        return found

    @staticmethod
    def _is_dataclass_import(stmt) -> bool:
        """`from dataclasses import dataclass` — the import the bundled-params
        fix prepends; True when the body already carries it."""
        if not isinstance(stmt, cst.SimpleStatementLine) or len(stmt.body) != 1:
            return False
        imp = stmt.body[0]
        if not isinstance(imp, cst.ImportFrom) or not isinstance(imp.module, cst.Name):
            return False
        if "dataclasses" not in imp.module.value:
            return False
        if isinstance(imp.names, cst.ImportStar):
            return False  # a star import binds no explicit `dataclass`
        return any(isinstance(a, cst.ImportAlias) and a.name.value == "dataclass" for a in imp.names)

    @staticmethod
    def _is_receiver_param(params: list[cst.Param]) -> bool:
        """The first parameter is the receiver (`self`/`cls`) — it is not part of
        the list a parameter record bundles."""
        first = params[0].name if params else None
        return first is not None and first.value in ("self", "cls")

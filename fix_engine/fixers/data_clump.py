"""The data-clump shape arm: a shared parameter pair becomes one object.

One fix family, one class; the class holds the request, its helpers are
members, and its libcst visitors are nested classes.
"""

from __future__ import annotations

import keyword
import re
from pathlib import Path
from typing import Any, NamedTuple, override

import libcst as cst
from libcst.metadata import ExpressionContextProvider, ParentNodeProvider, PositionProvider

from fix_engine.bindings import Bindings, StoredNames
from fix_engine.module_outline import ModuleOutline
from fix_engine.rewrite import Rewrite

# lucidlint: ignore-file record-shape the libcst layer's tuple/dict shorthands ARE its wire records —
# lucidlint: ignore-file record-shape a class per helper hop is ceremony


class DataClump:
    """The parameter-object fix for one request: the clump's pair is threaded
    through a record class."""

    class _Clump(NamedTuple):
        """The recovered clump: the shared pair (in the anchor's signature order)
        and the module-level defs carrying it, in source order."""

        pair: tuple[str, str]
        fns: tuple[cst.FunctionDef, ...]

    def __init__(self, request) -> None:
        self.request: Any = request

    def run(self) -> str | None:
        """Thread the clump's shared pair through one parameter object: the class
        is prepended, the clump's signatures and bodies change, and module-scope
        call sites construct the instance. Declines when the pair cannot be
        recovered, a caller lives in another module, or a call cannot be
        resolved."""
        source, line = self.request.loaded_source(), self.request.line
        # the wrapper's own tree: metadata resolves by NODE IDENTITY, so the
        # module inspected here must be the wrapper's copy, not a parallel parse
        wrapper = cst.MetadataWrapper(cst.parse_module(source))
        module = wrapper.module
        clump = DataClump._clump_at(module, line, wrapper.resolve(PositionProvider))
        if clump is None:
            self.request.decline = (
                f"no parameter pair shared by 3+ module functions is anchored at {self.request.rel}:{line} "
                "— the anchor is stale, or the clump is gone"
            )
            return None
        pair_a, pair_b = clump.pair
        index: dict[str, tuple[int, int]] = {}
        for fn in clump.fns:
            if DataClump._has_positional_only_or_variadic(fn.params):
                self.request.decline = (
                    f"'{fn.name.value}' has positional-only or variadic parameters — the pair "
                    "cannot be threaded mechanically; do it by hand"
                )
                return None
            if any(p.default is not None for p in fn.params.params if p.name.value in clump.pair):
                self.request.decline = (
                    f"'{fn.name.value}' defaults '{pair_a}'/'{pair_b}' — threading would change the "
                    "call contract; do it by hand"
                )
                return None
            args = DataClump._plain_args(fn)
            if pair_a not in args or pair_b not in args:
                self.request.decline = (
                    f"'{fn.name.value}' does not carry the pair in its plain parameters — thread it by hand"
                )
                return None
            index[fn.name.value] = (args.index(pair_a), args.index(pair_b))
        class_name = self.request.opts.name or Rewrite.pascal_case(f"{pair_a}_{pair_b}")
        if not class_name.isidentifier() or keyword.iskeyword(class_name):
            self.request.decline = f"'{class_name}' is not a usable class name — pass --name <Name>"
            return None
        parameter = DataClump._snake_case(class_name)
        if parameter in clump.pair:
            self.request.decline = f"the threaded parameter '{parameter}' collides with the pair — pass another --name"
            return None
        if Bindings.binds_name(module, class_name):
            self.request.decline = f"'{class_name}' is already bound in {self.request.rel} — pass another --name"
            return None
        culprit = DataClump._imported_elsewhere(
            self.request.repo, self.request.rel, {fn.name.value for fn in clump.fns}
        )
        if culprit:
            self.request.decline = (
                f"'{culprit}' imports a clump function — its call site is outside this module; thread the pair by hand"
            )
            return None
        transformer = DataClump._DataClumpToRecord(
            clump.pair, class_name, parameter, Bindings.stores_by_function(wrapper), index
        )
        transformed = wrapper.visit(transformer)
        if transformer.decline:
            self.request.decline = transformer.decline
            return None
        body: list = [Rewrite.record_class_def(class_name, [pair_a, pair_b]), cst.EmptyLine(), *transformed.body]
        return cst.Module(body=body).code

    class _ImportedNames(cst.CSTVisitor):
        """The names a module binds through `from m import a, b`."""

        def __init__(self) -> None:
            self.names: set[str] = set()

        @override
        def visit_ImportFrom(self, node) -> None:
            if isinstance(node.names, cst.ImportStar):
                return
            for alias in node.names:
                if not isinstance(alias.name, cst.Name):
                    continue
                asname = alias.asname
                if asname is None:
                    self.names.add(alias.name.value)
                    continue
                if isinstance(asname.name, cst.Name):
                    self.names.add(asname.name.value)

    @staticmethod
    def _imported_elsewhere(repo: Path, rel: str, names: set[str]) -> str | None:
        """The repo-relative path of another module importing any of `names` —
        that caller's call site is outside the fixer's reach. None when the names
        are module-local."""
        for path in sorted(p for p in repo.rglob("*.py") if p.is_file()):
            if path == repo / rel:
                continue
            try:
                tree = cst.parse_module(path.read_text(encoding="utf-8"))
            except (OSError, cst.ParserSyntaxError):
                continue
            finder = DataClump._ImportedNames()
            tree.visit(finder)
            if finder.names & names:
                return str(path.relative_to(repo))
        return None

    @staticmethod
    def _plain_args(fn: cst.FunctionDef) -> list[str]:
        """The regular positional parameter names — the only set the scanner's
        data-clump rule pairs over."""
        return [param.name.value for param in fn.params.params]

    @staticmethod
    def _type_alias_value(node: cst.BaseExpression) -> bool:
        """A type expression, not a runtime value: the scanner's alias table
        accepts names, attributes, subscripts, and `|` unions."""
        if isinstance(node, (cst.Name, cst.Attribute, cst.Subscript)):
            return True
        return isinstance(node, cst.BinaryOperation) and isinstance(node.operator, cst.BitOr)

    @staticmethod
    def _module_aliases(module: cst.Module) -> dict[str, cst.BaseExpression]:
        """Module-level type aliases (`WallAlias = Wall`), the fixer's mirror of
        the scanner's `module_type_bindings` resolution."""
        aliases: dict[str, cst.BaseExpression] = {}
        for stmt in module.body:
            if not isinstance(stmt, cst.SimpleStatementLine):
                continue
            for small in stmt.body:
                if isinstance(small, cst.Assign) and len(small.targets) == 1:
                    target = small.targets[0].target
                    if isinstance(target, cst.Name) and DataClump._type_alias_value(small.value):
                        aliases[target.value] = small.value
                elif isinstance(small, cst.AnnAssign) and isinstance(small.target, cst.Name):
                    if small.value is not None and DataClump._type_alias_value(small.value):
                        aliases[small.target.value] = small.value
        return aliases

    @staticmethod
    def _annotation_key(
        annotation: cst.BaseExpression | None,
        module: cst.Module,
        aliases: dict[str, cst.BaseExpression],
    ) -> str:
        """The parameter's grouping key: annotation source text without
        whitespace, resolved through module aliases. `""` is unannotated."""
        node = annotation
        seen: set[str] = set()
        while DataClump._is_unresolved_alias(node, aliases, seen):
            assert isinstance(node, cst.Name)
            seen.add(node.value)
            node = aliases[node.value]
        if node is None:
            return ""
        return "".join(module.code_for_node(node).split())

    @staticmethod
    def _typed_args(
        fn: cst.FunctionDef,
        module: cst.Module,
        aliases: dict[str, cst.BaseExpression],
    ) -> list[tuple[str, str]]:
        """The regular positional parameters with their grouping keys."""
        return [
            (
                param.name.value,
                DataClump._annotation_key(
                    param.annotation.annotation if param.annotation else None,
                    module,
                    aliases,
                ),
            )
            for param in fn.params.params
        ]

    @staticmethod
    def _clump_at(module: cst.Module, line: int, positions) -> DataClump._Clump | None:
        """The clump anchored at `line`: the module-level defs sharing a regular-
        parameter pair (>= 3 of them, the scanner's rule), the pair in the
        anchor's signature order. None when no pair qualifies there (a stale
        anchor). Pairs that share names but not annotations stay separate."""
        fns = [stmt for stmt in module.body if isinstance(stmt, cst.FunctionDef)]
        aliases = DataClump._module_aliases(module)
        pairs: dict[tuple[tuple[str, str], tuple[str, str]], list[cst.FunctionDef]] = {}
        for fn in fns:
            params = DataClump._typed_args(fn, module, aliases)
            for i in range(len(params)):
                for j in range(i + 1, len(params)):
                    left, right = params[i], params[j]
                    key = (left, right) if left[0] <= right[0] else (right, left)
                    pairs.setdefault(key, []).append(fn)
        anchor = next((f for f in fns if ModuleOutline.span(positions[f]).start.line == line), None)
        if anchor is None:
            return None
        qualifying = sorted(pair for pair, group in pairs.items() if len(group) >= 3 and anchor in group)
        if not qualifying:
            return None
        pair = qualifying[0]
        order = DataClump._plain_args(anchor)
        kept = {id(f) for f in pairs[pair]}
        ordered = tuple(sorted((pair[0][0], pair[1][0]), key=order.index))
        return DataClump._Clump((ordered[0], ordered[1]), tuple(f for f in fns if id(f) in kept))

    class _DataClumpToRecord(cst.CSTTransformer):
        """Thread the clump's pair as one parameter object: a body read of either
        name becomes `<instance>.<name>`, each clump signature drops the two
        params for the single instance, and the module's calls to the clump build
        the instance where they passed the two values."""

        METADATA_DEPENDENCIES = (ExpressionContextProvider, ParentNodeProvider)

        def __init__(
            self,
            pair: tuple[str, str],
            class_name: str,
            param_name: str,
            stores: StoredNames,
            index: dict[str, tuple[int, int]],
        ) -> None:
            self.pair: tuple[str, str] = pair
            self.class_name: str = class_name
            self.param_name: str = param_name
            self.stores: StoredNames = stores
            self.index: dict[str, tuple[int, int]] = index
            self.decline: str = ""
            self._current: str | None = None
            self._fn_stack: list[str | None] = []
            self._shadow_stack: list[set[str]] = []

        def _is_forwarded_pair(self, a_val, b_val) -> bool:
            """Both arguments are the clump's two fields, read from the parameter
            the caller already holds — the pair is forwarded, not rebuilt."""
            return (
                isinstance(a_val, cst.Attribute)
                and isinstance(b_val, cst.Attribute)
                and isinstance(a_val.value, cst.Name)
                and isinstance(b_val.value, cst.Name)
                and a_val.value.value == self.param_name
                and b_val.value.value == self.param_name
                and (a_val.attr.value, b_val.attr.value) == self.pair
            )

        @staticmethod
        def _own_params(node: cst.FunctionDef) -> set[str]:
            return {
                p.name.value for p in [*node.params.posonly_params, *node.params.params, *node.params.kwonly_params]
            }

        @override
        def visit_FunctionDef(self, node) -> None:
            self._fn_stack.append(self._current)
            self._current = node.name.value
            shadow = set(self.stores.get(node.name.value, set()))
            if node.name.value not in self.index:
                # a non-clump scope's own params (a nested def's `folder`) are its
                # locals — its reads must not be retargeted at the instance
                shadow |= self._own_params(node)
            self._shadow_stack.append(shadow)

        @override
        def leave_FunctionDef(self, original_node, updated_node):
            self._current = self._fn_stack.pop()
            self._shadow_stack.pop()
            fn = updated_node.name.value
            if fn not in self.index:
                return updated_node
            params = [p for p in updated_node.params.params if p.name.value not in self.pair]
            if len(params) != len(updated_node.params.params) - 2:
                self.decline = f"'{fn}' does not carry the pair in its plain parameters — thread it by hand"
                return updated_node
            if params and isinstance(params[-1].comma, cst.Comma):
                # the removed pair carried the list's tail; a stray comma would
                # render as `def f(x, )`
                params[-1] = params[-1].with_changes(comma=cst.MaybeSentinel.DEFAULT)
            return updated_node.with_changes(
                params=updated_node.params.with_changes(params=[cst.Param(cst.Name(self.param_name)), *params])
            )

        @override
        def leave_Name(self, original_node, updated_node):
            fn = self._current or ""
            if fn not in self.index or updated_node.value not in self.pair:
                return updated_node
            if updated_node.value in self._shadow_stack[-1]:
                return updated_node
            parent = self.get_metadata(ParentNodeProvider, original_node)
            if ModuleOutline.is_keyword_name(original_node, parent):
                return updated_node
            try:
                ctx = self.get_metadata(ExpressionContextProvider, original_node)
            except KeyError:
                return updated_node
            if ctx is not cst.metadata.ExpressionContext.LOAD:
                return updated_node
            return cst.Attribute(value=cst.Name(self.param_name), attr=cst.Name(updated_node.value))

        @override
        def leave_Call(self, original_node, updated_node):
            if not isinstance(updated_node.func, cst.Name) or updated_node.func.value not in self.index:
                return updated_node
            fn = updated_node.func.value
            idx_a, idx_b = self.index[fn]
            if any(arg.star for arg in updated_node.args):
                self.decline = f"the call to '{fn}' unpacks its arguments — thread the pair by hand"
                return updated_node
            pos_args = [arg for arg in updated_node.args if arg.keyword is None]
            keywords = {arg.keyword.value: arg for arg in updated_node.args if arg.keyword is not None}
            a_arg = pos_args[idx_a] if idx_a < len(pos_args) else keywords.get(self.pair[0])
            b_arg = pos_args[idx_b] if idx_b < len(pos_args) else keywords.get(self.pair[1])
            if a_arg is None or b_arg is None:
                self.decline = (
                    f"the call to '{fn}' does not pass both '{self.pair[0]}' and '{self.pair[1]}' — "
                    "thread the pair by hand"
                )
                return updated_node
            a_val, b_val = a_arg.value, b_arg.value
            if self._is_forwarded_pair(a_val, b_val):
                # the caller already holds the instance (a forwarded pair) — pass
                # it on instead of rebuilding it from its own fields
                replacement: cst.BaseExpression = cst.Name(self.param_name)
            else:
                replacement = cst.Call(func=cst.Name(self.class_name), args=[cst.Arg(a_val), cst.Arg(b_val)])
            # the untouched args keep their own nodes: their comments and
            # formatting survive the rewrite
            kept = [arg for i, arg in enumerate(pos_args) if i not in (idx_a, idx_b)]
            kept.insert(min(idx_a, idx_b), cst.Arg(replacement))
            new_args = list(kept)
            new_args.extend(
                arg for arg in updated_node.args if arg.keyword is not None and arg.keyword.value not in self.pair
            )
            return updated_node.with_changes(args=new_args)

    @staticmethod
    def _snake_case(text: str) -> str:
        """`FolderFingerprint` -> `folder_fingerprint` (the threaded parameter)."""
        return re.sub(r"(?<=[a-z0-9])(?=[A-Z])", "_", text).lower()

    @staticmethod
    def _is_unresolved_alias(node: cst.BaseExpression | None, aliases: dict, seen: set[str]) -> bool:
        """The name is an import alias that has not been walked yet."""
        return isinstance(node, cst.Name) and node.value in aliases and node.value not in seen

    @staticmethod
    def _has_positional_only_or_variadic(params: cst.Parameters) -> bool:
        """The signature has positional-only or variadic parameters — the fix
        cannot rewrite it faithfully."""
        return bool(params.posonly_params) or isinstance(params.star_arg, cst.Param) or params.star_kwarg is not None

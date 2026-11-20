"""The dispatch chain becomes a handler registry.

Each arm's literal selects a handler, and one call dispatches.
"""

from __future__ import annotations

from typing import override

import libcst as cst

from fix_engine.cst_mechanics import CstMechanics

# the dispatch-chain collections — named so the signatures are not bare
# record collections (the record-shape rule's escape hatch)
_AMBIENT = frozenset(
    {
        "str",
        "int",
        "float",
        "bool",
        "list",
        "dict",
        "set",
        "tuple",
        "bytes",
        "bytearray",
        "len",
        "sorted",
        "min",
        "max",
        "sum",
        "any",
        "all",
        "range",
        "enumerate",
        "zip",
        "map",
        "filter",
        "next",
        "iter",
        "print",
        "isinstance",
        "issubclass",
        "repr",
        "abs",
        "round",
        "format",
        "hash",
        "id",
        "type",
        "object",
        "getattr",
        "setattr",
        "hasattr",
        "callable",
        "chr",
        "ord",
        "bin",
        "hex",
        "oct",
        "reversed",
        "slice",
        "Exception",
        "ValueError",
        "KeyError",
        "TypeError",
        "NotImplementedError",
        "RuntimeError",
        "None",
        "True",
        "False",
        "open",
        "staticmethod",
        "classmethod",
        "property",
        "super",
        "self",
    }
)

_DispatchArm = tuple[str, str | bytes, list[cst.BaseStatement]]
_DispatchChain = list[tuple[str, list[cst.BaseStatement]]]
_DispatchShape = tuple[list[cst.BaseStatement], _DispatchChain, str, cst.Return]
_DispatchArms = tuple[list | None, str | None]
_RuleBuild = tuple[list[cst.FunctionDef], list]
_DispatchBuild = tuple[list[cst.FunctionDef], cst.SimpleStatementLine]


# lucidlint: ignore-file record-shape the dispatch/rule analyses are cst construction
# plans — their tuples are local multi-value returns between private helpers, and a
# class per hop is ceremony (the same reviewed why the pre-split module carried)


class DispatchRegistry:
    """One request's dispatch registry fix. The request is the fixer's state."""

    def __init__(self, request) -> None:
        self.request = request

    def fix_dispatch_registry(self) -> str | None:
        source, line = self.request._loaded_source(), self.request.line
        """A dispatch chain (`if sel == "a": return f()  if sel == "b": ...`)
        is a handler REGISTRY in disguise — each arm is already a named handler.
        Rewrite it to a dict of selector -> handler functions and a one-line
        dispatch, extracting every arm's body as a private function named from
        its literal. The handler signature is the UNION of the arms' free
        variables (minus the selector) so the dispatch call is uniform."""
        module = cst.parse_module(source)
        wrapper = cst.MetadataWrapper(module)
        finder = CstMechanics._FindFnLine(line)
        wrapper.visit(finder)
        fn = finder.found
        if fn is None or not CstMechanics._module_contains(wrapper.module, fn):
            return None
        body = list(fn.body.body)
        shaped = DispatchRegistry._dispatch_chain_shape(fn, body)
        if shaped is None:
            return None
        preamble, chain, selector, default = shaped

        # the LAMBDA TABLE when every arm is a single expression — the pure data
        # form (selector adjacent to its expression, closures capture the scope,
        # no names, no plumbing — the rule-table principle). Multi-statement arms
        # fall back to named handlers (a lambda cannot hold a body).
        exprs = [DispatchRegistry._arm_single_expression(arm_body) for _lit, arm_body in chain]
        if all(e is not None for e in exprs):
            return DispatchRegistry._dispatch_lambda_mode(fn, wrapper, shaped, exprs)
        return DispatchRegistry._dispatch_named_mode(fn, wrapper, shaped)

    class _BoundNames(cst.CSTVisitor):
        """Names ASSIGNED inside a node — the arm's locals (not free vars)."""

        def __init__(self) -> None:
            self.bound: set[str] = set()

        @override
        def visit_Assign(self, node: cst.Assign) -> None:
            for t in node.targets:
                self.bound.update(CstMechanics._hoist_target_names(t.target))

        @override
        def visit_AnnAssign(self, node: cst.AnnAssign) -> None:
            if node.target:
                self.bound.update(CstMechanics._hoist_target_names(node.target))

        @override
        def visit_For(self, node: cst.For) -> None:
            self.bound.update(CstMechanics._hoist_target_names(node.target))

        @override
        def visit_CompFor(self, node: cst.CompFor) -> None:
            self.bound.update(CstMechanics._hoist_target_names(node.target))

        @override
        def visit_With(self, node: cst.With) -> None:
            for item in node.items:
                if item.asname is not None:
                    self.bound.update(CstMechanics._hoist_target_names(item.asname.name))

    @staticmethod
    def _arm_single_expression(arm_body: list):
        """The returned expression when the arm is exactly `return <expr>`, else
        None — the lambda-table eligibility test."""
        if len(arm_body) != 1:
            return None
        st = arm_body[0]
        if not isinstance(st, cst.SimpleStatementLine) or len(st.body) != 1:
            return None
        r = st.body[0]
        if isinstance(r, cst.Return) and r.value is not None:
            return r.value
        return None

    @staticmethod
    def _dispatch_arms(stmts: list) -> _DispatchArms:
        """The contiguous dispatch arms over ONE selector from a statement run —
        (chain, selector) or (None, None) when an arm is not the table shape."""
        chain = []
        selector: str | None = None
        for stmt in stmts:
            if not isinstance(stmt, cst.If):
                break
            parsed = DispatchRegistry._parse_dispatch_arm(stmt)
            if parsed is None:
                return None, None
            sel, lit, arm_body = parsed
            if selector is None:
                selector = sel
            elif sel != selector:
                return None, None
            chain.append((lit, arm_body))
        return chain, selector

    @staticmethod
    def _dispatch_build(chain: _DispatchChain, union: list[str], existing: set[str]) -> _DispatchBuild:
        """The handler functions (_<slug> taking the free-var union) and the
        registry dict. The handlers are defined before the registry — the dict
        evaluates their names at module load. The literal IS the handler's name
        (the selector value is the domain vocabulary) — no "route" prefix; a
        collision with an existing module name gets a numeric suffix."""
        handlers: list[cst.FunctionDef] = []
        registry_entries: list[cst.DictElement] = []
        used: set[str] = set(existing)  # never shadow an existing module name
        for i, (lit, arm_body) in enumerate(chain):
            slug = "".join(ch if ch.isalnum() else "_" for ch in str(lit).lower()).strip("_")
            slug = slug or f"arm{i}"
            base, n = slug, 1
            while f"_{base}" in used:
                base, n = f"{slug}_{n}", n + 1
            used.add(f"_{base}")
            name = f"_{base}"
            handlers.append(
                cst.FunctionDef(
                    name=cst.Name(name),
                    params=cst.Parameters(params=[cst.Param(cst.Name(v)) for v in union]),
                    body=cst.IndentedBlock(body=arm_body),
                )
            )
            registry_entries.append(cst.DictElement(key=cst.SimpleString(repr(str(lit))), value=cst.Name(name)))
        registry = cst.SimpleStatementLine(
            body=[
                cst.Assign(targets=[cst.AssignTarget(cst.Name("_REGISTRY"))], value=cst.Dict(elements=registry_entries))
            ]
        )
        return handlers, registry

    @staticmethod
    def _dispatch_call(selector: str, default, union: list[str]) -> list:
        """The rewritten dispatch: registry lookup, the no-match default, the
        uniform handler call."""
        return [
            cst.SimpleStatementLine(
                body=[
                    cst.Assign(
                        targets=[cst.AssignTarget(cst.Name("handler"))],
                        value=cst.Call(
                            func=cst.Attribute(value=cst.Name("_REGISTRY"), attr=cst.Name("get")),
                            args=[cst.Arg(cst.Name(selector))],
                        ),
                    )
                ]
            ),
            cst.If(
                test=cst.Comparison(
                    left=cst.Name("handler"),
                    comparisons=[cst.ComparisonTarget(cst.Is(), cst.Name("None"))],
                ),
                body=cst.IndentedBlock(body=[cst.SimpleStatementLine(body=[default])]),
                orelse=None,
            ),
            cst.SimpleStatementLine(
                body=[cst.Return(cst.Call(func=cst.Name("handler"), args=[cst.Arg(cst.Name(v)) for v in union]))]
            ),
        ]

    @staticmethod
    def _dispatch_chain_shape(fn: cst.FunctionDef, body: list) -> _DispatchShape | None:
        """The dispatch-chain shape of a function: a PREAMBLE (locals computed
        before the chain), the >=3 arms over ONE selector, and a single trailing
        `return <default>`. None when the body is not that shape."""
        first_if = next((i for i, s in enumerate(body) if isinstance(s, cst.If)), None)
        if first_if is None:
            return None
        preamble = body[:first_if]
        chain, selector = DispatchRegistry._dispatch_arms(body[first_if:])
        if chain is None:
            return None
        if len(chain) < 3:
            return None  # >= 3 arms make a registry worth it
        tail = body[first_if + len(chain) :]
        if not DispatchRegistry._is_single_return_tail(tail):
            return None  # v1: a single trailing `return <default>` is the no-match path
        return preamble, chain, selector or "", tail[0].body[0]

    @staticmethod
    def _dispatch_lambda_call(selector: str, default) -> list:
        """The dispatch: `_handler = _tools.get(sel)`; the no-match default; the
        zero-arg lambda call."""
        return [
            cst.SimpleStatementLine(
                body=[
                    cst.Assign(
                        targets=[cst.AssignTarget(cst.Name("_handler"))],
                        value=cst.Call(
                            func=cst.Attribute(value=cst.Name("_tools"), attr=cst.Name("get")),
                            args=[cst.Arg(cst.Name(selector))],
                        ),
                    )
                ]
            ),
            cst.If(
                test=cst.Comparison(
                    left=cst.Name("_handler"),
                    comparisons=[cst.ComparisonTarget(cst.Is(), cst.Name("None"))],
                ),
                body=cst.IndentedBlock(body=[cst.SimpleStatementLine(body=[default])]),
                orelse=None,
            ),
            cst.SimpleStatementLine(body=[cst.Return(cst.Call(func=cst.Name("_handler"), args=[]))]),
        ]

    @staticmethod
    def _dispatch_lambda_mode(fn, wrapper, shaped: _DispatchShape, exprs) -> str:
        """The lambda-table rewrite of a single-expression dispatch: `_tools =
        {"lit": lambda: <expr>, ...}` inside the fn (the closures capture the
        scope) + a lookup dispatch. The fn is replaced in place — the table
        needs no module-level additions."""
        preamble, chain, selector, default = shaped
        table = DispatchRegistry._dispatch_lambda_table(chain, exprs)
        dispatch = DispatchRegistry._dispatch_lambda_call(selector, default)
        new_fn = fn.with_changes(body=fn.body.with_changes(body=preamble + [table] + dispatch))
        return cst.Module(body=[new_fn if s is fn else s for s in wrapper.module.body]).code

    @staticmethod
    def _dispatch_lambda_table(chain: _DispatchChain, exprs: list) -> cst.SimpleStatementLine:
        """`_tools = {"lit": lambda: <expr>, ...}` — the hoisted latent data
        structure. The lambdas capture the enclosing scope: no free-var analysis,
        no names, no param plumbing."""
        entries = [
            cst.DictElement(
                key=cst.SimpleString(repr(str(lit))),
                value=cst.Lambda(params=cst.Parameters(), body=expr),
            )
            for (lit, _), expr in zip(chain, exprs, strict=True)
        ]
        return cst.SimpleStatementLine(
            body=[cst.Assign(targets=[cst.AssignTarget(cst.Name("_tools"))], value=cst.Dict(elements=entries))]
        )

    @staticmethod
    def _dispatch_named_mode(fn, wrapper, shaped: _DispatchShape) -> str | None:
        """The named-handler rewrite of a multi-statement dispatch: module-level
        `_<slug>` handlers (literal-derived, collision-guarded) + the registry
        dict + a lookup dispatch.

        Two scope cases the extraction must not break: an arm that READS the
        SELECTOR gets it passed as the first handler parameter (module-level
        handlers cannot capture the fn's locals); an arm that reads a name
        BOUND IN A SIBLING arm is refused — the original if/elif runs one arm,
        so the value may not exist at the uniform call site, and the rewrite
        would crash every selector instead of only the broken one."""
        preamble, chain, selector, default = shaped
        scope = DispatchRegistry._dispatch_scope_analysis(chain, selector)
        if scope is None:
            return None  # sibling-bound read: not preservable — refuse
        bounds, reads, selector_read = scope
        union: list[str] = []
        for i, _unused in enumerate(chain):
            free = [n for n in reads[i] if n not in bounds[i] and n not in union]
            union.extend(free)
        if selector_read:
            union = [selector] + [v for v in union if v != selector]
        handlers, registry = DispatchRegistry._dispatch_build(chain, union, DispatchRegistry._module_names(wrapper))
        dispatch = DispatchRegistry._dispatch_call(selector, default, union)
        new_fn = fn.with_changes(body=fn.body.with_changes(body=preamble + dispatch))
        out_body: list = []
        for stmt in wrapper.module.body:
            if stmt is fn:
                out_body.append(new_fn)
                out_body.extend(handlers)
                out_body.append(registry)
            else:
                out_body.append(stmt)
        return cst.Module(body=out_body).code

    @staticmethod
    def _dispatch_scope_analysis(chain, selector) -> tuple[list[set[str]], list[set[str]], bool] | None:
        """Per-arm bound + read name analysis. Returns (bounds, reads, selector_read)
        or None when an arm reads a name bound in a sibling arm (the value does
        not exist at the uniform call site — the rewrite would crash every
        selector instead of only the broken one)."""
        bounds: list[set[str]] = []
        reads: list[set[str]] = []
        for _lit, arm_body in chain:
            bound = DispatchRegistry._BoundNames()
            for st in arm_body:
                st.visit(bound)
            bounds.append(bound.bound)
            reads.append(set(DispatchRegistry._read_names(arm_body)))
        for i in range(len(chain)):
            for j, b in enumerate(bounds):
                if i != j and reads[i] & b:
                    return None
        return bounds, reads, any(selector in r for r in reads)

    @staticmethod
    def _is_single_return_tail(tail: list) -> bool:
        """The function ends with exactly one `return <value>` statement."""
        return (
            len(tail) == 1
            and isinstance(tail[0], cst.SimpleStatementLine)
            and len(tail[0].body) == 1
            and isinstance(tail[0].body[0], cst.Return)
        )

    @staticmethod
    def _module_names(wrapper: cst.MetadataWrapper) -> set[str]:
        """The top-level def/class names — the collision guard's existing-name
        set for generated handler names."""
        names = set()
        for s in wrapper.module.body:
            if isinstance(s, (cst.FunctionDef, cst.ClassDef)):
                names.add(s.name.value)
        return names

    @staticmethod
    def _parse_dispatch_arm(stmt: cst.If) -> _DispatchArm | None:
        """Validate one dispatch arm: `if sel == "lit":` with an IndentedBlock
        body. Returns (selector, literal, body statements)."""
        test = stmt.test
        if not isinstance(test, cst.Comparison) or len(test.comparisons) != 1:
            return None
        left = test.left
        comp = test.comparisons[0]
        if not isinstance(comp.operator, cst.Equal) or not isinstance(comp.comparator, cst.SimpleString):
            return None
        if not isinstance(left, cst.Name):
            return None
        if stmt.orelse:
            return None
        body = stmt.body
        if not isinstance(body, cst.IndentedBlock) or not body.body:
            return None
        return left.value, comp.comparator.evaluated_value, list(body.body)

    @staticmethod
    def _read_names(nodes) -> list[str]:
        """The Names READ in `nodes` — the arm's free-var candidates. Attribute
        names (`args.get` -> the `get`) and their base-members are NOT reads of
        standalone names: only the BASE (`args`) is. Builtins are ambient."""
        reads: list[str] = []
        seen: set[str] = set()

        def walk(n) -> None:
            if isinstance(n, cst.Name):
                if n.value not in seen and n.value not in _AMBIENT:
                    seen.add(n.value)
                    reads.append(n.value)
            elif isinstance(n, cst.Attribute):
                walk(n.value)  # the base only — the attr name is not a free var
            else:
                for ch in getattr(n, "children", []):
                    walk(ch)

        for n in nodes:
            walk(n)
        return reads

"""The rule battery becomes a table.

Each (condition, violation) pair is data, and one loop collects the
violations whose condition holds.
"""

from __future__ import annotations

from typing import NamedTuple

import libcst as cst

from fix_engine.module_outline import ModuleOutline


class _AccumulatorInit(NamedTuple):
    """The `acc = []` opener: the accumulator's name and where it sits."""

    name: str
    index: int


class _BatteryCheck(NamedTuple):
    """One rule-battery check: its condition and the violation it appends."""

    condition: cst.BaseExpression
    violation: cst.BaseExpression


class _BatteryShape(NamedTuple):
    """A rule battery's parts: the accumulator, the checks, the locals before
    it, and the statements after it."""

    accumulator: str
    checks: list[_BatteryCheck]
    preamble: list
    tail: list


class _RuleTablePlan(NamedTuple):
    """The hoisted table statement and the collector that produces the
    accumulator."""

    table: cst.SimpleStatementLine
    collector: cst.SimpleStatementLine


class RuleTable:
    """One request's rule table fix. The request is the fixer's state."""

    def __init__(self, request) -> None:
        self.request = request

    def fix_rule_table(self) -> str | None:
        source, line = self.request._loaded_source(), self.request.line
        """A rule battery (`if cond: violations.append(...)` repeated) is a
        LATENT DATA STRUCTURE — a table of (condition, violation) pairs. Hoist
        it: each check becomes a tuple `(lambda: <cond>, <violation>)`, the
        function collects the violations whose condition holds. The lambdas
        capture the enclosing scope (shared preamble locals need no plumbing)
        and need NO names — the naming problem dissolves. v1: each check is a
        single `acc.append(<value>)` with no else."""
        module = cst.parse_module(source)
        wrapper = cst.MetadataWrapper(module)
        outline = ModuleOutline(wrapper)
        fn = outline.function_at(line)
        if fn is None or not outline.contains(fn):
            return None
        body = list(fn.body.body)
        shaped = RuleTable._rule_battery_shape(fn, body)
        if shaped is None:
            return None
        acc, checks, preamble, tail = shaped
        table, collector = RuleTable._rule_table_build(acc, checks)
        new_fn = fn.with_changes(body=fn.body.with_changes(body=preamble + [table, collector] + tail))
        # the lambdas live IN the fn — no module-level additions
        out_body: list = [new_fn if stmt is fn else stmt for stmt in wrapper.module.body]
        return cst.Module(body=out_body).code

    @staticmethod
    def _acc_init(body: list) -> _AccumulatorInit | None:
        """The `acc = []` opener (plain or annotated `acc: list = []`) — (acc
        name, its index) or None. Preamble statements may precede it (the hoisted
        lambdas capture them)."""
        for i, stmt in enumerate(body):
            if not isinstance(stmt, cst.SimpleStatementLine) or len(stmt.body) != 1:
                continue
            a = stmt.body[0]
            if RuleTable._is_empty_list_assign(a):
                assert isinstance(a, cst.Assign) and isinstance(a.targets[0].target, cst.Name)
                return _AccumulatorInit(a.targets[0].target.value, i)
            if RuleTable._is_empty_list_annassign(a):
                assert isinstance(a, cst.AnnAssign) and isinstance(a.target, cst.Name)
                return _AccumulatorInit(a.target.value, i)
        return None

    @staticmethod
    def _append_value(branch, acc: str):
        """The value of a single `acc.append(<value>)` statement, or None when
        the branch is not exactly that."""
        if not isinstance(branch, cst.IndentedBlock) or len(branch.body) != 1:
            return None
        app = branch.body[0]
        if not isinstance(app, cst.SimpleStatementLine) or len(app.body) != 1:
            return None
        app_stmt = app.body[0]
        if not isinstance(app_stmt, cst.Expr) or not isinstance(app_stmt.value, cst.Call):
            return None
        call = app_stmt.value
        if not RuleTable._is_accumulator_append(call, acc) or len(call.args) != 1:
            return None
        return call.args[0].value

    @staticmethod
    def _is_accumulator_append(call: cst.Call, acc: str) -> bool:
        """`<acc>.append(...)` — the loop's per-item write."""
        return (
            isinstance(call.func, cst.Attribute)
            and call.func.attr.value == "append"
            and isinstance(call.func.value, cst.Name)
            and call.func.value.value == acc
        )

    @staticmethod
    def _is_empty_list_annassign(stmt: cst.BaseSmallStatement) -> bool:
        """`name: list = []` — the annotated form of the accumulator."""
        return (
            isinstance(stmt, cst.AnnAssign)
            and isinstance(stmt.value, cst.List)
            and len(stmt.value.elements) == 0
            and isinstance(stmt.target, cst.Name)
        )

    @staticmethod
    def _is_empty_list_assign(stmt: cst.BaseSmallStatement) -> bool:
        """`name = []` — the empty-list accumulator's declaration."""
        return (
            isinstance(stmt, cst.Assign)
            and isinstance(stmt.value, cst.List)
            and len(stmt.value.elements) == 0
            and isinstance(stmt.targets[0].target, cst.Name)
        )

    @staticmethod
    def _rule_battery_shape(fn: cst.FunctionDef, body: list) -> _BatteryShape | None:
        """The rule-battery shape: `acc = []`, >= 3 `if <cond>: acc.append(<v>)`
        checks (each a single append, no else), then anything as the tail. The
        conditions may read ANY enclosing name — the hoisted lambdas capture the
        scope. Returns (acc, [(cond, value), ...], preamble, tail)."""
        probe = RuleTable._acc_init(body)
        if probe is None:
            return None
        acc, init_idx = probe
        preamble = list(body[:init_idx])  # locals computed before the init —
        # the hoisted lambdas CAPTURE them, so they need no plumbing
        checks: list = []
        idx = init_idx + 1
        while idx < len(body) and isinstance(body[idx], cst.If):
            stmt = body[idx]
            if stmt.orelse:
                return None
            value = RuleTable._append_value(stmt.body, acc)
            if value is None:
                return None  # v1: one append per check
            checks.append(_BatteryCheck(stmt.test, value))
            idx += 1
        if len(checks) < 3:
            return None  # fewer than 3 checks is not a battery
        # everything after the last check is the TAIL — kept verbatim (the
        # collector comprehension produces `acc`, then the tail uses it)
        return _BatteryShape(acc, checks, preamble, list(body[idx:]))

    @staticmethod
    def _rule_table_build(acc: str, checks: list[_BatteryCheck]) -> _RuleTablePlan:
        """The hoisted table — `rules = [(lambda: <cond>, <violation>), ...]` —
        and the collector comprehension `acc = [v for _cond, v in rules if _cond()]`.
        The lambdas close over the enclosing scope: shared preamble locals are
        captured, and no condition needs a name."""
        entries = []
        for cond, value in checks:
            # BOTH the condition and the value are lambdas: the original
            # semantics evaluate the value ONLY when its condition holds (a
            # guard-then-use `if d.get("k"): violations.append(d["k"])` must not
            # KeyError at table-construction time), and in source order
            entries.append(
                cst.Element(
                    value=cst.Tuple(
                        elements=[
                            cst.Element(value=cst.Lambda(params=cst.Parameters(), body=cond)),
                            cst.Element(value=cst.Lambda(params=cst.Parameters(), body=value)),
                        ]
                    )
                )
            )
        table = cst.SimpleStatementLine(
            body=[cst.Assign(targets=[cst.AssignTarget(cst.Name("rules"))], value=cst.List(elements=entries))]
        )
        collector = cst.SimpleStatementLine(
            body=[
                cst.Assign(
                    targets=[cst.AssignTarget(cst.Name(acc))],
                    value=cst.ListComp(
                        elt=cst.Call(func=cst.Name("_val"), args=[]),
                        for_in=cst.CompFor(
                            target=cst.Tuple(elements=[cst.Element(cst.Name("_cond")), cst.Element(cst.Name("_val"))]),
                            iter=cst.Name("rules"),
                            ifs=[cst.CompIf(test=cst.Call(func=cst.Name("_cond"), args=[]))],
                        ),
                    ),
                )
            ]
        )
        return _RuleTablePlan(table, collector)

"""The partition shape arm: a field-disjoint class splits by method group.

One fix family, one class; the class holds the request, its helpers are
members, and its libcst visitors are nested classes.
"""

from __future__ import annotations

import keyword
from typing import NamedTuple, override

import libcst as cst

from fix_engine.bindings import Bindings
from fix_engine.module_outline import ModuleOutline
from fix_engine.rewrite import Rewrite


class _InitPlan(NamedTuple):
    """What a class's __init__ does, in order: the field initializers, the
    docstring statement, and the parameters those initializers need."""

    fields: list[tuple[str, cst.BaseExpression]]
    docstring: cst.BaseStatement | None
    params: list[tuple[str, cst.Annotation | None, cst.BaseExpression | None]]


class Partition:
    """The field-disjoint split for one request: one class per method group."""

    class _PartitionGroup(NamedTuple):
        """One field-disjoint method group: its methods in source order and the
        fields only they touch."""

        methods: tuple[cst.FunctionDef, ...]
        fields: tuple[str, ...]

    class _GroupInit(NamedTuple):
        """One group's synthesized __init__: the field assignments it keeps and
        the parameters those assignments need (name, annotation, default)."""

        statements: list[cst.BaseStatement]
        params: list[cst.Param]

    class _PartitionPlan(NamedTuple):
        groups: tuple[Partition._PartitionGroup, ...]
        init_fields: tuple[tuple[str, cst.BaseExpression], ...]
        init_docstring: cst.BaseStatement | None
        init_params: tuple[tuple[str, cst.Annotation | None, cst.BaseExpression | None], ...]

    class _PartitionAttempt(NamedTuple):
        """The plan, or the specific reason it cannot be computed."""

        plan: Partition._PartitionPlan | None
        decline: str

    def __init__(self, request) -> None:
        self.request = request

    def run(self) -> str | None:
        """Split the field-disjoint class into one class per method group: the
        group's fields move into its synthesized __init__, the first group keeps
        the class's name, and every group keeps the shared bases. Declines when a
        connector method (or unattributable __init__ logic) ties the groups
        together."""
        source, line = self.request.loaded_source(), self.request.line
        # the wrapper's own tree: the visitor's nodes must be the ones spliced
        # back into `module.body`, and metadata resolves by NODE IDENTITY
        wrapper = cst.MetadataWrapper(cst.parse_module(source))
        module = wrapper.module
        cls = ModuleOutline(wrapper).enclosing_class(line)
        if cls is None or not any(stmt is cls for stmt in module.body):
            self.request.decline = f"no module-level class anchored at {self.request.rel}:{line} — the anchor is stale"
            return None
        attempt = Partition._partition_plan(cls)
        if attempt.plan is None:
            self.request.decline = attempt.decline
            return None
        plan = attempt.plan
        base = self.request.opts.name or cls.name.value
        if not base.isidentifier() or keyword.iskeyword(base):
            self.request.decline = f"'{base}' is not a usable class-name prefix — pass --name <Prefix>"
            return None
        class_names = [base]
        for group in plan.groups[1:]:
            class_names.append(base + Rewrite.pascal_case(group.fields[0]))
        if len(set(class_names)) != len(class_names):
            self.request.decline = "two groups derive the same class name — pass --name <Prefix> to disambiguate"
            return None
        for name in class_names:
            if name != cls.name.value and Bindings.binds_name(module, name):
                self.request.decline = f"'{name}' is already bound in {self.request.rel} — pass another --name"
                return None
        class_docstring = next(
            (
                stmt
                for stmt in cls.body.body
                if isinstance(stmt, cst.SimpleStatementLine) and ModuleOutline.is_docstring_stmt(stmt)
            ),
            None,
        )
        replacements: list[cst.BaseStatement] = []
        for i, group in enumerate(plan.groups):
            init_stmts: list[cst.BaseStatement] = []
            if i == 0 and plan.init_docstring is not None:
                init_stmts.append(plan.init_docstring)
            retained = [(field, value) for field, value in plan.init_fields if field in group.fields]
            init_stmts.extend(Partition._init_stmt(field, value) for field, value in retained)
            needed = set(group.fields)
            for _field, value in retained:
                needed |= set(Bindings.expr_names(value))
            # a valid Python signature keeps its defaulted parameters last, so any
            # kept subset is valid: a skipped parameter can never be required
            init_params = [
                cst.Param(name=cst.Name(name), annotation=annotation, default=default)
                for name, annotation, default in plan.init_params
                if name in needed
            ]
            head: list[cst.BaseStatement] = []
            if i == 0 and class_docstring is not None:
                head.append(class_docstring)
            group_cls = Partition._group_class(
                class_names[i], cls, list(group.methods), Partition._GroupInit(init_stmts, init_params), head
            )
            if i:
                group_cls = group_cls.with_changes(leading_lines=[cst.EmptyLine()])
            replacements.append(group_cls)
        body: list[cst.BaseStatement] = list(module.body)
        index = next(i for i, stmt in enumerate(body) if stmt is cls)
        body[index : index + 1] = replacements
        return module.with_changes(body=body).code

    @staticmethod
    def _partition_plan(cls: cst.ClassDef) -> Partition._PartitionAttempt:
        """The field-disjoint split of `cls`, or the reason it cannot be split —
        connector methods merge the groups and produce the refusal."""
        methods = [stmt for stmt in cls.body.body if isinstance(stmt, cst.FunctionDef)]
        extra = [
            stmt
            for stmt in cls.body.body
            if not isinstance(stmt, cst.FunctionDef) and not ModuleOutline.is_docstring_stmt(stmt)
        ]
        if extra:
            return Partition._PartitionAttempt(
                None,
                "the class carries statements besides a docstring — they cannot be attributed "
                "to one group; split by hand",
            )
        if len(methods) < 6:
            return Partition._PartitionAttempt(
                None,
                f"the class has only {len(methods)} methods — not the latent-partition shape (6+); the anchor is stale",
            )
        init = next((method for method in methods if method.name.value == "__init__"), None)
        parsed = Partition._init_plan(init)
        if parsed is None:
            return Partition._PartitionAttempt(
                None,
                "the class's __init__ does more than assign self fields — a split would reorder or "
                "retarget a statement; split by hand",
            )
        init_fields, init_docstring, init_params = parsed.fields, parsed.docstring, parsed.params
        fielded = [(method, Partition._self_field_reads(method)) for method in methods if method is not init]
        fieldless = [method.name.value for method, fields in fielded if not fields]
        if fieldless:
            return Partition._PartitionAttempt(
                None,
                f"'{fieldless[0]}' touches no self fields — it belongs to no group; split by hand",
            )
        fields_of = {method.name.value: fields for method, fields in fielded}
        order = {method.name.value: i for i, (method, _) in enumerate(fielded)}
        groups: list[list[str]] = []
        seen: set[str] = set()
        for method, _ in fielded:
            if method.name.value in seen:
                continue
            component: list[str] = []
            stack = [method.name.value]
            while stack:
                current = stack.pop()
                if current in seen:
                    continue
                seen.add(current)
                component.append(current)
                for other, other_fields in fielded:
                    if other.name.value not in seen and not fields_of[current].isdisjoint(other_fields):
                        stack.append(other.name.value)
            groups.append(component)
        if len(groups) < 2:
            return Partition._PartitionAttempt(
                None,
                "the methods share fields across the whole class (a connector method ties the "
                "groups together) — the split would not be field-disjoint; split by hand",
            )
        group_fields = [tuple(sorted({f for name in group for f in fields_of[name]})) for group in groups]
        if any(len(group) < 2 or len(fields) < 2 for group, fields in zip(groups, group_fields, strict=True)):
            return Partition._PartitionAttempt(
                None,
                "a method group is too small (needs 2+ methods and 2+ fields) — the split would not "
                "produce classes; split by hand",
            )
        all_fields = {field for _, fields in fielded for field in fields} | {field for field, _ in init_fields}
        attributed = {field for fields in group_fields for field in fields}
        orphan = sorted(all_fields - attributed)
        if orphan:
            return Partition._PartitionAttempt(
                None,
                f"'{orphan[0]}' is initialized but touched by no method — it belongs to no group; split by hand",
            )
        plan = Partition._PartitionPlan(
            groups=tuple(
                Partition._PartitionGroup(tuple(fielded[i][0] for i in sorted(order[name] for name in group)), fields)
                for group, fields in zip(groups, group_fields, strict=True)
            ),
            init_fields=tuple(init_fields),
            init_docstring=init_docstring,
            init_params=tuple(init_params),
        )
        return Partition._PartitionAttempt(plan, "")

    @staticmethod
    def _self_field_reads(node: cst.CSTNode) -> set[str]:
        """The `self.<attr>` fields a node touches (nested bodies included — the
        scanner's rule counts them too)."""
        found: set[str] = set()

        class _Attrs(cst.CSTVisitor):
            @override
            def visit_Attribute(self, node) -> None:
                if isinstance(node.value, cst.Name) and node.value.value == "self":
                    found.add(node.attr.value)

        node.visit(_Attrs())
        return found

    @staticmethod
    def _reads_self(expr: cst.BaseExpression) -> bool:
        """Does the expression read `self.<attr>`? An initializer that does would
        have to move between classes in order — not mechanical."""
        return bool(Partition._self_field_reads(expr))

    @staticmethod
    def _init_plan(init: cst.FunctionDef | None) -> _InitPlan | None:
        """__init__'s `self.x = <expr>` statements (plus its docstring) in order;
        None when the body does anything else — a split would have to reorder or
        retarget that statement."""
        if init is None:
            return _InitPlan([], None, [])
        params = init.params
        if Partition._has_irregular_params(params):
            return None
        init_params = [(param.name.value, param.annotation, param.default) for param in params.params[1:]]
        assigns: list[tuple[str, cst.BaseExpression]] = []
        docstring: cst.SimpleStatementLine | None = None
        for stmt in init.body.body:
            small = stmt.body[0] if isinstance(stmt, cst.SimpleStatementLine) and len(stmt.body) == 1 else None
            if small is None:
                return None
            if isinstance(small, cst.Expr) and isinstance(small.value, (cst.SimpleString, cst.ConcatenatedString)):
                assert isinstance(stmt, cst.SimpleStatementLine)
                docstring = stmt
                continue
            target = small.targets[0].target if isinstance(small, cst.Assign) and len(small.targets) == 1 else None
            if not Partition._is_plain_self_assign(target, small):
                return None
            assert isinstance(small, cst.Assign) and isinstance(target, cst.Attribute)
            assigns.append((target.attr.value, small.value))
        return _InitPlan(assigns, docstring, init_params)

    @staticmethod
    def _init_stmt(field: str, value: cst.BaseExpression) -> cst.BaseStatement:
        """One `self.<field> = <value>` line for a group's synthesized __init__."""
        return cst.SimpleStatementLine(
            body=[
                cst.Assign(
                    targets=Rewrite.self_attr_target(field),
                    value=value,
                )
            ]
        )

    @staticmethod
    def _group_class(
        name: str,
        template: cst.ClassDef,
        methods: list[cst.FunctionDef],
        init: Partition._GroupInit,
        head: list[cst.BaseStatement],
    ) -> cst.ClassDef:
        """One group's class: the template's decorators, bases, and keywords, a
        synthesized __init__ over its own fields (and the parameters they need),
        then its methods."""
        body: list[cst.BaseStatement] = list(head)
        if init.statements:
            body.append(
                cst.FunctionDef(
                    name=cst.Name("__init__"),
                    params=cst.Parameters(params=[cst.Param(cst.Name("self")), *init.params]),
                    body=cst.IndentedBlock(body=init.statements),
                )
            )
        body.extend(methods)
        if isinstance(body[0], (cst.FunctionDef, cst.ClassDef)) and body[0].leading_lines:
            body[0] = body[0].with_changes(leading_lines=[])
        return template.with_changes(name=cst.Name(name), body=cst.IndentedBlock(body=body))

    @staticmethod
    def _has_irregular_params(params: cst.Parameters) -> bool:
        """The signature has positional-only, keyword-only, or variadic
        parameters."""
        return (
            bool(params.posonly_params)
            or bool(params.kwonly_params)
            or isinstance(params.star_arg, cst.Param)
            or params.star_kwarg is not None
        )

    @staticmethod
    def _is_plain_self_assign(target, stmt) -> bool:
        """The statement is `self.<attr> = <value not read from self>` — one field
        initialised from outside."""
        if target is None or not ModuleOutline.is_self_attribute(target):
            return False
        return isinstance(stmt, cst.Assign) and not Partition._reads_self(stmt.value)

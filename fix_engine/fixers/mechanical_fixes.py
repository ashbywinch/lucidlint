"""The mechanical fixers: delete, keyword, annotate, and the loop rewrites.

One concern — the transforms whose finding already names every fact they need.
The class holds the request; each helper is a member and each libcst
transformer is a nested class (a fixer's implementation, not a second concern).
"""

from __future__ import annotations

from dataclasses import dataclass
from pathlib import Path
from typing import Any, override

import libcst as cst
import libcst.matchers as m
from libcst.metadata import CodePosition, ParentNodeProvider, PositionProvider

from fix_engine.bindings import Bindings
from fix_engine.module_outline import ModuleOutline
from fix_engine.rewrite import Rewrite
from fix_engine.source_files import SourceFiles


class MechanicalFixes:
    """The mechanical fix families for one request: the deletes, keyword
    arguments, the member annotation, and the three loop rewrites."""

    _HOIST_MUTATORS = {
        "append",
        "add",
        "extend",
        "appendleft",
        "update",
        "setdefault",
        "add_update",
        "discard",
        "remove",
    }
    _OTHER_MUTATORS = _HOIST_MUTATORS | {
        "pop",
        "insert",
        "sort",
        "reverse",
        "clear",
        "put",
        "push",
        "enqueue",
        "__setitem__",
    }

    @dataclass
    class _LoopPipelinePlan:
        """One loop's accumulation shape for the comprehension rewrite: the
        accumulator, the collection kind, and the element/key/flatten exprs."""

        acc: str
        collection: str  # "list" | "set" | "dict"
        element: cst.BaseExpression | None  # the appended/added value or the dict value
        key: cst.BaseExpression | None  # the dict key (subscript store) or None
        flatten: cst.BaseExpression | None  # the extend/`+= [a, b]` value whose ITEMS join
        if_test: cst.BaseExpression | None  # the one-filter test or None

    def __init__(self, request: Any) -> None:
        self.request = request

    def run(self) -> str | None:
        kind, source, line, opts = (
            self.request.kind,
            self.request.loaded_source(),
            self.request.line,
            self.request.opts,
        )
        """The mechanical transforms — a changed source, or None when the callee
        is unresolvable (the retry protocol supplies params)."""
        if kind in ("noop-statement", "unreachable"):
            return cst.MetadataWrapper(cst.parse_module(source)).visit(MechanicalFixes._DeleteStatement(line)).code
        if kind == "stale-suppression":
            return cst.MetadataWrapper(cst.parse_module(source)).visit(MechanicalFixes._DeleteComment(line)).code
        if kind == "positional-literals":
            params = opts.params if opts.params is not None else self._callee_params_for_call()
            if params is None:
                self.request.decline = (
                    "the callee's parameter names could not be resolved from this file — "
                    "pass --params entries (comma,separated,parameter,names)"
                )
                return None
            xform = MechanicalFixes._KeywordArgs(line, params, opts.callee)
            out = cst.MetadataWrapper(cst.parse_module(source)).visit(xform).code
            return out if xform.applied else None
        if kind == "duplicate-def":
            return self.fix_duplicate_def()
        if kind == "restating-docstring":
            return self.fix_restating_docstring()
        if kind == "duplicate-block":
            return self.fix_duplicate_block()
        if kind == "undeclared-attribute":
            param_types = self._member_param_types(line)
            return (
                cst.MetadataWrapper(cst.parse_module(source))
                .visit(MechanicalFixes._DeclareMember(line, param_types))
                .code
            )
        if kind in ("loop-pipeline", "loop-sequence"):
            xform = MechanicalFixes._LoopsIntoComprehensions(
                line, sequence=(kind == "loop-sequence"), fresh_name=MechanicalFixes._fresh_loop_name(source)
            )
            out = cst.MetadataWrapper(cst.parse_module(source)).visit(xform).code
            if xform.decline:
                self.request.decline = xform.decline
                return None
            return out if xform.applied else None
        if kind == "loop-hoist":
            if not opts.name:
                self.request.decline = (
                    "the hoisted helper needs a semantic name — pass --fix-name <Name> "
                    "(a domain noun for what one item contributes)"
                )
                return None
            xform = MechanicalFixes._LoopHoistExtractor(
                line,
                name=opts.name,
                helper_local=MechanicalFixes._fresh_loop_name(source, base="val"),
                element=MechanicalFixes._fresh_loop_name(source, base="part"),
            )
            out = cst.MetadataWrapper(cst.parse_module(source)).visit(xform).code
            if xform.decline:
                self.request.decline = xform.decline
                return None
            return out if xform.applied else None
        return None

    def fix_duplicate_def(self) -> str | None:
        source, line = self.request.loaded_source(), self.request.line
        """Delete the shadowing (second) module-scope binding when nothing
        references it — proven by a repo-wide Name count (only the two def sites
        hit). Referenced shadows are renamed by the agent, never auto-deleted."""
        wrapper = cst.MetadataWrapper(cst.parse_module(source))
        probe = MechanicalFixes._FindModuleDef(line)
        wrapper.visit(probe)
        if probe.found is None or probe.name is None:
            return None
        if MechanicalFixes._name_occurrences(self.request.repo, probe.name) > 2:
            return None  # something references the name — a delete would break it
        return Rewrite(wrapper.module).without([probe.found]).code

    def fix_restating_docstring(self) -> str | None:
        source, line = self.request.loaded_source(), self.request.line
        return cst.MetadataWrapper(cst.parse_module(source)).visit(MechanicalFixes._StripDocstring(line)).code

    def fix_duplicate_block(self) -> str | None:
        source, line = self.request.loaded_source(), self.request.line
        """Delete the second copy of a repeated 3-statement block. Refuses when
        the removal would empty a block (an emptied body needs `pass` — the
        agent's call)."""
        wrapper = cst.MetadataWrapper(cst.parse_module(source))

        class _FindFn(cst.CSTVisitor):
            METADATA_DEPENDENCIES = (PositionProvider,)

            def __init__(self, target: int) -> None:
                self.target = target
                self.fn: cst.FunctionDef | None = None

            @override
            def visit_FunctionDef(self, node) -> None:
                pos = ModuleOutline.span(self.get_metadata(PositionProvider, node))
                if pos.start.line <= self.target <= pos.end.line:
                    self.fn = node  # last (innermost) containing function wins

        finder = _FindFn(line)
        wrapper.visit(finder)
        if finder.fn is None:
            return None
        flat: list = []
        MechanicalFixes._flatten_stmts(list(finder.fn.body.body), flat)
        if len(flat) < 6:
            return None
        targets: list | None = None
        for i in range(len(flat) - 5):
            for j in range(i + 3, len(flat) - 2):
                if all(flat[i + k].deep_equals(flat[j + k]) for k in range(3)):
                    pos = wrapper.resolve(PositionProvider)[flat[j]]
                    if pos.start.line == line:
                        targets = flat[j : j + 3]
                        break
            if targets is not None:
                break

        if targets is None:
            return None
        # a removal that empties a block leaves invalid Python (`pass` required) —
        # refuse when every statement of a block is a target (the agent places
        # the pass by hand)
        target_set = set(targets)
        for t in targets:
            parent = wrapper.resolve(ParentNodeProvider)[t]
            if isinstance(parent, cst.IndentedBlock):
                block_stmts = list(parent.body)
                if block_stmts and all(s in target_set for s in block_stmts):
                    return None
        return Rewrite(wrapper.module).without(targets).code

    def _member_param_types(self, line: int) -> dict[str, str]:
        """The enclosing def's param name -> annotation map, for the member
        annotation inference."""
        module = cst.parse_module(self.request.loaded_source())
        wrapper = cst.MetadataWrapper(module)
        fn = ModuleOutline(wrapper).enclosing_function(line)
        if fn is None:
            return {}
        result: dict[str, str] = {}
        for p in list(fn.params.posonly_params) + list(fn.params.params) + list(fn.params.kwonly_params):
            if p.annotation is not None:
                result[p.name.value] = cst.Module(body=[]).code_for_node(p.annotation.annotation)
        return result

    def _repo_params(self, callee: str) -> list[str] | None:
        repo, rel = self.request.repo, self.request.rel
        """Resolve the callee's params repo-wide — the call's file first, then
        every other .py under the repo (a module-level def or a class __init__).
        First match in sorted order wins; ambiguity is documented, not fatal."""
        candidates = sorted(
            p
            for p in repo.rglob("*.py")
            if p.is_file() and not any(part.startswith((".venv", "venv", "node_modules")) for part in p.parts)
        )
        # the finding's own file first (fast path + locality)
        own = repo / rel
        if own in candidates:
            candidates.remove(own)
            candidates.insert(0, own)
        for path in candidates:
            try:
                source = path.read_text(encoding="utf-8")
            except OSError:
                continue
            params = MechanicalFixes._params_of_any_def(source, callee)
            if params is not None:
                return params
        return None

    def _callee_params_for_call(self) -> list[str] | None:
        """The callee's param names for the call on `line`, resolved repo-wide.
        Mirrors the scanner's Name-callee rule: a method/builtin callee is not
        auto-fixable."""
        source, line = self.request.loaded_source(), self.request.line
        module = cst.parse_module(source)
        wrapper = cst.MetadataWrapper(module)
        finder = MechanicalFixes._FindCall(line)
        wrapper.visit(finder)
        if finder.callee is None:
            return None
        return self._repo_params(finder.callee)

    class _FindCall(cst.CSTVisitor):
        """The first Name-callee call on one source line — the call the
        keywording fix can resolve to a same-file callee."""

        METADATA_DEPENDENCIES = (PositionProvider,)

        def __init__(self, line: int) -> None:
            self.line: int = line
            self.callee: str | None = None

        @override
        def visit_Call(self, node) -> None:
            if self.callee is not None:
                return
            pos = ModuleOutline.span(self.get_metadata(PositionProvider, node))
            if pos.start.line <= self.line <= pos.end.line and isinstance(node.func, cst.Name):
                self.callee = node.func.value

    class _DeclareMember(cst.CSTTransformer):
        """Annotate the undeclared `self.x = v` member: `self.x: T = v` with T
        inferred from the assigned value. Pure — no behavior change."""

        METADATA_DEPENDENCIES = (PositionProvider,)

        def __init__(self, target_line: int, param_types: dict[str, str]):
            self.target_line: int = target_line
            self.param_types: dict[str, str] = param_types

        @override
        def leave_Assign(self, original_node, updated_node):
            pos = ModuleOutline.span(self.get_metadata(PositionProvider, original_node))
            if pos.start.line != self.target_line:
                return updated_node
            if len(updated_node.targets) != 1:
                return updated_node
            target = updated_node.targets[0].target
            if not isinstance(target, cst.Attribute):
                return updated_node
            if not isinstance(target.value, cst.Name) or target.value.value != "self":
                return updated_node
            annotation = MechanicalFixes._infer_member_type(updated_node.value, self.param_types)
            if annotation is None:
                return updated_node
            return cst.AnnAssign(
                target=target,
                annotation=cst.Annotation(cst.parse_expression(annotation)),
                value=updated_node.value,
                equal=cst.AssignEqual(),
                semicolon=updated_node.semicolon,
            )

    class _DeleteComment(cst.CSTTransformer):
        """Remove the `lucidlint: ignore` comment on the target line — both the
        standalone form (an EmptyLine's comment) and the trailing form."""

        METADATA_DEPENDENCIES = (PositionProvider,)

        def __init__(self, target_line: int) -> None:
            self.target_line: int = target_line
            self.deleted: bool = False

        @override
        def leave_EmptyLine(self, original_node, updated_node):
            if self.deleted or updated_node.comment is None:
                return updated_node
            if "lucidlint: ignore" not in updated_node.comment.value:
                return updated_node
            pos = ModuleOutline.span(self.get_metadata(PositionProvider, original_node))
            if pos.start.line == self.target_line:
                self.deleted = True
                return cst.RemoveFromParent()
            return updated_node

        @override
        def on_leave(self, original_node, updated_node):
            # the typed leave_Comment contract returns only a Comment (no removal),
            # so the trailing-form deletion hooks the untyped dispatch instead;
            # post-order keeps a comment leaving before its EmptyLine parent,
            # exactly as with a leave_Comment override
            if isinstance(updated_node, cst.Comment):
                if self.deleted or "lucidlint: ignore" not in updated_node.value:
                    return updated_node
                pos = ModuleOutline.span(self.get_metadata(PositionProvider, original_node))
                if pos.start.line == self.target_line:
                    self.deleted = True
                    return cst.RemoveFromParent()
                return updated_node
            return super().on_leave(original_node, updated_node)

    class _DeleteStatement(cst.CSTTransformer):
        """Remove the SimpleStatementLine covering the target line."""

        METADATA_DEPENDENCIES = (PositionProvider,)

        def __init__(self, target_line: int) -> None:
            self.target_line: int = target_line
            self.deleted: bool = False

        @override
        def leave_SimpleStatementLine(self, original_node, updated_node):
            if self.deleted:
                return updated_node
            pos = ModuleOutline.span(self.get_metadata(PositionProvider, original_node))
            if pos.start.line <= self.target_line <= pos.end.line:
                self.deleted = True
                return cst.RemoveFromParent()
            return updated_node

    class _FindModuleDef(cst.CSTVisitor):
        """The module-scope def (depth 0) whose def line equals the target — the
        duplicate-def finding's second binding."""

        METADATA_DEPENDENCIES = (PositionProvider,)

        def __init__(self, line: int) -> None:
            self.line: int = line
            self.depth: int = 0
            self.found: cst.CSTNode | None = None
            self.name: str | None = None

        @override
        def visit_FunctionDef(self, node) -> None:
            self._enter(node)

        @override
        def visit_ClassDef(self, node) -> None:
            self._enter(node)

        @override
        def leave_FunctionDef(self, original_node) -> None:
            self.depth -= 1

        @override
        def leave_ClassDef(self, original_node) -> None:
            self.depth -= 1

        def _enter(self, node) -> None:
            pos = ModuleOutline.span(self.get_metadata(PositionProvider, node))
            if self._is_target_at_top(pos):
                self.found, self.name = node, node.name.value
            self.depth += 1

        def _is_target_at_top(self, pos) -> bool:
            """The first top-level definition at the finding's line."""
            return self.depth == 0 and self.found is None and pos.start.line == self.line

    class _KeywordArgs(cst.CSTTransformer):
        """Keyword the positional literal args of the flagged call on the target
        line — parameter names come from the same-file callee definition or
        --params for an external one. The TARGET is selected exactly like the
        param resolver selects the callee: the OUTERMOST Name-callee call
        spanning the line (or the --callee-named one) — innermost-first binding
        keyworded a nested call with the outer call's names (houses quirk 3:
        GeoPoint(amount=0, currency=0))."""

        METADATA_DEPENDENCIES = (PositionProvider,)

        def __init__(self, target_line: int, params: list[str], callee: str | None = None) -> None:
            self.target_line: int = target_line
            self.params: list[str] = params
            self.callee: str | None = callee
            self.target_start: CodePosition | None = None  # outermost spanning call
            self.target_end: CodePosition | None = None
            self.applied: bool = False

        @override
        def visit_Call(self, node):
            if self.target_start is not None:
                return True
            if not isinstance(node.func, cst.Name):
                return True
            if self.callee is not None and node.func.value != self.callee:
                return True
            pos = ModuleOutline.span(self.get_metadata(PositionProvider, node))
            if not (pos.start.line <= self.target_line <= pos.end.line):
                return True
            self.target_start, self.target_end = pos.start, pos.end
            return True

        @override
        def leave_Call(self, original_node, updated_node):
            pos = ModuleOutline.span(self.get_metadata(PositionProvider, original_node))
            if (self.target_start, self.target_end) != (pos.start, pos.end) or not updated_node.args:
                return updated_node
            # rebuild the positional args with keywords, in param order. Once the
            # first literal is keyworded every LATER positional arg must keyword
            # too — a positional after a keyword is a syntax error (the old loop
            # skipped non-literals and could emit `f(x=1, Pair(2, 3))`).
            positional = [i for i, a in enumerate(updated_node.args) if a.keyword is None]
            first_kw = next(
                (
                    i
                    for i in positional
                    if m.matches(
                        updated_node.args[i].value,
                        m.Integer() | m.Float() | m.SimpleString() | m.ConcatenatedString(),
                    )
                    and i < len(self.params)
                ),
                None,
            )
            if first_kw is None:
                return updated_node
            if any(i >= len(self.params) for i in positional if i >= first_kw):
                return updated_node  # out of params past the first keyword
            new_args = []
            for i, arg in enumerate(updated_node.args):
                if MechanicalFixes._arg_stays_positional(arg, i, positional, first_kw):
                    new_args.append(arg)
                    continue
                self.applied = True
                new_args.append(
                    arg.with_changes(
                        keyword=cst.Name(self.params[i]),
                        equal=cst.AssignEqual(
                            whitespace_before=cst.SimpleWhitespace(""),
                            whitespace_after=cst.SimpleWhitespace(""),
                        ),
                    )
                )
            return updated_node.with_changes(args=new_args)

    class _LoopHoistExtractor(cst.CSTTransformer):
        """loop-hoist (mechanical): a single-accumulator for-loop whose body does
        real work becomes a per-item helper + a flattening comprehension. The
        body moves into the helper UNCHANGED except the accumulator's mutation
        receiver is renamed to a fresh local the helper returns:

            def _per_item(item):          # the moved body, acc -> val
                ...                        #   with every `acc` renamed to `val`
                return val
            acc = [x for item in items for x in _per_item(item)]

        The rewrite is lossless when the body's ONLY outer state writes are
        append/add on ONE empty-initialized accumulator and it never READS that
        accumulator mid-build (a comprehension sees no partial state). Everything
        else declines with a reason — not a silent no-op."""

        METADATA_DEPENDENCIES = (PositionProvider,)

        def __init__(self, line: int, name: str, helper_local: str = "val", element: str = "part") -> None:
            self.line = line
            if Rewrite.extraction_is_private() and not name.startswith("_"):
                name = "_" + name
            self.name = name
            self.helper_local = helper_local
            self.element = element
            self.applied = False
            self.decline = ""

        @override
        def leave_IndentedBlock(self, original_node, updated_node):
            plan = self._block_plan(original_node)
            if plan is None:
                return updated_node
            init_idx, loop_idx, helper_def, new_stmt = plan
            new_body = list(updated_node.body)
            new_body[init_idx : loop_idx + 1] = [helper_def, new_stmt]
            self.applied = True
            return updated_node.with_changes(body=new_body)

        def _block_plan(self, block):
            stmts = list(block.body)
            loop_idx = MechanicalFixes._for_index_at_line(self, stmts, self.line)
            if loop_idx is None:
                return None
            loop = stmts[loop_idx]
            if loop.orelse:
                self.decline = "the hoist rewrite is for for-loops without an else"
                return None
            if not isinstance(loop.target, cst.Name):
                self.decline = "the loop target must be a simple name to become the helper's parameter"
                return None
            if loop.asynchronous is not None:
                self.decline = "an async for-loop cannot become a comprehension (comprehensions are synchronous)"
                return None
            init_idx = loop_idx - 1
            while init_idx >= 0 and isinstance(stmts[init_idx], cst.EmptyLine):
                init_idx -= 1
            if init_idx < 0:
                self.decline = (
                    "the accumulator must be initialized to an empty []/set() directly before "
                    "the loop — the hoist cannot preserve prior contents"
                )
                return None
            init = stmts[init_idx]
            init_small = MechanicalFixes._small_stmt(init)
            plan = self._hoist_plan(list(loop.body.body), loop.target.value)
            if plan is None:
                return None
            acc, collection = plan
            if acc is None or collection is None:
                return None  # unreachable (the plan declines, never returns Nones) — the checker needs the guard
            init_value = init_small.value if isinstance(init_small, (cst.Assign, cst.AnnAssign)) else None
            if not MechanicalFixes._empty_init(init_value, collection):
                self.decline = (
                    f"the accumulator `{acc}` must be initialized to an empty "
                    f"{'[]' if collection == 'list' else 'set()'}"
                    f" directly before the loop — the hoist cannot preserve prior contents"
                )
                return None
            rewriter = MechanicalFixes._NameRewriter(acc, self.helper_local)
            moved = [s.visit(rewriter) for s in loop.body.body]
            moved[0] = moved[0].with_changes(leading_lines=[])
            ret = cst.SimpleStatementLine(body=[cst.Return(value=cst.Name(self.helper_local))])
            helper_def = cst.FunctionDef(
                name=cst.Name(self.name),
                params=cst.Parameters(params=[cst.Param(cst.Name(loop.target.value))]),
                body=cst.IndentedBlock(body=moved + [ret]),
            )
            # outer clause FIRST: [x for it in items for x in _h(it)]
            inner = cst.CompFor(
                target=cst.Name(self.element),
                iter=cst.Call(cst.Name(self.name), args=[cst.Arg(cst.Name(loop.target.value))]),
            )
            comp_for = cst.CompFor(
                target=cst.Name(loop.target.value),
                iter=loop.iter,
                inner_for_in=inner,
            )
            if collection == "set":
                comp: cst.BaseComp = cst.SetComp(elt=cst.Name(self.element), for_in=comp_for)
            else:
                comp = cst.ListComp(elt=cst.Name(self.element), for_in=comp_for)
            new_small = init_small.with_changes(value=comp)
            return init_idx, loop_idx, helper_def, init.with_changes(body=[new_small])

        def _hoist_plan(self, body_stmts: list[cst.BaseStatement], target_name: str):
            """(acc, collection) when the body is rewritable: every outer-state
            write is an append/add on ONE accumulator, never a read of it."""
            bound = MechanicalFixes._loop_body_bound(body_stmts)
            bound.add(target_name)
            acc: str | None = None
            collection: str | None = None
            receivers: set[int] = set()  # Name node ids that ARE the append/add receiver
            nodes = list(ModuleOutline.iter_nodes(body_stmts))
            for n in nodes:
                if not isinstance(n, cst.Call) or not isinstance(n.func, cst.Attribute):
                    continue
                base = n.func.value
                if not isinstance(base, cst.Name) or base.value in bound:
                    continue
                method = n.func.attr.value
                if method not in MechanicalFixes._HOIST_MUTATORS:
                    if method in MechanicalFixes._OTHER_MUTATORS:
                        self.decline = (
                            f"`{method}` on `{base.value}` — only append/add accumulation can be "
                            "hoisted into a helper; refactor by hand"
                        )
                        return None
                    continue  # a plain READ call on outer state is fine (the helper closes over it)
                if method not in ("append", "add"):
                    self.decline = (
                        f"`{method}` accumulation (`{base.value}`) keeps the loop — the hoist "
                        "rewrite only expresses append/add"
                    )
                    return None
                if acc is None:
                    acc = base.value
                    collection = "list" if method == "append" else "set"
                elif base.value != acc:
                    self.decline = (
                        f"the body accumulates BOTH `{acc}` and `{base.value}` — "
                        "the hoist rewrite needs one accumulator"
                    )
                    return None
                receivers.add(id(base))
            if acc is None:
                self.decline = "no outer collection mutation found in the loop body"
                return None
            for n in nodes:
                if MechanicalFixes._reads_accumulator_mid_build(n, acc, receivers):
                    self.decline = (
                        f"the body READS `{acc}` mid-build — a comprehension has no "
                        "partial state to read; refactor by hand"
                    )
                    return None
            for s in body_stmts:
                for n in ModuleOutline.iter_nodes_pruned([s]):
                    if isinstance(n, cst.AnnAssign):
                        target_list: list = [n.target]
                    elif isinstance(n, cst.Assign):
                        target_list = list(n.targets)
                    elif isinstance(n, cst.AugAssign):
                        target_list = [n.target]
                    else:
                        continue
                    for t in target_list:
                        for sub in ModuleOutline.iter_nodes(t):
                            if MechanicalFixes._writes_other_name(sub, bound, acc):
                                self.decline = (
                                    f"the body also writes `{sub.value}` — the hoist rewrite "
                                    "only moves a single-accumulator loop"
                                )
                                return None
            return acc, collection

    class _LoopsIntoComprehensions(cst.CSTTransformer):
        """Replace Loop with Pipeline (mechanical):
        - kind `loop-pipeline`: the single-mutation for-loop AT `line` becomes
          the comprehension that replaces it;
        - kind `loop-sequence`: the enclosing function's contiguous chain of
          pipeline loops sharing ONE empty-initialized accumulator becomes the
          concatenated/merged comprehensions.
        The accumulator must be initialized to an EMPTY collection literally
        directly before the first loop — the rewrite preserves only the loop,
        not prior contents. Unsupported or unsafe shapes set `decline`; a
        missing target stays silent (the stale-finding protocol)."""

        METADATA_DEPENDENCIES = (PositionProvider, ParentNodeProvider)

        def __init__(self, line: int, sequence: bool, fresh_name: str = "item") -> None:
            self.line = line
            self.sequence = sequence
            self.fresh = fresh_name
            self.applied = False
            self.decline = ""

        @override
        def leave_IndentedBlock(self, original_node, updated_node):
            plan = self._block_plan(original_node)
            if plan is None:
                return updated_node
            body, first_idx, loops, new_stmt = plan
            new_body = list(updated_node.body)
            new_body[first_idx : first_idx + loops] = [new_stmt]
            self.applied = True
            return updated_node.with_changes(body=new_body)

        def _block_plan(self, block):
            """(body, first_index, loop_count, replacement_statement) for the
            block that owns the target loop(s), or None when this block is not
            the one the finding points at."""
            stmts = list(block.body)
            if self.sequence:
                parent = self.get_metadata(ParentNodeProvider, block)
                if not isinstance(parent, cst.FunctionDef):
                    return None
                def_line = ModuleOutline.span(self.get_metadata(PositionProvider, parent)).start.line
                if def_line != self.line:
                    return None
                first = next((i for i, s in enumerate(stmts) if not isinstance(s, cst.EmptyLine)), None)
                if first is None or not isinstance(MechanicalFixes._small_stmt(stmts[first]), (cst.Assign, cst.AnnAssign)):
                    return None
                first_loop = next(
                    (i for i in range(first + 1, len(stmts)) if not isinstance(stmts[i], cst.EmptyLine)),
                    None,
                )
                if first_loop is None or not isinstance(stmts[first_loop], cst.For):
                    return None
                loop_count = self._chain_length(stmts, first_loop)
                if loop_count < 2:
                    self.decline = (
                        "the top of the function needs >=2 consecutive single-mutation loops on the "
                        "same accumulator, straight after its empty initialization — the chain "
                        "rewrite stops at the first incompatible statement"
                    )
                    return None
                return self._rewrite_plan(stmts, first_loop, loop_count)
            else:
                first_idx = MechanicalFixes._for_index_at_line(self, stmts, self.line)
                if first_idx is None:
                    return None
                loop_count = 1
            return self._rewrite_plan(stmts, first_idx, loop_count)

        @staticmethod
        def _chain_length(stmts, start: int) -> int:
            n = 0
            for s in stmts[start:]:
                if isinstance(s, cst.For):
                    n += 1
                    continue
                if isinstance(s, cst.EmptyLine):
                    continue
                break
            return n

        def _rewrite_plan(self, stmts, first_idx: int, loop_count: int):
            """The init + rewritable loops -> the replacement statement. Declines
            (self.decline) when the chain cannot be rewritten losslessly."""
            loops = [s for s in stmts[first_idx : first_idx + loop_count] if isinstance(s, cst.For)]
            if len(loops) != loop_count:
                self.decline = (
                    "the chain mixes loops and other statements — reorder so the loops are "
                    "consecutive, or fix them one at a time"
                )
                return None
            init_idx = first_idx - 1
            while init_idx >= 0 and isinstance(stmts[init_idx], cst.EmptyLine):
                init_idx -= 1
            if init_idx < 0:
                self.decline = (
                    "the accumulator must be initialized to an empty []/{} (or set()) directly "
                    "before the loop — the comprehension rewrite cannot preserve prior contents"
                )
                return None
            init = stmts[init_idx]
            init_small = MechanicalFixes._small_stmt(init)
            init_value = init_small.value if isinstance(init_small, (cst.Assign, cst.AnnAssign)) else None
            if isinstance(loops[0].asynchronous, cst.Asynchronous):
                self.decline = "an async for-loop cannot become a comprehension (comprehensions are synchronous)"
                return None
            raw_plans: list[MechanicalFixes._LoopPipelinePlan | None] = [self._loop_plan(f) for f in loops]
            if any(p is None for p in raw_plans):
                self.decline = (
                    "a loop in the chain is not a single collection mutation (append/add/extend/"
                    "`+= [..]`/subscript-store) — a comprehension cannot express it; refactor by hand"
                )
                return None
            plans: list[MechanicalFixes._LoopPipelinePlan] = []
            for p in raw_plans:
                if p is not None:
                    plans.append(p)
            acc = plans[0].acc
            collection = plans[0].collection
            if any(p.acc != acc or p.collection != collection for p in plans):
                self.decline = (
                    f"the chain mixes accumulators or collection kinds — the rewrite needs every "
                    f"loop to build the SAME empty-initialized collection ({acc})"
                )
                return None
            if not MechanicalFixes._empty_init(init_value, collection):
                self.decline = (
                    f"the accumulator `{acc}` must be initialized to an empty "
                    f"{'[]' if collection == 'list' else '{}' if collection == 'dict' else 'set()'}"
                    f" directly before the loop — the comprehension rewrite cannot preserve prior contents"
                )
                return None
            comps = [self._comprehension(f, p) for f, p in zip(loops, plans, strict=True)]
            value = comps[0]
            for comp in comps[1:]:
                op = cst.Add() if collection == "list" else cst.BitOr()
                value = cst.BinaryOperation(left=value, operator=op, right=comp)
            new_small = init_small.with_changes(value=value)
            new_stmt = init.with_changes(body=[new_small])
            return stmts, init_idx, (first_idx + loop_count) - init_idx, new_stmt

        @staticmethod
        def _loop_plan(for_node: cst.For) -> MechanicalFixes._LoopPipelinePlan | None:
            if for_node.orelse:
                return None
            body = list(for_node.body.body)
            stmt = body[0]
            if_test = None
            if isinstance(stmt, cst.If):
                if stmt.orelse:
                    return None
                if_body = list(stmt.body.body)
                if len(if_body) != 1:
                    return None
                if_test = stmt.test
                stmt = if_body[0]
            plan = MechanicalFixes._mutation_plan(stmt)
            if plan is None:
                return None
            plan.if_test = if_test
            return plan

        def _comprehension(self, for_node: cst.For, plan: MechanicalFixes._LoopPipelinePlan):
            comp_for = cst.CompFor(
                target=for_node.target,
                iter=for_node.iter,
                ifs=[cst.CompIf(test=plan.if_test)] if plan.if_test is not None else [],
            )
            if plan.flatten is not None:
                inner = cst.CompFor(target=cst.Name(self.fresh), iter=plan.flatten, inner_for_in=comp_for)
                return cst.ListComp(elt=cst.Name(self.fresh), for_in=inner)
            if plan.collection == "set":
                if plan.element is None:
                    return None
                return cst.SetComp(elt=plan.element, for_in=comp_for)
            if plan.collection == "dict":
                if plan.element is None or plan.key is None:
                    return None
                return cst.DictComp(key=plan.key, value=plan.element, for_in=comp_for)
            if plan.element is None:
                return None
            return cst.ListComp(elt=plan.element, for_in=comp_for)

    class _NameCount(cst.CSTVisitor):
        def __init__(self, name: str) -> None:
            self.name: str = name
            self.n: int = 0

        @override
        def visit_Name(self, node) -> None:
            if node.value == self.name:
                self.n += 1

    class _NameRewriter(cst.CSTTransformer):
        """Rename every occurrence of one name — the hoist re-targets the
        accumulator's mutation receiver into the helper's fresh local."""

        def __init__(self, old: str, new: str) -> None:
            self.old = old
            self.new = new

        @override
        def leave_Name(self, original_node, updated_node):
            return Rewrite.renamed(updated_node, self.old, self.new)

    class _StripDocstring(cst.CSTTransformer):
        """Delete the first body statement of the def at `line` when it is a
        string-literal expression — the restating docstring (the rule proved it
        adds nothing beyond the body tokens)."""

        METADATA_DEPENDENCIES = (PositionProvider,)

        def __init__(self, line: int) -> None:
            self.line: int = line
            self.done: bool = False

        @override
        def leave_FunctionDef(self, original_node, updated_node):
            if self.done:
                return updated_node
            if ModuleOutline.span(self.get_metadata(PositionProvider, original_node)).start.line == self.line:
                self.done = True
                return MechanicalFixes._strip_first_docstring(updated_node, original_node)
            return updated_node

        # the class body can carry the docstring too — libcst dispatches the two
        # definition kinds through separate hooks, so the body repeats
        @override
        def leave_ClassDef(self, original_node, updated_node):
            if self.done:
                return updated_node
            if ModuleOutline.span(self.get_metadata(PositionProvider, original_node)).start.line == self.line:
                self.done = True
                return MechanicalFixes._strip_first_docstring(updated_node, original_node)
            return updated_node

    @staticmethod
    def _arg_stays_positional(arg: cst.Arg, index: int, positional: list[int], first_kw: int) -> bool:
        """The argument is not one of the trailing positional targets the rewrite
        moves: a keyword, outside the target set, or before the first keyword."""
        return arg.keyword is not None or index not in positional or index < first_kw

    @staticmethod
    def _augassign_mutation_plan(node: cst.AugAssign) -> MechanicalFixes._LoopPipelinePlan | None:
        """`name += [a, b]` (list/tuple literal concatenation): one element is
        a direct element, several flatten through the literal."""
        aug = node
        if not MechanicalFixes._is_list_extend(aug):
            return None
        assert isinstance(aug.target, cst.Name) and isinstance(aug.value, (cst.List, cst.Tuple))
        elts = aug.value.elements
        if len(elts) == 1:
            return MechanicalFixes._LoopPipelinePlan(
                acc=aug.target.value, collection="list", element=elts[0].value, key=None, flatten=None, if_test=None
            )
        return MechanicalFixes._LoopPipelinePlan(
            acc=aug.target.value, collection="list", element=None, key=None, flatten=aug.value, if_test=None
        )

    @staticmethod
    def _bind_children(stmt, bound: set[str]) -> None:
        """The names a compound statement binds across its child blocks."""
        seqs: list = []
        if isinstance(stmt, cst.For):
            bound.update(Bindings.expr_names(stmt.target))
            seqs = MechanicalFixes._stmt_seq(stmt.body) + MechanicalFixes._stmt_seq(stmt.orelse)
        elif isinstance(stmt, (cst.While, cst.If)):
            seqs = MechanicalFixes._stmt_seq(stmt.body) + MechanicalFixes._stmt_seq(stmt.orelse)
        elif isinstance(stmt, cst.Try):
            seqs = (
                MechanicalFixes._stmt_seq(stmt.body)
                + MechanicalFixes._stmt_seq(stmt.orelse)
                + MechanicalFixes._stmt_seq(stmt.finalbody)
            )
            for h in stmt.handlers:
                seqs += MechanicalFixes._stmt_seq(h.body)
        elif isinstance(stmt, cst.With):
            for item in stmt.items:
                if item.asname is not None:
                    bound.update(Bindings.expr_names(item.asname.name))
            seqs = MechanicalFixes._stmt_seq(stmt.body)
        elif isinstance(stmt, cst.Match):
            for case in stmt.cases:
                seqs += MechanicalFixes._stmt_seq(case.body)
        for b in seqs:
            MechanicalFixes._bind_names(b, bound)

    @staticmethod
    def _bind_names(stmt, bound: set[str]) -> None:
        """The names ONE statement binds — control-flow descend, nested defs
        excluded. `counts[x] = v` is a WRITE to counts, not a binding."""
        if isinstance(stmt, cst.SimpleStatementLine):
            for small in stmt.body:
                MechanicalFixes._bind_small(small, bound)
            return
        MechanicalFixes._bind_children(stmt, bound)

    @staticmethod
    def _bind_small(small, bound: set[str]) -> None:
        """The names one small statement binds."""
        if isinstance(small, cst.AnnAssign):
            bound.update(Bindings.hoist_target_names(small.target))
        elif isinstance(small, cst.Assign):
            for t in small.targets:
                bound.update(Bindings.hoist_target_names(t))
        elif isinstance(small, cst.AugAssign):
            bound.update(Bindings.hoist_target_names(small.target))

    @staticmethod
    def _call_mutation_plan(node: cst.Expr) -> MechanicalFixes._LoopPipelinePlan | None:
        """A mutating receiver call: append/appendleft/add (direct element) or
        extend (flatten)."""
        value_node = node.value
        if not isinstance(value_node, cst.Call):
            return None
        func = value_node.func
        if not isinstance(func, cst.Attribute) or not isinstance(func.value, cst.Name):
            return None
        acc = func.value.value
        method = func.attr.value
        if len(value_node.args) != 1 or not isinstance(value_node.args[0], cst.Arg):
            return None
        value = value_node.args[0].value
        if method in ("append", "appendleft"):
            return MechanicalFixes._LoopPipelinePlan(
                acc=acc, collection="list", element=value, key=None, flatten=None, if_test=None
            )
        if method == "add":
            return MechanicalFixes._LoopPipelinePlan(
                acc=acc, collection="set", element=value, key=None, flatten=None, if_test=None
            )
        if method == "extend":
            return MechanicalFixes._LoopPipelinePlan(
                acc=acc, collection="list", element=None, key=None, flatten=value, if_test=None
            )
        return None

    @staticmethod
    def _flatten_stmts(stmts, out: list) -> None:
        """Source-ordered statement flatten: nested block bodies included, so a
        loop body repeated after the loop is one sequence (the Rust rule's
        mirror)."""
        for s in stmts:
            out.append(s)
            if isinstance(s, cst.If):
                MechanicalFixes._flatten_stmts(list(s.body.body), out)
                if s.orelse is not None:
                    # an elif chain nests the next If directly; a plain else wraps
                    # its suite in an Else — flatten either into its statements
                    nested = [s.orelse] if isinstance(s.orelse, cst.If) else list(s.orelse.body.body)
                    MechanicalFixes._flatten_stmts(nested, out)
            elif isinstance(s, (cst.For, cst.While)):
                MechanicalFixes._flatten_stmts(list(s.body.body), out)
                if s.orelse is not None:
                    MechanicalFixes._flatten_stmts(list(s.orelse.body.body), out)
            elif isinstance(s, cst.Try):
                MechanicalFixes._flatten_stmts(list(s.body.body), out)
                for h in s.handlers:
                    MechanicalFixes._flatten_stmts(list(h.body.body), out)
                if s.orelse is not None:
                    MechanicalFixes._flatten_stmts(list(s.orelse.body.body), out)
            elif isinstance(s, cst.With):
                MechanicalFixes._flatten_stmts(list(s.body.body), out)
            elif isinstance(s, cst.Match):
                for case in s.cases:
                    MechanicalFixes._flatten_stmts(list(case.body.body), out)

    @staticmethod
    def _small_stmt(stmt):
        """The small statement inside a statement line (blocks hold
        SimpleStatementLine wrappers) — or the statement itself when it is
        already compound/small."""
        if isinstance(stmt, cst.SimpleStatementLine) and len(stmt.body) == 1:
            return stmt.body[0]
        return stmt

    @staticmethod
    def _empty_init(value, collection: str) -> bool:
        """Is this accumulator initializer the empty collection the loop fills?"""
        if collection == "list":
            return isinstance(value, cst.List) and not value.elements
        if collection == "dict":
            return isinstance(value, cst.Dict) and not value.elements
        return MechanicalFixes._is_empty_set_call(value)

    @staticmethod
    def _for_index_at_line(metadata: cst.CSTTransformer, stmts: list[cst.BaseStatement], line: int) -> int | None:
        """The index of the `for` statement at the given source line."""
        return next(
            (
                i
                for i, s in enumerate(stmts)
                if isinstance(s, cst.For)
                and ModuleOutline.span(metadata.get_metadata(PositionProvider, s)).start.line == line
            ),
            None,
        )

    @staticmethod
    def _fresh_loop_name(source: str, base: str = "item") -> str:
        """A comprehension-flatten variable absent from the module — the
        extend / `+= [a, b]` rewrite introduces one and must not shadow an
        existing name."""
        names: set[str] = set()

        class _Names(cst.CSTVisitor):
            @override
            def visit_Name(self, node) -> None:
                names.add(node.value)

        try:
            cst.parse_module(source).visit(_Names())
        except cst.ParserSyntaxError:
            return base
        candidate = base
        i = 2
        while candidate in names:
            candidate = f"{base}_{i}"
            i += 1
        return candidate

    @staticmethod
    def _infer_member_type(value: cst.BaseExpression, param_types: dict[str, str]) -> str | None:
        """The annotation for a member's initial value: a literal's type, a
        param's annotation, a cst.X(...) construction's class; None when the
        type cannot be named safely (leave the member unannotated)."""
        if isinstance(value, cst.Name):
            return param_types.get(value.value)
        if isinstance(value, cst.Integer):
            return "int"
        if isinstance(value, cst.Float):
            return "float"
        if isinstance(value, (cst.SimpleString, cst.ConcatenatedString)):
            return "str"
        if isinstance(value, cst.List):
            return "list"
        if isinstance(value, cst.Dict):
            return "dict"
        if isinstance(value, cst.Set):
            return "set"
        if isinstance(value, cst.Tuple):
            return "tuple"
        if isinstance(value, cst.Call) and isinstance(value.func, cst.Name):
            return value.func.value  # cst.FunctionDef(...) -> FunctionDef
        return None

    @staticmethod
    def _is_empty_set_call(value: cst.BaseExpression) -> bool:
        """`set()` with no arguments — the empty-set literal's fix target."""
        return (
            isinstance(value, cst.Call)
            and isinstance(value.func, cst.Name)
            and value.func.value == "set"
            and not value.args
        )

    @staticmethod
    def _is_list_extend(aug: cst.AugAssign) -> bool:
        """The `acc += [x]` shape: a name extended by a list or tuple literal."""
        return (
            isinstance(aug.target, cst.Name)
            and isinstance(aug.operator, cst.AddAssign)
            and isinstance(aug.value, (cst.List, cst.Tuple))
        )

    @staticmethod
    def _loop_body_bound(stmts: list[cst.BaseStatement]) -> set[str]:
        """The names BOUND inside a loop body (temps) — the temps the hoist
        rewrite may keep inside the helper, as opposed to outer state writes."""
        bound: set[str] = set()
        for s in stmts:
            MechanicalFixes._bind_names(s, bound)
        return bound

    @staticmethod
    def _mutation_plan(stmt: cst.BaseStatement | cst.BaseSmallStatement) -> MechanicalFixes._LoopPipelinePlan | None:
        """The single collection mutation of a pipeline body statement, as the
        plan the comprehension rewrite needs. None when the statement is not
        one the rewrite expresses."""
        if not isinstance(stmt, cst.SimpleStatementLine):
            return None
        body = stmt.body
        if len(body) != 1:
            return None
        node = body[0]
        if isinstance(node, cst.Expr) and isinstance(node.value, cst.Call):
            return MechanicalFixes._call_mutation_plan(node)
        if isinstance(node, cst.AugAssign):
            return MechanicalFixes._augassign_mutation_plan(node)
        if isinstance(node, cst.Assign):
            return MechanicalFixes._subscript_mutation_plan(node)
        return None

    @staticmethod
    def _name_occurrences(repo: Path, name: str) -> int:
        """Name-node occurrences of `name` across the repo's .py files — the
        duplicate-def safety check: a delete is only offered when the shadowing
        def is provably unreferenced (the two def sites are the only hits)."""
        total = 0
        for p in SourceFiles(repo).py_files():
            try:
                module = cst.parse_module(p.read_text(encoding="utf-8"))
            except (OSError, UnicodeDecodeError, cst.ParserSyntaxError):
                continue  # an unreadable/unparsable sibling is its own finding

            probe = MechanicalFixes._NameCount(name)
            module.visit(probe)
            total += probe.n
        return total

    @staticmethod
    def _params_of_any_def(source: str, callee: str) -> list[str] | None:
        """Parameter names of a module-level `def callee(` or `class callee:`
        `__init__` in one module — self/cls dropped."""
        try:
            module = cst.parse_module(source)
        except cst.ParserSyntaxError:
            return None
        found = None

        class _Find(cst.CSTVisitor):
            @override
            def visit_FunctionDef(self, node) -> None:
                nonlocal found
                if node.name.value == callee and found is None:
                    found = [p.name.value for p in node.params.params if p.name is not None]

            @override
            def visit_ClassDef(self, node) -> None:
                nonlocal found
                if node.name.value == callee and found is None:
                    for stmt in node.body.body:
                        if isinstance(stmt, cst.FunctionDef) and stmt.name.value == "__init__":
                            found = [p.name.value for p in stmt.params.params if p.name is not None]

        module.visit(_Find())
        if found is None:
            return None
        if found and found[0] in ("self", "cls"):
            found = found[1:]
        return found or None

    @staticmethod
    def _reads_accumulator_mid_build(node: cst.BaseExpression, acc: str, receivers: set[int]) -> bool:
        """The body reads the accumulator while still building it — a comprehension
        has no partial state to read."""
        return isinstance(node, cst.Name) and node.value == acc and id(node) not in receivers

    @staticmethod
    def _stmt_seq(x) -> list:
        """A statement suite as a list: an IndentedBlock, an `else` clause's
        block, or a plain list (None is empty)."""
        if isinstance(x, cst.IndentedBlock):
            return list(x.body)
        if isinstance(x, (cst.Else, cst.Finally)):
            return list(x.body.body)
        return list(x or [])

    @staticmethod
    def _strip_first_docstring(
        updated: cst.FunctionDef | cst.ClassDef, original: object
    ) -> cst.FunctionDef | cst.ClassDef:
        if not isinstance(updated.body, cst.IndentedBlock):
            return updated  # a one-line `def f(): ...` body has no docstring statement
        body = updated.body.body
        first = body[0] if body else None
        if isinstance(first, cst.SimpleStatementLine) and len(first.body) == 1:
            stmt = first.body[0]
            if isinstance(stmt, cst.Expr) and isinstance(stmt.value, cst.SimpleString):
                return updated.with_changes(body=updated.body.with_changes(body=list(body[1:])))
        return updated

    @staticmethod
    def _subscript_mutation_plan(node: cst.Assign) -> MechanicalFixes._LoopPipelinePlan | None:
        """`acc[key] = value` — a dict comprehension."""
        if len(node.targets) != 1:
            return None
        target = node.targets[0].target
        if not isinstance(target, cst.Subscript) or not isinstance(target.value, cst.Name):
            return None
        # this libcst models the slice as SubscriptElement(s) — a plain index
        # unwraps to its value; anything else is not a dict key
        sl = target.slice
        if not isinstance(sl, tuple) or len(sl) != 1:
            return None
        inner = sl[0].slice
        if not isinstance(inner, cst.Index) or inner.value is None:
            return None
        return MechanicalFixes._LoopPipelinePlan(
            acc=target.value.value, collection="dict", element=node.value, key=inner.value, flatten=None, if_test=None
        )

    @staticmethod
    def _writes_other_name(node: cst.BaseExpression, bound: set[str], acc: str) -> bool:
        """The body writes a name other than the accumulator — the hoist rewrite
        moves a single-accumulator loop only."""
        return isinstance(node, cst.Name) and node.value not in bound and node.value != acc

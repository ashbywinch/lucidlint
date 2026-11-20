"""Extracting a method's best self-contained seam into a new function.

The seam is bounded to the decision budget so the extracted function lands
under the complexity gate — extraction SPLITS complexity, it does not move
it. The name is normalized: the extracted function is private by
construction, so a public-looking name is underscored.
"""

from __future__ import annotations

from typing import override

import libcst as cst
from libcst.metadata import ExpressionContextProvider, ParentNodeProvider, PositionProvider

from fix_engine.bindings import Bindings
from fix_engine.module_outline import ModuleOutline
from fix_engine.rewrite import Rewrite


class MethodExtraction:
    """One request's extract-method fix: the seam analysis, the proposal, and
    the apply. The request is the fixer's state."""

    # the complexity gate: 15 decision points is the fail bar, so 14 decisions
    # remain the most the original may keep above the extraction
    _CC_GATE_DECISIONS = 14

    def __init__(self, request) -> None:
        self.request = request

    def _fn_seam_analysis(self):
        """Analyze the function at `line`: its body statements with per-statement
        spans, first-use contexts, writes, and control-flow flags."""
        source, line = self.request.loaded_source(), self.request.line
        module = cst.parse_module(source)
        wrapper = cst.MetadataWrapper(module)
        state = MethodExtraction._FnBodyState(line)
        wrapper.visit(MethodExtraction._Analyse(state))
        state.module_globals = MethodExtraction._module_level_names(module)
        state.fn_writes = set().union(*(w for w in state.writes.values())) if state.writes else set()
        return module, wrapper, state

    def extract_method_proposal(self):
        """Compute the best extraction seam and the resulting source, WITHOUT
        writing. Returns (new_source, seam_text) or (None, None) when no safe
        seam exists. `name` may be None — the preview then shows a placeholder
        (`_extracted`) the agent replaces after reviewing the seam; a supplied
        name is normalized to the private convention (the extracted function
        cannot have external callers, so it takes the underscore)."""
        source, line = self.request.loaded_source(), self.request.line
        name = self.request.opts.name
        # the _FixRequest refactor dropped the caller-supplied 13 — the
        # bound is the contract (an extracted helper must not still be
        # complex); restore it as the default (review bot)
        max_decisions = self.request.max_decisions if self.request.max_decisions is not None else 13
        if name is None:
            name = "_extracted"
        elif Rewrite.extraction_is_private() and not name.startswith("_"):
            name = "_" + name
        module, wrapper, state = self._fn_seam_analysis()
        if state.fn_node is None or len(state.flat) < 2:
            return None, None
        # a nested-function target cannot host the extracted helper — the insert
        # lands at module/class level, so the rewritten call would NameError
        # (refuse rather than write a broken file)
        if MethodExtraction._is_nested_target(wrapper, state.fn_node):
            return None, None
        seam = state.best_seam(
            max_window_decisions=max_decisions,
            min_window_decisions=MethodExtraction._min_seam_decisions(state),
        )
        if seam is None:
            return None, None
        block_sids, free_vars = seam
        first_span = state.stmt_spans[block_sids[0]]
        flat_entry = next(e for e in state.flat if e[0] == block_sids[0])
        container_sid, insert_index = flat_entry[1], flat_entry[2]
        body_stmts = [state.nodes[sid] for sid in block_sids]
        if body_stmts:
            # the first moved statement carries its original leading blank
            # lines (it was separated from the previous statement in the
            # source) — that would render as an empty first body line
            body_stmts[0] = body_stmts[0].with_changes(leading_lines=[])
        new_def = cst.FunctionDef(
            name=cst.Name(name),
            params=cst.Parameters(params=[cst.Param(cst.Name(v)) for v in free_vars]),
            body=cst.IndentedBlock(body=body_stmts),
            returns=None,
        )
        replaced = wrapper.visit(
            MethodExtraction._ExtractMethodRewrite(set(block_sids), (container_sid, insert_index), name, free_vars)
        ).code
        inserted = (
            cst.MetadataWrapper(cst.parse_module(replaced))
            .visit(MethodExtraction._InsertExtractedFn(state.fn_node.name.value, line, new_def))
            .code
        )
        seam_text = source.splitlines()[first_span[0] - 1] if source else ""
        return inserted, f"line {first_span[0]}: {seam_text.strip()}"

    def fix_extract_method(self) -> str | None:
        """Extract Function (applied): the best self-contained seam of the
        function at `line` becomes a new function named `name`. The seam is
        bounded to <= 13 decisions so the extracted function lands under the
        CC-15 gate — extraction SPLITS complexity, it does not move it. The
        name is normalized: the extracted function is private by construction
        (see _extraction_is_private), so a public-looking name is underscored."""
        new_source, _ = self.extract_method_proposal()
        return new_source

    class _Analyse(cst.CSTVisitor):
        """Collects the target function's body statement data — flattened so
        seams can descend into compound statements (a big loop's inner chunks
        become extractable, not just the whole loop). Module-level so the
        latent-class rule does not fire on the enclosing analysis function."""

        METADATA_DEPENDENCIES = (PositionProvider, ExpressionContextProvider, ParentNodeProvider)

        def __init__(self, state: MethodExtraction._FnBodyState) -> None:
            self.state: MethodExtraction._FnBodyState = state

        @override
        def visit_FunctionDef(self, node) -> None:
            if (
                self.state.fn_node is not None
                or ModuleOutline.span(self.get_metadata(PositionProvider, node)).start.line != self.state.line
            ):
                return
            self.state.fn_node = node
            self._flatten(node)

        def _flatten(self, container) -> None:
            """Collect the IndentedBlock-bodied statements under `container`,
            recursing into compounds (If/For/While/Try — their nested bodies
            carry IndentedBlocks). Nested defs/classes are scopes — not
            descended into."""
            suite = getattr(container, "body", None)
            if not isinstance(suite, cst.IndentedBlock):
                return
            children = []
            for idx, s in enumerate(suite.body):
                sid = id(s)
                self.state.flat.append((sid, id(container), idx))
                self.state.nodes[sid] = s
                p = ModuleOutline.span(self.get_metadata(PositionProvider, s))
                self.state.stmt_spans[sid] = (p.start.line, p.end.line)
                self.state.first_use[sid] = {}
                self.state.writes[sid] = set()
                self.state.control_flow[sid] = self._subtree_control_flow(s)
                self.state.decisions[sid] = MethodExtraction._stmt_decision_count(s)
                children.append(sid)
                if isinstance(s, (cst.If, cst.For, cst.While, cst.Try)):
                    self._flatten(s)
            self.state.container_sids[id(container)] = children

        def _subtree_control_flow(self, stmt) -> bool:
            """Would moving this statement alone change control flow? A
            `return`/`yield` inside always escapes the extracted fn (the call
            would return instead of continuing). A `break`/`continue` is only
            safe when its enclosing loop MOVES ALONG (lives inside the
            statement's subtree) — else it would escape to nothing. Nested
            defs are their own scopes: skipped."""
            nodes: list = []

            def collect(node) -> None:
                nodes.append(node)
                if isinstance(node, cst.FunctionDef):
                    return  # a nested fn's control flow is its own
                for child in node.children:
                    collect(child)

            collect(stmt)
            loops = {id(n) for n in nodes if isinstance(n, (cst.For, cst.While))}
            for n in nodes:
                if isinstance(n, (cst.Return, cst.Yield)):
                    return True
                if isinstance(n, (cst.Break, cst.Continue)):
                    p = ModuleOutline.resolve_node(self.get_metadata(ParentNodeProvider, n))
                    ok = False
                    while p is not None and not isinstance(p, cst.Module):
                        if isinstance(p, (cst.For, cst.While)):
                            ok = id(p) in loops
                            break
                        p = ModuleOutline.resolve_node(self.get_metadata(ParentNodeProvider, p))
                    if not ok:
                        return True
            return False

        @override
        def visit_Name(self, node) -> None:
            if self.state.fn_node is None:
                return
            sid = None
            parent = ModuleOutline.resolve_node(self.get_metadata(ParentNodeProvider, node))
            if ModuleOutline.is_keyword_name(node, parent):
                return  # a keyword-argument name (f(a=1)) is not a variable ref
            while parent is not None and not isinstance(parent, cst.Module):
                if parent is self.state.fn_node:
                    break
                if id(parent) in self.state.nodes:
                    sid = id(parent)
                    break
                parent = ModuleOutline.resolve_node(self.get_metadata(ParentNodeProvider, parent))
            if sid is None:
                return  # the fn's own signature/name — not a body read
            try:
                ctx = self.get_metadata(ExpressionContextProvider, node)
            except KeyError:
                return  # attribute names (obj.append) are not variable refs
            parent = self.get_metadata(ParentNodeProvider, node)
            is_aug_target = isinstance(parent, cst.AugAssign)
            if node.value not in self.state.first_use[sid]:
                if ctx == cst.metadata.ExpressionContext.LOAD or is_aug_target:
                    self.state.first_use[sid][node.value] = "read"
                else:
                    self.state.first_use[sid][node.value] = "write"
            if ctx == cst.metadata.ExpressionContext.STORE:
                self.state.writes[sid].add(node.value)

    class _DecisionCount(cst.CSTVisitor):
        """Counts the radon-style decisions in one statement: ifs + elifs, and/or
        BooleanOperations, loops, asserts, match arms, comprehension clauses —
        the same families the scanner's complexity rule counts (extraction must
        SPLIT the CC, not move it)."""

        def __init__(self) -> None:
            self.n: int = 0
            self._boolop_depth: int = 0
            self._boolop_root: cst.BooleanOperation | None = None

        @override
        def visit_If(self, node) -> None:
            # libcst models elifs as nested Ifs in orelse — the descent counts
            # them; an else IndentedBlock adds nothing (radon: else is free);
            # the test's and/or is counted by visit_BooleanOperation below
            self.n += 1

        @override
        def visit_For(self, node) -> None:
            self.n += 1

        @override
        def visit_While(self, node) -> None:
            self.n += 1

        @override
        def visit_CompFor(self, node) -> None:
            # comprehension clauses: radon counts the for + each if filter
            self.n += 1 + len(node.ifs)

        @override
        def visit_Match(self, node) -> None:
            self.n += sum(1 for c in node.cases if not MethodExtraction._wildcard(c.pattern))

        @override
        def visit_BooleanOperation(self, node) -> None:
            # libcst's and/or node (the old visit_BoolOp never fired). radon
            # counts one decision per `and`/`or` OPERATOR in a BoolOp tree —
            # _chain_operands counts the full left-nested chain. A chain may be
            # parenthesized into several roots overlapping one operand
            # (`(a and b) and (c and d)` is ONE radon BoolOp with 4 values):
            # count each OUTERMOST and/or node once, so a nested chain inside a
            # parenthesized operand is not double-counted. Depth-tracking
            # distinguishes the roots from the nested nodes (a single shared
            # flag cannot: the inner node's leave resets it for the outer).
            if isinstance(node.operator, (cst.And, cst.Or)):
                if self._boolop_depth == 0:
                    self.n += MethodExtraction._chain_operands(node) - 1
                self._boolop_depth += 1

        @override
        def leave_BooleanOperation(self, original_node) -> None:
            self._boolop_depth = max(0, self._boolop_depth - 1)

        @override
        def visit_Assert(self, node) -> None:
            self.n += 1

        @override
        def visit_IfExp(self, node) -> None:
            self.n += 1

        @override
        def visit_ExceptHandler(self, node) -> None:
            self.n += 1  # radon counts each except handler

    class _ExtractMethodRewrite(cst.CSTTransformer):
        """Replace the target block with a call to the new function. Works at any
        nesting depth: removed statements vanish via `on_leave`, and the call is
        inserted into their container's body at the first removed statement's
        index — so a seam inside a loop becomes a call inside the loop."""

        def __init__(self, block_sids: set[int], insertion, new_name: str, free_vars: list[str]) -> None:
            self.block_sids: set[int] = block_sids
            self.container_sid, self.insert_index = insertion
            self.new_name: str = new_name
            self.free_vars: list[str] = free_vars

        def on_leave(self, original_node, updated_node):
            if id(original_node) in self.block_sids:
                return cst.RemoveFromParent()
            if id(original_node) == self.container_sid:
                suite = getattr(updated_node, "body", None)
                if isinstance(suite, cst.IndentedBlock):
                    body = list(suite.body)
                    call = cst.SimpleStatementLine(
                        body=[
                            cst.Expr(
                                cst.Call(
                                    func=cst.Name(self.new_name),
                                    args=[cst.Arg(cst.Name(v)) for v in self.free_vars],
                                )
                            )
                        ]
                    )
                    idx = min(self.insert_index, len(body))
                    body.insert(idx, call)
                    return updated_node.with_changes(body=suite.with_changes(body=body))
            return updated_node

    class _FnBodyState:
        """The extract-method analysis state: the target function's body
        statements with per-statement spans, first-use contexts, writes, and
        control-flow flags."""

        def __init__(self, line: int) -> None:
            self.line: int = line
            self.fn_node: cst.FunctionDef | None = None
            # flattened statement list: (sid, container_sid, index_in_container)
            self.flat: list[tuple[int, int, int]] = []
            self.nodes: dict[int, cst.BaseStatement] = {}  # sid -> statement node
            self.container_sids: dict[int, list[int]] = {}  # container -> body sids
            self.stmt_spans: dict[int, tuple[int, int]] = {}
            self.first_use: dict[int, dict[str, str]] = {}
            self.writes: dict[int, set[str]] = {}
            self.control_flow: dict[int, bool] = {}
            self.decisions: dict[int, int] = {}
            # module-scope names — the extracted fn sits at module level too, so
            # imports/constants/defs are ambient, not parameters
            self.module_globals: set[str] = set()
            # names the fn assigns anywhere — a local shadow, so the name IS a
            # parameter even when a module binding exists
            self.fn_writes: set[str] = set()

        def _window_score(self, i: int, j: int, min_lines: int):
            """Score one candidate window over the FLAT statement list: a seam
            must stay within ONE container (a window cannot span the loop body
            and the fn body). Returns (free_count, -span, start) when safe, or
            None. Free = names whose first use in the window is a read
            (builtins excluded); out-variables and control-flow exits
            disqualify; the whole function is not a seam."""
            if i == 0 and j == len(self.flat) - 1:
                return None  # the whole flattened body is not a refactoring
            container = self.flat[i][1]
            same_container = all(self.flat[k][1] == container for k in range(i, j + 1))
            if not same_container:
                return None
            if any(self.control_flow[self.flat[k][0]] for k in range(i, j + 1)):
                return None  # a return/break inside the seam changes control flow
            span = self.stmt_spans[self.flat[j][0]][1] - self.stmt_spans[self.flat[i][0]][0] + 1
            if span < min_lines:
                return None
            free: set[str] = set()
            seen: set[str] = set()
            writes_all: set[str] = set()
            # the window's statements PLUS their nested subtrees — a statement's
            # body (the if's, the loop's) belongs to it for free/out-var math
            window_sids = self._window_sids(i, j)
            for sid in window_sids:
                for name, ctx in self.first_use[sid].items():
                    if name not in seen:
                        seen.add(name)
                        if ctx == "read":
                            free.add(name)
                writes_all |= self.writes[sid]
            # builtins and true module globals (not shadowed by a fn-local) are
            # ambient in the extracted fn; writes to them are not out-variables
            shadowed = {n for n in self.module_globals if n in self.fn_writes}
            ambient = Bindings.BUILTINS | (self.module_globals - shadowed)
            free -= ambient
            writes_all -= ambient
            if self._window_has_outvars(i, j, writes_all) or not free:
                return None  # out-variable or no-input seam — skip
            return len(free), -span, i, sorted(free)

        def _subtree_sids(self, sid: int) -> list[int]:
            """The statement's subtree ids in SOURCE ORDER — first-use analysis
            must see a write before a later read or it fabricates free variables
            (the phantom-param bug)."""
            out = [sid]
            for child in self.container_sids.get(sid, []):
                out.extend(self._subtree_sids(child))
            return out

        def _window_sids(self, i: int, j: int) -> list[int]:
            """The window's statement ids plus all descendant ids (a compound's
            body) in SOURCE ORDER, so the free/out-var math sees the whole
            subtree with writes before reads."""
            sids: list[int] = []
            for k in range(i, j + 1):
                sids.extend(self._subtree_sids(self.flat[k][0]))
            return sids

        def _window_has_outvars(self, i: int, j: int, writes_all: set[str]) -> bool:
            """Does any name written in the window get read after it — in the
            SEQUENTIAL sense? A read in a LATER statement of the window's own
            container is a real same-iteration dependency — the extracted helper
            cannot hand its locals back — EXCEPT for the container loop's OWN
            targets, which the loop statement re-binds each pass (a `for shape`
            target read after a window inside the body sees the loop's binding,
            not the window's). Only reads in LATER flat positions outside the
            window's container are iteration-scoped.
            (0c8bedc's blanket skip exempted EVERY same-container after-read; it
            let a block-local accumulator be extracted and dropped — issue #21.)"""
            container = self.flat[i][1]
            container_node = self.nodes.get(container)
            re_bound = MethodExtraction._for_target_names(container_node)
            window_subtree = set(self._window_sids(i, j))
            after: set[str] = set()
            for k in range(j + 1, len(self.flat)):
                sid = self.flat[k][0]
                if sid in window_subtree:
                    continue  # inside the window's own subtree, not sequential-after
                same_container = self.flat[k][1] == container
                for dsid in self._subtree_sids(sid):
                    for name, ctx in self.first_use[dsid].items():
                        if MethodExtraction._is_rebound_read(ctx, same_container, name, re_bound):
                            continue  # the loop re-binds it each pass — no dependency
                        after.add(name)
            return bool(writes_all & after)

        def best_seam(
            self,
            min_lines: int = 2,
            max_window_decisions: int | None = None,
            min_window_decisions: int = 0,
            max_free_vars: int = 6,
        ):
            """The window with the MOST decisions (real CC progress — extraction
            splits complexity, it does not move it) among those whose free
            variables fit the interface budget and whose out-variables are empty.
            A name is free iff its FIRST use in the window is a read. Ties go to
            fewer free vars, then the larger window. Returns (block_ids,
            free_vars) or None. Out-variables are a smell — the seam must be
            self-contained; the whole body is not a seam."""
            n = len(self.flat)
            best = None  # (score, flat indices)
            for i in range(n):
                for j in range(i, n):
                    score = self._window_score(i, j, min_lines)
                    if score is None:
                        continue
                    free_count, _, _, free_vars = score
                    if free_count > max_free_vars:
                        continue  # too-wide interface — not a cohesive seam
                    if max_window_decisions is not None:
                        window_decisions = sum(self.decisions[self.flat[k][0]] for k in range(i, j + 1))
                        if window_decisions > max_window_decisions:
                            continue  # the extracted fn would still be complex
                        if window_decisions < min_window_decisions:
                            continue  # the ORIGINAL would still be >= 15 — the
                            # seam must SPLIT enough for both sides to land clean
                        # decisions FIRST (CC progress), then interface width,
                        # then size — the descending order of the bound mode
                        score = (-window_decisions, *score)
                    if best is None or score < best[0]:
                        best = (score, list(range(i, j + 1)))
            if best is None:
                return None
            (*_, free_vars), flat_indices = best
            block_sids = [self.flat[k][0] for k in flat_indices]
            return list(block_sids), free_vars

    class _InsertExtractedFn(cst.CSTTransformer):
        """Insert the extracted function after its source function, in the same
        container (module or class body)."""

        METADATA_DEPENDENCIES = (ParentNodeProvider,)

        def __init__(self, fn_name: str, fn_line: int, new_def: cst.FunctionDef) -> None:
            self.fn_name: str = fn_name
            self.fn_line: int = fn_line
            self.new_def: cst.FunctionDef = new_def
            self.done: bool = False

        def _is_target_fn(self, stmt: cst.BaseStatement) -> bool:
            """The source function the extracted def is inserted after."""
            return isinstance(stmt, cst.FunctionDef) and stmt.name.value == self.fn_name

        def _maybe_insert(self, body: list, blank_lines: int) -> list:
            if self.done:
                return body
            out = []
            for stmt in body:
                out.append(stmt)
                if self._is_target_fn(stmt):
                    out.append(self.new_def.with_changes(leading_lines=[cst.EmptyLine()] * blank_lines))
                    self.done = True
            return out

        @override
        def leave_Module(self, original_node, updated_node):
            return updated_node.with_changes(body=self._maybe_insert(list(updated_node.body), 2))

        @override
        def leave_ClassDef(self, original_node, updated_node):
            return updated_node.with_changes(
                body=updated_node.body.with_changes(body=self._maybe_insert(list(updated_node.body.body), 1))
            )

    @staticmethod
    def _chain_operands(node) -> int:
        """The number of operands in a chained and/or tree — radon's
        `len(values)` for a BoolOp; a non-BoolOp leaf is one operand."""
        if isinstance(node, cst.BooleanOperation) and isinstance(node.operator, (cst.And, cst.Or)):
            return MethodExtraction._chain_operands(node.left) + MethodExtraction._chain_operands(node.right)
        return 1

    @staticmethod
    def _for_target_names(node) -> set[str]:
        """The names a for-loop's target binds (single name, tuple/starred
        destructuring) — the ONLY names the loop statement re-binds each pass.
        A container that is not a for-loop binds nothing here."""
        if not isinstance(node, cst.For):
            return set()
        return MethodExtraction._target_bound_names(node.target)

    @staticmethod
    def _is_nested_target(wrapper: cst.MetadataWrapper, fn_node) -> bool:
        """Is the target function nested inside another function? The extracted
        helper is inserted at module/class level — a nested target would leave
        the call undefined (refuse rather than write a broken file)."""
        parent_resolver = wrapper.resolve(ParentNodeProvider)
        parent = parent_resolver[fn_node]
        while parent is not None and not isinstance(parent, (cst.Module, cst.ClassDef, cst.FunctionDef)):
            parent = parent_resolver[parent]
        return isinstance(parent, cst.FunctionDef)

    @staticmethod
    def _is_rebound_read(ctx: str, same_container: bool, name: str, re_bound: set[str]) -> bool:
        """A read of a name the loop re-binds each pass — not a dependency on the
        code after the loop."""
        return ctx == "read" and same_container and name in re_bound

    @staticmethod
    def _min_seam_decisions(state) -> int:
        """The seam must take enough decisions that the ORIGINAL lands under the
        CC-15 gate too (the max bound keeps the extracted side clean; this min
        bound keeps the original side clean). The fn's CC is its DIRECT body
        statements — a compound's own count already includes its subtree, so
        summing every flat entry would double-count loop bodies."""
        fn_sid = id(state.fn_node)
        total = sum(state.decisions[sid] for sid, container, _ in state.flat if container == fn_sid)
        # the CC gate is 15 decision points; the ORIGINAL keeps the rest
        gate = MethodExtraction._CC_GATE_DECISIONS
        return max(0, total - gate) if total > gate else 0

    @staticmethod
    def _module_level_names(module: cst.Module) -> set[str]:
        """The names bound at module scope: imports (and their aliases),
        assignments, and defs/classes. Any of these is ambient for a function
        placed at module level — the extracted helper needs no parameter for
        them."""
        names: set[str] = set()

        def add_target(t) -> None:
            if isinstance(t, cst.AssignTarget):
                add_target(t.target)
            elif isinstance(t, cst.Name):
                names.add(t.value)
            elif isinstance(t, cst.Tuple):
                for el in t.elements:
                    add_target(el.value)
            # obj.attr = ... binds nothing at module scope

        for line_stmt in module.body:
            MethodExtraction.collect_stmt_names(add_target, line_stmt, names)
        return names

    @staticmethod
    def _stmt_decision_count(stmt) -> int:
        probe = MethodExtraction._DecisionCount()
        stmt.visit(probe)
        return probe.n

    @staticmethod
    def _target_bound_names(t) -> set[str]:
        """Recursively collect the names an assignment target binds."""
        if isinstance(t, cst.Name):
            return {t.value}
        if isinstance(t, (cst.Tuple, cst.List)):
            out: set[str] = set()
            for el in t.elements:
                # an Element wraps its expression; a StarredElement wraps the
                # starred one — both carry the target at .value
                out |= MethodExtraction._target_bound_names(el.value)
            return out
        return set()

    @staticmethod
    def _wildcard(pattern) -> bool:
        return isinstance(pattern, cst.MatchAs) and pattern.pattern is None and pattern.name is None

    @staticmethod
    def collect_stmt_names(add_target, line_stmt, names):
        stmts = line_stmt.body if isinstance(line_stmt, cst.SimpleStatementLine) else [line_stmt]
        for stmt in stmts:
            if isinstance(stmt, cst.Import):
                Bindings.collect_alias_names(names, stmt)
            elif isinstance(stmt, cst.ImportFrom):
                if isinstance(stmt.names, cst.ImportStar):
                    continue  # `from x import *` binds no importable name
                for a in stmt.names:
                    if a.asname is not None:
                        names.update(Bindings.hoist_target_names(a.asname.name))
                    elif isinstance(a.name, cst.Name) and a.name.value != "*":
                        names.add(a.name.value)
            elif isinstance(stmt, (cst.Assign, cst.AnnAssign)):
                targets = stmt.targets if isinstance(stmt, cst.Assign) else [stmt.target]
                for t in targets:
                    add_target(t)
            elif isinstance(stmt, (cst.FunctionDef, cst.ClassDef)):
                names.add(stmt.name.value)

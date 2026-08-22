"""The auto-fix engine — libcst transforms for mechanical findings.

A finding carries (kind, file, line); each mechanical kind has a lossless
transform (libcst round-trips comments and formatting). The agent workflow:

    lucidlint.py fix --kind stale-suppression --file x.py --line 12

The transform edits the working tree; the gate re-run confirms the finding
is gone. libcst is optional (extra `fix`) — without it the engine reports
the missing dependency and exits non-zero; nothing is half-applied.

Transforms are deliberately MINIMAL: only the finding's node changes. The
structural tier (extract-class, split-function) needs a name and is not
here — those are agent-furnished (`--name`) and hand-verified.
"""

from __future__ import annotations

import builtins
from dataclasses import dataclass, field
from pathlib import Path
from typing import override

import libcst as cst
import libcst.matchers as m
from libcst.metadata import ExpressionContextProvider, ParentNodeProvider, PositionProvider

MECHANICAL_KINDS = {
    "stale-suppression": "delete the stale lucidlint: ignore comment",
    "noop-statement": "delete the dead statement",
    "unreachable": "delete the unreachable statement",
    "positional-literals": "keyword the literal arguments (same-file callee)",
    "duplicate-def": "delete the unreferenced shadowing module-scope def (renames stay with the agent)",
    "restating-docstring": "delete the docstring that restates the body",
    "duplicate-block": "delete the second copy of the repeated statement block",
}

@dataclass
class FixOptions:
    """The agent-supplied bits a fix may need: the callee signature for
    positional-literals, the class name for extract-class. `repo`/`rel`
    (filled by fix_finding) scope the repo-wide callee resolution."""
    params: list[str] | None = None
    name: str | None = None
    repo: Path | None = None
    rel: str | None = None


@dataclass
class StrewingGroup:
    """The free-function group extract-class moves — shared leading type,
    the fn names in source order, the anchor line."""
    shared: str
    fns: list[str] = field(default_factory=list)
    anchor: int = 0


# structural kinds need a name (agent-supplied via --fix-name, or defaulted to
# the shared leading type) — they are never applied blindly
STRUCTURAL_KINDS = {
    "extract-method": "extract the seam into a private function (preview without a name, apply with --fix-name)",
    "extract-class": "move the strewing free functions into a class, rewriting call sites",
    "extract-module": (
        "move the named module-scope defs (--params) into a new module "
        "(--name), re-exported from the origin"
    ),
    "magic-number": "Replace Magic Literal: introduce the named constant",
    "vague-name": "Rename the type and its references (same-file)",
    "long-param-list": "Introduce Parameter Object: bundle the params into a dataclass",
    "dispatch-registry": "convert the if/elif dispatch chain into a dict of selector -> handler functions",
    "rule-table": "hoist the latent data structure: the if/append battery becomes a (condition, violation) table",
}

# structural fixes whose result is genuinely novel (a class split, a new
# function, a bundled signature) preview a diff before --confirm; the
# obvious ones (a constant inserted, a rename) apply directly
PREVIEW_KINDS = {
    "extract-method", "extract-class", "extract-module", "long-param-list",
    "dispatch-registry", "rule-table",
}

# the gate reports DISPLAY kinds (final_kind output: strewing shows as
# latent-class); the fix command accepts either and normalizes here — the
# finding's message tees up the fix by name
KIND_ALIASES = {
    "latent-class": "extract-class",
    "complexity": "extract-method",
    "large-function": "extract-method",
}


# --------------------------------------------------------------------------- transforms

class _DeleteStatement(cst.CSTTransformer):
    """Remove the SimpleStatementLine covering the target line."""

    METADATA_DEPENDENCIES = (PositionProvider,)

    def __init__(self, target_line: int) -> None:
        self.target_line = target_line
        self.deleted = False

    @override
    def leave_SimpleStatementLine(
        self, original_node, updated_node
    ):
        if self.deleted:
            return updated_node
        pos = self.get_metadata(PositionProvider, original_node)
        if pos.start.line <= self.target_line <= pos.end.line:
            self.deleted = True
            return cst.RemoveFromParent()
        return updated_node


class _DeleteComment(cst.CSTTransformer):
    """Remove the `lucidlint: ignore` comment on the target line — both the
    standalone form (an EmptyLine's comment) and the trailing form."""

    METADATA_DEPENDENCIES = (PositionProvider,)

    def __init__(self, target_line: int) -> None:
        self.target_line = target_line
        self.deleted = False

    @override
    def leave_EmptyLine(
        self, original_node, updated_node
    ):
        if self.deleted or updated_node.comment is None:
            return updated_node
        if "lucidlint: ignore" not in updated_node.comment.value:
            return updated_node
        pos = self.get_metadata(PositionProvider, original_node)
        if pos.start.line == self.target_line:
            self.deleted = True
            return cst.RemoveFromParent()
        return updated_node

    @override
    def leave_Comment(
        self, original_node, updated_node
    ):
        if self.deleted or "lucidlint: ignore" not in updated_node.value:
            return updated_node
        pos = self.get_metadata(PositionProvider, original_node)
        if pos.start.line == self.target_line:
            self.deleted = True
            return cst.RemoveFromParent()
        return updated_node


class _KeywordArgs(cst.CSTTransformer):
    """Keyword the positional literal args of the call on the target line —
    parameter names come from the same-file callee definition."""

    METADATA_DEPENDENCIES = (PositionProvider,)

    def __init__(self, target_line: int, params: list[str]) -> None:
        self.target_line = target_line
        self.params = params
        self.done = False

    @override
    def leave_Call(
        self, original_node, updated_node
    ):
        if self.done or not updated_node.args:
            return updated_node
        pos = self.get_metadata(PositionProvider, original_node)
        if not (pos.start.line <= self.target_line <= pos.end.line):
            return updated_node
        # rebuild the positional args with keywords, in param order
        new_args = []
        pi = 0
        for arg in updated_node.args:
            if arg.keyword is not None:
                new_args.append(arg)
                continue
            if pi >= len(self.params):
                return updated_node  # out of params — leave untouched
            if not m.matches(
                arg.value,
                m.Integer() | m.Float() | m.SimpleString() | m.ConcatenatedString(),
            ):
                new_args.append(arg)
                pi += 1
                continue
            new_args.append(
                arg.with_changes(
                    keyword=cst.Name(self.params[pi]),
                    equal=cst.AssignEqual(
                        whitespace_before=cst.SimpleWhitespace(""),
                        whitespace_after=cst.SimpleWhitespace(""),
                    ),
                )
            )
            pi += 1
        self.done = True
        return updated_node.with_changes(args=new_args)


def _params_of_any_def(source: str, callee: str) -> list[str] | None:
    """Parameter names of a module-level `def callee(` or `class callee:`
    `__init__` in one module — self/cls dropped."""
    try:
        module = cst.parse_module(source)
    except Exception:
        return None
    found = None

    class _Find(cst.CSTVisitor):
        @override
        def visit_FunctionDef(
            self, node
        ) -> None:
            nonlocal found
            if node.name.value == callee and found is None:
                found = [p.name.value for p in node.params.params if p.name is not None]

        @override
        def visit_ClassDef(
            self, node
        ) -> None:
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


def _repo_params(repo: Path, rel: str, callee: str) -> list[str] | None:
    """Resolve the callee's params repo-wide — the call's file first, then
    every other .py under the repo (a module-level def or a class __init__).
    First match in sorted order wins; ambiguity is documented, not fatal."""
    candidates = sorted(
        p for p in repo.rglob("*.py")
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
        params = _params_of_any_def(source, callee)
        if params is not None:
            return params
    return None


# --------------------------------------------------------------------------- extract-class (strewing)

class _MoveIntoClass(cst.CSTTransformer):
    """Delete the strewing free functions from module scope; rewrite
    same-file call sites `fn(recv, ...)` -> `recv.fn(...)`."""

    METADATA_DEPENDENCIES = (PositionProvider,)

    def __init__(self, fns: set[str], delete: bool = True) -> None:
        self.fns = fns
        self.delete = delete

    @override
    def leave_FunctionDef(
        self, original_node, updated_node
    ):
        if self.delete and updated_node.name.value in self.fns:
            return cst.RemoveFromParent()
        return updated_node

    @override
    def leave_Call(
        self, original_node, updated_node
    ):
        if not m.matches(updated_node.func, m.Name()):
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


def _annotation_base(node) -> str | None:
    """The base name of a first-param annotation: `contract: GraphContract`
    -> "GraphContract"; unannotated or non-name -> None."""
    if node.annotation is None:
        return None
    ann = node.annotation.annotation
    if m.matches(ann, m.Name()):
        return ann.value
    if m.matches(ann, m.Subscript()):
        value = getattr(ann, "value", None)
        if m.matches(value, m.Name()):
            return value.value
    return None


def _strewing_group(source: str, anchor_line: int) -> StrewingGroup | None:
    """The (shared type, fn names, fn source of the anchor) for the strewing
    group anchored at `anchor_line` — mirroring the scanner's rule: >=3
    module-level defs sharing a leading param annotated with a file-local
    class. None when the finding is stale or not auto-fixable."""
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
            pos = self.get_metadata(PositionProvider, node)
            if pos.start.line == anchor_line:
                pass
                if node.params.params:
                    anchor_ann = _annotation_base(node.params.params[0])

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
            if _annotation_base(node.params.params[0]) == anchor_ann:
                fns.append(node.name.value)

    module.visit(_Group())
    if len(fns) < 3:
        return None
    return StrewingGroup(shared=anchor_ann, fns=fns, anchor=anchor_line)


def fix_extract_class(source: str, line: int, name: str | None) -> str | None:
    """Move the strewing group into a class named `name` (default: the shared
    leading type). Returns the new source, or None when the finding is stale
    or the group is not auto-fixable."""
    module = cst.parse_module(source)
    wrapper = cst.MetadataWrapper(module)
    found = _strewing_group(source, line)
    if found is None:
        return None
    shared = found.shared
    fns = found.fns
    class_name = name or shared

    # pass 1: rewrite the call sites WITHOUT deleting — the moved bodies must
    # be collected from this pass so their inter-fn calls are method calls
    # (`_window_score(state, ...)` -> `state._window_score(...)`)
    call_rewritten = wrapper.visit(_MoveIntoClass(set(fns), delete=False)).code

    # pass 2: per-function receiver rename (each fn's OWN first param, LOAD
    # references only), then swap the first param to `self`
    methods = _collect_defs(cst.parse_module(call_rewritten), set(fns))
    receivers = {
        m.name.value: (m.params.params[0].name.value if m.params.params else None)
        for m in methods
    }
    mod2 = cst.parse_module(call_rewritten)
    wrap2 = cst.MetadataWrapper(mod2)
    stores = _CollectStores()
    wrap2.visit(stores)
    renamed = wrap2.visit(_ReceiverToSelf(receivers, stores.per_fn))
    new_methods = []
    for method in _collect_defs(renamed, set(fns)):
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
    deleted = wrapper.visit(_MoveIntoClass(set(fns))).code
    classdef = cst.ClassDef(
        name=cst.Name(class_name),
        bases=[],
        body=cst.IndentedBlock(body=list(new_methods)),
    )
    return cst.parse_module(deleted).visit(_InsertClass(class_name, classdef, new_methods)).code


def fix_parameter_object(source: str, line: int, name: str) -> str | None:
    """Introduce Parameter Object: bundle the function's params into a
    dataclass named `name`, change the signature to `options: name`, rewrite
    the body references and same-file call sites."""
    module = cst.parse_module(source)
    wrapper = cst.MetadataWrapper(module)
    target = _find_fn_at(wrapper, line)
    if target is None:
        return None
    raw_params = [p for p in target.params.params if p.name is not None]
    params = [p for p in raw_params if p.name.value not in ("self", "cls")]
    if len(params) < 6:
        return None  # not the long-param-list shape (threshold is > 5)
    param_names = [p.name.value for p in params]
    renamed = wrapper.visit(_FnBodyRewrite(target.name.value, line, param_names, name)).code
    call_fixed = (
        cst.parse_module(renamed)
        .visit(_CallSiteRewrite(target.name.value, param_names, name))
        .code
    )
    module3 = cst.parse_module(call_fixed)
    body = list(module3.body)
    first = next(
        (i for i, s in enumerate(body) if isinstance(s, (cst.FunctionDef, cst.ClassDef))),
        len(body),
    )
    body.insert(first, _dataclass_def(name, params))
    return module3.with_changes(body=_ensure_dataclasses_import(body)).code


class _ExtractMethodRewrite(cst.CSTTransformer):
    """Replace the target block with a call to the new function. Works at any
    nesting depth: removed statements vanish via `on_leave`, and the call is
    inserted into their container's body at the first removed statement's
    index — so a seam inside a loop becomes a call inside the loop."""

    def __init__(self, block_sids: set[int], insertion, new_name: str, free_vars: list[str]) -> None:
        self.block_sids = block_sids
        self.container_sid, self.insert_index = insertion
        self.new_name = new_name
        self.free_vars = free_vars

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
                return updated_node.with_changes(
                    body=suite.with_changes(body=body)
                )
        return updated_node


class _InsertExtractedFn(cst.CSTTransformer):
    """Insert the extracted function after its source function, in the same
    container (module or class body)."""

    METADATA_DEPENDENCIES = (ParentNodeProvider,)

    def __init__(self, fn_name: str, fn_line: int, new_def: cst.FunctionDef) -> None:
        self.fn_name = fn_name
        self.fn_line = fn_line
        self.new_def = new_def
        self.done = False

    def _maybe_insert(self, body: list, blank_lines: int) -> list:
        if self.done:
            return body
        out = []
        for stmt in body:
            out.append(stmt)
            if not self.done and isinstance(stmt, cst.FunctionDef) and stmt.name.value == self.fn_name:
                out.append(
                    self.new_def.with_changes(
                        leading_lines=[cst.EmptyLine()] * blank_lines
                    )
                )
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


class _FnBodyState:
    """The extract-method analysis state: the target function's body
    statements with per-statement spans, first-use contexts, writes, and
    control-flow flags."""

    def __init__(self, line: int) -> None:
        self.line = line
        self.fn_node: cst.FunctionDef | None = None
        # flattened statement list: (sid, container_sid, index_in_container)
        self.flat: list[tuple[int, int, int]] = []
        self.nodes: dict[int, object] = {}          # sid -> statement node
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
        ambient = _BUILTINS | (self.module_globals - shadowed)
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
        SEQUENTIAL sense? Reads inside the window's own container (a loop's
        other body statements) are iteration-scoped and not out-variables:
        the loop target re-binds each pass. Only reads in LATER flat
        positions outside the window's container count."""
        container = self.flat[i][1]
        container_node = self.nodes.get(container)
        loop_scoped = isinstance(container_node, (cst.For, cst.While))
        window_subtree = set(self._window_sids(i, j))
        after: set[str] = set()
        for k in range(j + 1, len(self.flat)):
            sid = self.flat[k][0]
            if sid in window_subtree:
                continue  # inside the window's own subtree, not sequential-after
            if loop_scoped and self.flat[k][1] == container:
                continue  # a loop's later body stmts re-bind per iteration
            for dsid in self._subtree_sids(sid):
                for name, ctx in self.first_use[dsid].items():
                    if ctx == "read":
                        after.add(name)
        return bool(writes_all & after)

    def best_seam(self, min_lines: int = 2, max_window_decisions: int | None = None,
            min_window_decisions: int = 0, max_free_vars: int = 6,
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
                    window_decisions = sum(
                        self.decisions[self.flat[k][0]] for k in range(i, j + 1)
                    )
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


def _is_keyword_name(node, parent) -> bool:
    """A Name that is a keyword-argument's keyword (`f(a=1)` — the `a`) is
    not a variable reference; counting it made the seam analysis fabricate
    phantom parameters (the `leading_lines` in `with_changes(leading_lines=[])`
    would appear as a free var and the rewritten call would NameError)."""
    return isinstance(parent, cst.Arg) and parent.keyword is node


class _Analyse(cst.CSTVisitor):
    """Collects the target function's body statement data — flattened so
    seams can descend into compound statements (a big loop's inner chunks
    become extractable, not just the whole loop). Module-level so the
    latent-class rule does not fire on the enclosing analysis function."""

    METADATA_DEPENDENCIES = (PositionProvider, ExpressionContextProvider, ParentNodeProvider)

    def __init__(self, state: _FnBodyState) -> None:
        self.state = state

    @override
    def visit_FunctionDef(self, node) -> None:
        if (
            self.state.fn_node is not None
            or self.get_metadata(PositionProvider, node).start.line != self.state.line
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
            p = self.get_metadata(PositionProvider, s)
            self.state.stmt_spans[sid] = (p.start.line, p.end.line)
            self.state.first_use[sid] = {}
            self.state.writes[sid] = set()
            self.state.control_flow[sid] = self._subtree_control_flow(s)
            self.state.decisions[sid] = _stmt_decision_count(s)
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
                p = self.get_metadata(ParentNodeProvider, n)
                ok = False
                while p is not None and not isinstance(p, cst.Module):
                    if isinstance(p, (cst.For, cst.While)):
                        ok = id(p) in loops
                        break
                    p = self.get_metadata(ParentNodeProvider, p)
                if not ok:
                    return True
        return False

    @override
    def visit_Name(self, node) -> None:
        if self.state.fn_node is None:
            return
        sid = None
        parent = self.get_metadata(ParentNodeProvider, node)
        if _is_keyword_name(node, parent):
            return  # a keyword-argument name (f(a=1)) is not a variable ref
        while parent is not None and not isinstance(parent, cst.Module):
            if parent is self.state.fn_node:
                break
            if id(parent) in self.state.nodes:
                sid = id(parent)
                break
            parent = self.get_metadata(ParentNodeProvider, parent)
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


def _fn_seam_analysis(source: str, line: int):
    """Analyze the function at `line`: its body statements with per-statement
    spans, first-use contexts, writes, and control-flow flags."""
    module = cst.parse_module(source)
    wrapper = cst.MetadataWrapper(module)
    state = _FnBodyState(line)
    wrapper.visit(_Analyse(state))
    state.module_globals = _module_level_names(module)
    state.fn_writes = set().union(*(w for w in state.writes.values())) if state.writes else set()
    return module, wrapper, state


_BUILTINS = frozenset(dir(builtins))


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
        collect_stmt_names(add_target, line_stmt, names)
    return names
def collect_stmt_names(add_target, line_stmt, names):
    stmts = line_stmt.body if isinstance(line_stmt, cst.SimpleStatementLine) else [line_stmt]
    for stmt in stmts:
        if isinstance(stmt, cst.Import):
            for a in stmt.names:
                bound = a.asname.name.value if a.asname else a.name.value.split(".")[0]
                names.add(bound)
        elif isinstance(stmt, cst.ImportFrom):
            for a in stmt.names:
                if a.asname:
                    names.add(a.asname.name.value)
                elif a.name.value != "*":
                    names.add(a.name.value)
        elif isinstance(stmt, (cst.Assign, cst.AnnAssign)):
            targets = stmt.targets if isinstance(stmt, cst.Assign) else [stmt.target]
            for t in targets:
                add_target(t)
        elif isinstance(stmt, (cst.FunctionDef, cst.ClassDef)):
            names.add(stmt.name.value)





def _is_nested_target(wrapper: cst.MetadataWrapper, fn_node) -> bool:
    """Is the target function nested inside another function? The extracted
    helper is inserted at module/class level — a nested target would leave
    the call undefined (refuse rather than write a broken file)."""
    parent_resolver = wrapper.resolve(ParentNodeProvider)
    parent = parent_resolver[fn_node]
    while parent is not None and not isinstance(parent, (cst.Module, cst.ClassDef, cst.FunctionDef)):
        parent = parent_resolver[parent]
    return isinstance(parent, cst.FunctionDef)


def _min_seam_decisions(state) -> int:
    """The seam must take enough decisions that the ORIGINAL lands under the
    CC-15 gate too (the max bound keeps the extracted side clean; this min
    bound keeps the original side clean). The fn's CC is its DIRECT body
    statements — a compound's own count already includes its subtree, so
    summing every flat entry would double-count loop bodies."""
    fn_sid = id(state.fn_node)
    total = sum(
        state.decisions[sid] for sid, container, _ in state.flat if container == fn_sid
    )
    return max(0, total - 14) if total > 14 else 0


def extract_method_proposal(
    source: str, line: int, name: str | None = None, max_decisions: int | None = None,
):
    """Compute the best extraction seam and the resulting source, WITHOUT
    writing. Returns (new_source, seam_text) or (None, None) when no safe
    seam exists. `name` may be None — the preview then shows a placeholder
    (`_extracted`) the agent replaces after reviewing the seam; a supplied
    name is normalized to the private convention (the extracted function
    cannot have external callers, so it takes the underscore)."""
    if name is None:
        name = "_extracted"
    elif _extraction_is_private() and not name.startswith("_"):
        name = "_" + name
    module, wrapper, state = _fn_seam_analysis(source, line)
    if state.fn_node is None or len(state.flat) < 2:
        return None, None
    # a nested-function target cannot host the extracted helper — the insert
    # lands at module/class level, so the rewritten call would NameError
    # (refuse rather than write a broken file)
    if _is_nested_target(wrapper, state.fn_node):
        return None, None
    seam = state.best_seam(
        max_window_decisions=max_decisions,
        min_window_decisions=_min_seam_decisions(state),
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
        _ExtractMethodRewrite(set(block_sids), (container_sid, insert_index), name, free_vars)
    ).code
    inserted = cst.MetadataWrapper(cst.parse_module(replaced)).visit(
        _InsertExtractedFn(state.fn_node.name.value, line, new_def)
    ).code
    seam_text = source.splitlines()[first_span[0] - 1] if source else ""
    return inserted, f"line {first_span[0]}: {seam_text.strip()}"


def _extraction_is_private() -> bool:
    """Is an extracted function private (leading underscore)? Yes — a fresh
    helper cannot have external callers (it did not exist before the fix),
    so by construction any extraction is an implementation detail of its
    source function. The convention propagates automatically: the seam's
    name gets the underscore whether the agent supplies it or not."""

    return True


def fix_extract_method(source: str, line: int, name: str | None) -> str | None:
    """Extract Function (applied): the best self-contained seam of the
    function at `line` becomes a new function named `name`. The seam is
    bounded to <= 13 decisions so the extracted function lands under the
    CC-15 gate — extraction SPLITS complexity, it does not move it. The
    name is normalized: the extracted function is private by construction
    (see _extraction_is_private), so a public-looking name is underscored."""
    new_source, _ = extract_method_proposal(source, line, name, max_decisions=13)
    return new_source


def propose_finding(kind: str, rel: str, repo: Path, line: int, opts: FixOptions | None = None):
    """Compute the fix WITHOUT writing — the preview surface for every
    structural kind. Returns (new_source, description) or (None, None) when
    nothing changes or the agent's semantic bit is missing."""
    opts = opts or FixOptions()
    kind = KIND_ALIASES.get(kind, kind)
    path = repo / rel
    source = path.read_text(encoding="utf-8")
    if kind not in STRUCTURAL_KINDS:
        return None, None  # mechanical kinds apply directly, no preview
    if kind == "extract-method":
        # name-free preview: the seam is shown with a placeholder name the
        # agent replaces — naming AFTER seeing the diff, not before
        new_source, seam = extract_method_proposal(source, line, opts.name, max_decisions=13)
        if new_source is None or new_source == source:
            return None, None
        return new_source, seam  # "line N: <first seam line>" — what moves

    if kind == "extract-module":
        # the preview shows the origin diff; the description names the seam
        # (the members moving and the new module) so the agent can judge.
        # Name-free: the reexport line uses a placeholder the agent replaces
        # (naming AFTER seeing the diff); the apply path requires the real
        # module name.
        if not opts.params:
            return None, None
        preview_opts = FixOptions(
            params=opts.params,
            name=opts.name or "_extracted",
            repo=opts.repo,
            rel=opts.rel,
        )
        result = _extract_module_proposal(source, preview_opts)
        if result is None or result[0] == source:
            return None, None
        return result[0], (
            f"extract-module: moves {', '.join(opts.params)} into a new module "
            "(name the module to apply)"
        )
    new_source = _fix_structural(kind, source, line, opts)
    if new_source is None or new_source == source:
        return None, None
    return new_source, STRUCTURAL_KINDS[kind]


class _CollectStores(cst.CSTVisitor):
    """The names each function assigns anywhere in its body — those are
    locals, never the receiver (a shadowing `state = state.line` must not
    have its loads renamed to `self`). One pass over the whole module,
    tracking the current function."""

    METADATA_DEPENDENCIES = (ExpressionContextProvider,)

    def __init__(self) -> None:
        self.per_fn: dict[str, set[str]] = {}
        self._current: str | None = None
        self._in_param = False
        self._fn_stack: list[str | None] = []

    @override
    def visit_FunctionDef(self, node) -> None:
        # remember the enclosing fn so leave_FunctionDef can restore it —
        # a nested def must not clobber the strewing fn's attribution
        self._fn_stack.append(self._current)
        self._current = node.name.value
        self.per_fn.setdefault(self._current, set())

    @override
    def leave_FunctionDef(self, original_node) -> None:
        self._current = self._fn_stack.pop()

    @override
    def visit_Param(self, node) -> None:
        # parameter names are STORE-context Names — they are the signature,
        # not body locals; skip them
        self._in_param = True

    @override
    def leave_Param(self, original_node) -> None:
        self._in_param = False

    @override
    def visit_Name(self, node) -> None:
        try:
            ctx = self.get_metadata(ExpressionContextProvider, node)
        except KeyError:
            return  # attribute names (obj.state) are not refs
        if ctx == cst.metadata.ExpressionContext.STORE and self._current is not None and not self._in_param:
            self.per_fn[self._current].add(node.value)


# each moved function's stored-name set — a named alias so the signature is
# not a bare dict collection (record-shape)
StoredNames = dict[str, set[str]]


class _ReceiverToSelf(cst.CSTTransformer):
    """The extract-class receiver rename: in each moved function, LOAD
    references to THAT function's own first parameter become `self`.
    Per-function (different strewing fns may name their receiver
    differently) and shadow-aware — a name stored anywhere in the body
    is a local and is never renamed, so `state = state.line` survives
    intact instead of becoming `self = self.line`."""

    METADATA_DEPENDENCIES = (ExpressionContextProvider, ParentNodeProvider)

    def __init__(self, receivers: dict[str, str | None], shadowed: StoredNames) -> None:
        self.receivers = receivers  # fn name -> its receiver param name
        self.shadowed = shadowed  # fn name -> names stored in its body
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
        if _is_keyword_name(original_node, parent):
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


class _InsertClass(cst.CSTTransformer):
    """Append the methods to the existing class, or insert a new class after
    the last one."""

    def __init__(self, class_name, classdef, methods) -> None:
        self.class_name = class_name
        self.classdef = classdef
        self.methods = methods

    @override
    def leave_ClassDef(
        self, original_node, updated_node
    ):
        if updated_node.name.value == self.class_name:
            existing = list(updated_node.body.body)
            existing.extend(self.methods)
            return updated_node.with_changes(
                body=updated_node.body.with_changes(body=existing)
            )
        return updated_node

    @override
    def leave_Module(
        self, original_node, updated_node
    ):
        if self.class_name in {
            s.name.value for s in updated_node.body if isinstance(s, cst.ClassDef)
        }:
            return updated_node
        body = list(updated_node.body)
        idx = max(
            (i for i, s in enumerate(body) if isinstance(s, cst.ClassDef)),
            default=-1,
        )
        body.insert(idx + 1, self.classdef)
        return updated_node.with_changes(body=body)




class _BodyParamRewrite(cst.CSTTransformer):
    """Rewrite body references of the bundled params to options.<param>."""

    def __init__(self, params: list[str]) -> None:
        self.params = set(params)

    @override
    def on_visit(self, node) -> bool:
        # nested functions and lambdas are their own scopes: a bundled name
        # there is a local/parameter of THAT scope, not the outer param
        # (def inner(a) must not become def inner(options.a))
        return not isinstance(node, (cst.FunctionDef, cst.Lambda))

    @override
    def leave_Name(
        self, original_node, updated_node
    ):
        if updated_node.value in self.params:
            return cst.Attribute(
                value=cst.Name("options"),
                attr=cst.Name(updated_node.value),
            )
        return updated_node


class _CallSiteRewrite(cst.CSTTransformer):
    """f(a, b, c) -> Name.f(a=a, b=b, c=c) for the renamed function."""

    def __init__(self, fn: str, params: list[str], class_name: str) -> None:
        self.fn = fn
        self.params = params
        self.class_name = class_name

    @override
    def leave_Call(
        self, original_node, updated_node
    ):
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


def _dataclass_def(name: str, params: list[cst.Param]) -> cst.ClassDef:
    """The bundled-params dataclass — one AnnAssign field per param."""
    field_lines = []
    for p in params:
        ann = p.annotation.annotation if p.annotation is not None else cst.Name("object")
        field_lines.append(
            cst.SimpleStatementLine(
                body=[
                    cst.AnnAssign(
                        target=cst.Name(p.name.value), annotation=cst.Annotation(ann)
                    )
                ]
            )
        )
    return cst.ClassDef(
        name=cst.Name(name),
        bases=[],
        decorators=[cst.Decorator(cst.Name("dataclass"))],
        body=cst.IndentedBlock(body=field_lines),
    )


def _ensure_dataclasses_import(body: list) -> list:
    """Prepend `from dataclasses import dataclass` when missing."""
    has_import = any(
        isinstance(s, cst.SimpleStatementLine)
        and len(s.body) == 1
        and isinstance(s.body[0], cst.ImportFrom)
        and s.body[0].module is not None
        and "dataclasses" in s.body[0].module.value
        and any(
            isinstance(a, cst.ImportAlias) and a.name.value == "dataclass"
            for a in (s.body[0].names or [])
        )
        for s in body
    )
    if has_import:
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


def _find_fn_at(wrapper, line: int, name: str | None = None) -> cst.FunctionDef | None:
    """The module-level def at `line` (optionally by name)."""
    found: cst.FunctionDef | None = None

    class _Find(cst.CSTVisitor):
        METADATA_DEPENDENCIES = (PositionProvider,)

        @override
        def visit_FunctionDef(self, node) -> None:
            nonlocal found
            if (
                found is None
                and self.get_metadata(PositionProvider, node).start.line == line
                and (name is None or node.name.value == name)
            ):
                found = node

    wrapper.visit(_Find())
    return found


class _FnBodyRewrite(cst.CSTTransformer):
    """The parameter-object fn: params -> options.<param> in the body, and the
    signature collapses to (receiver, options: Name)."""

    METADATA_DEPENDENCIES = (PositionProvider,)

    def __init__(self, fn_name: str, line: int, params: list[str], class_name: str) -> None:
        self.fn_name = fn_name
        self.line = line
        self.params = set(params)
        self.class_name = class_name

    @override
    def leave_FunctionDef(
        self, original_node, updated_node
    ):
        if updated_node.name.value != self.fn_name:
            return updated_node
        if self.get_metadata(PositionProvider, original_node).start.line != self.line:
            return updated_node
        new_body = updated_node.body.visit(_BodyParamRewrite(list(self.params)))
        receiver = [
            p
            for p in updated_node.params.params
            if p.name is not None and p.name.value in ("self", "cls")
        ]
        options_param = cst.Param(
            name=cst.Name("options"),
            annotation=cst.Annotation(cst.Name(self.class_name)),
        )
        return updated_node.with_changes(
            params=updated_node.params.with_changes(params=[*receiver, options_param]),
            body=new_body,
        )


class _ReplaceLiteral(cst.CSTTransformer):
    """Replace the numeric literal on the target line with a name."""

    METADATA_DEPENDENCIES = (PositionProvider,)

    def __init__(self, target_line: int, name: str) -> None:
        self.target_line = target_line
        self.name = name
        self.replaced = False

    @override
    def leave_Integer(
        self, original_node, updated_node
    ):
        if self.replaced:
            return updated_node
        pos = self.get_metadata(PositionProvider, original_node)
        if pos.start.line == self.target_line:
            self.replaced = True
            return cst.Name(self.name)
        return updated_node

    @override
    def leave_Float(
        self, original_node, updated_node
    ):
        if self.replaced:
            return updated_node
        pos = self.get_metadata(PositionProvider, original_node)
        if pos.start.line == self.target_line:
            self.replaced = True
            return cst.Name(self.name)
        return updated_node


def fix_magic_literal(source: str, line: int, name: str) -> str | None:
    """Replace Magic Literal: `f(10, ...)` -> `f(MAX_RETRIES, ...)` with
    `MAX_RETRIES = 10` inserted at module top."""
    module = cst.parse_module(source)
    wrapper = cst.MetadataWrapper(module)
    value: str | None = None

    class _Find(cst.CSTVisitor):
        METADATA_DEPENDENCIES = (PositionProvider,)

        @override
        def visit_Integer(self, node) -> None:
            nonlocal value
            if value is None and self.get_metadata(PositionProvider, node).start.line == line:
                value = node.value

        @override
        def visit_Float(self, node) -> None:
            nonlocal value
            if value is None and self.get_metadata(PositionProvider, node).start.line == line:
                value = node.value

    wrapper.visit(_Find())
    if value is None:
        return None
    replaced = wrapper.visit(_ReplaceLiteral(line, name)).code
    if replaced == source:
        return None
    module2 = cst.parse_module(replaced)
    assignment = cst.SimpleStatementLine(
        body=[
            cst.Assign(
                targets=[cst.AssignTarget(cst.Name(name))],
                value=cst.parse_expression(value),
            )
        ]
    )
    body = list(module2.body)
    first = next(
        (i for i, s in enumerate(body) if isinstance(s, (cst.FunctionDef, cst.ClassDef))),
        len(body),
    )
    body.insert(first, assignment)
    return module2.with_changes(body=body).code


class _RenameClass(cst.CSTTransformer):
    """Rename the class at the line + every Name reference in the file."""

    METADATA_DEPENDENCIES = (PositionProvider,)

    def __init__(self, target_line: int, old: str, new: str) -> None:
        self.target_line = target_line
        self.old = old
        self.new = new

    @override
    def leave_ClassDef(
        self, original_node, updated_node
    ):
        pos = self.get_metadata(PositionProvider, original_node)
        if pos.start.line == self.target_line and updated_node.name.value == self.old:
            return updated_node.with_changes(name=cst.Name(self.new))
        return updated_node

    @override
    def leave_Name(
        self, original_node, updated_node
    ):
        if updated_node.value == self.old:
            return updated_node.with_changes(value=self.new)
        return updated_node


def fix_rename(source: str, line: int, name: str) -> str | None:
    """Rename (vague-name): the class at `line` plus every same-file Name
    reference. Cross-file call sites need FullRepoManager — same-file v1."""
    module = cst.parse_module(source)
    wrapper = cst.MetadataWrapper(module)
    old: str | None = None

    class _Find(cst.CSTVisitor):
        METADATA_DEPENDENCIES = (PositionProvider,)

        @override
        def visit_ClassDef(self, node) -> None:
            nonlocal old
            if old is None and self.get_metadata(PositionProvider, node).start.line == line:
                old = node.name.value

    wrapper.visit(_Find())
    if old is None or old == name:
        return None
    renamed = wrapper.visit(_RenameClass(line, old, name)).code
    return None if renamed == source else renamed


class _DecisionCount(cst.CSTVisitor):
    """Counts the radon-style decisions in one statement: ifs + elifs, and/or
    BooleanOperations, loops, asserts, match arms, comprehension clauses —
    the same families the scanner's complexity rule counts (extraction must
    SPLIT the CC, not move it)."""

    def __init__(self) -> None:
        self.n = 0
        self._boolop_depth = 0
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
        self.n += sum(1 for c in node.cases if not _wildcard(c.pattern))

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
                self.n += _chain_operands(node) - 1
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


def _chain_operands(node) -> int:
    """The number of operands in a chained and/or tree — radon's
    `len(values)` for a BoolOp; a non-BoolOp leaf is one operand."""
    if isinstance(node, cst.BooleanOperation) and isinstance(node.operator, (cst.And, cst.Or)):
        return _chain_operands(node.left) + _chain_operands(node.right)
    return 1


def _wildcard(pattern) -> bool:
    return isinstance(pattern, cst.MatchAs) and pattern.pattern is None and pattern.name is None


def _stmt_decision_count(stmt) -> int:
    probe = _DecisionCount()
    stmt.visit(probe)
    return probe.n


# --------------------------------------------------------------------------- the fix surface

def _fix_mechanical(kind: str, source: str, line: int, opts) -> str | None:
    """The mechanical transforms — a changed source, or None when the callee
    is unresolvable (the retry protocol supplies params)."""
    if kind in ("noop-statement", "unreachable"):
        return cst.MetadataWrapper(cst.parse_module(source)).visit(_DeleteStatement(line)).code
    if kind == "stale-suppression":
        return cst.MetadataWrapper(cst.parse_module(source)).visit(_DeleteComment(line)).code
    if kind == "positional-literals":
        params = opts.params if opts.params is not None else _callee_params_for_call(opts.repo, opts.rel, source, line)
        if params is None:
            return None
        return cst.MetadataWrapper(cst.parse_module(source)).visit(_KeywordArgs(line, params)).code
    if kind == "duplicate-def":
        return fix_duplicate_def(source, line, opts)
    if kind == "restating-docstring":
        return fix_restating_docstring(source, line)
    if kind == "duplicate-block":
        return fix_duplicate_block(source, line)
    return None

# --------------------------------------------------------------------------- review-log fixes
# duplicate-def (delete the unreferenced shadow), restating-docstring (delete
# the docstring), duplicate-block (delete the second copy), extract-module
# (move a domain's defs to a new module) — the family-album review log §10/§11.

_REPO_SKIP_DIRS = {
    ".git", ".venv", "venv", "node_modules", "__pycache__", ".lucidlint-cache", ".ruff_cache",
    ".pytest_cache", ".mypy_cache", ".pyrefly-cache", "htmlcov", "dist", "build", "target",
    ".code-review-graph", ".tox", ".eggs",
}


def _py_files(repo: Path) -> list[Path]:
    """Every .py file under the repo, skipping venvs/caches/build output."""
    out: list[Path] = []
    stack = [repo]
    while stack:
        d = stack.pop()
        try:
            entries = list(d.iterdir())
        except OSError:
            continue
        for p in entries:
            if p.is_dir():
                if p.name not in _REPO_SKIP_DIRS:
                    stack.append(p)
            elif p.suffix == ".py":
                out.append(p)
    return sorted(out)


class _NameCount(cst.CSTVisitor):
    def __init__(self, name: str) -> None:
        self.name = name
        self.n = 0

    @override
    def visit_Name(self, node) -> None:
        if node.value == self.name:
            self.n += 1


def _name_occurrences(repo: Path, name: str) -> int:
    """Name-node occurrences of `name` across the repo's .py files — the
    duplicate-def safety check: a delete is only offered when the shadowing
    def is provably unreferenced (the two def sites are the only hits)."""
    total = 0
    for p in _py_files(repo):
        try:
            module = cst.parse_module(p.read_text(encoding="utf-8"))
        except Exception:  # parse errors in other files are their own findings
            continue

        probe = _NameCount(name)
        module.visit(probe)
        total += probe.n
    return total


class _FindModuleDef(cst.CSTVisitor):
    """The module-scope def (depth 0) whose def line equals the target — the
    duplicate-def finding's second binding."""

    METADATA_DEPENDENCIES = (PositionProvider,)

    def __init__(self, line: int) -> None:
        self.line = line
        self.depth = 0
        self.found: cst.CSTNode | None = None
        self.name: str | None = None

    @override
    def visit_FunctionDef(self, node) -> None:
        if self.depth == 0 and self.found is None and self.get_metadata(PositionProvider, node).start.line == self.line:
            self.found, self.name = node, node.name.value
        self.depth += 1

    @override
    def leave_FunctionDef(self, node) -> None:
        self.depth -= 1

    @override
    def visit_ClassDef(self, node) -> None:
        if self.depth == 0 and self.found is None and self.get_metadata(PositionProvider, node).start.line == self.line:
            self.found, self.name = node, node.name.value
        self.depth += 1

    @override
    def leave_ClassDef(self, node) -> None:
        self.depth -= 1


class _RemoveNodes(cst.CSTTransformer):
    """Remove exactly the given nodes from the tree (identity)."""

    def __init__(self, targets) -> None:
        self.targets = set(targets)

    @override
    def on_leave(self, original_node, updated_node):
        if original_node in self.targets:
            return cst.RemoveFromParent()
        return updated_node


def fix_duplicate_def(source: str, line: int, opts) -> str | None:
    """Delete the shadowing (second) module-scope binding when nothing
    references it — proven by a repo-wide Name count (only the two def sites
    hit). Referenced shadows are renamed by the agent, never auto-deleted."""
    wrapper = cst.MetadataWrapper(cst.parse_module(source))
    probe = _FindModuleDef(line)
    wrapper.visit(probe)
    if probe.found is None or probe.name is None:
        return None
    if _name_occurrences(opts.repo, probe.name) > 2:
        return None  # something references the name — a delete would break it
    return wrapper.module.visit(_RemoveNodes([probe.found])).code


def _strip_first_docstring(updated, original) -> object:
    body = updated.body.body
    if body and len(body[0].body) == 1:
        stmt = body[0].body[0]
        if isinstance(stmt, cst.Expr) and isinstance(stmt.value, cst.SimpleString):
            return updated.with_changes(body=updated.body.with_changes(body=list(body[1:])))
    return updated


class _StripDocstring(cst.CSTTransformer):
    """Delete the first body statement of the def at `line` when it is a
    string-literal expression — the restating docstring (the rule proved it
    adds nothing beyond the body tokens)."""

    METADATA_DEPENDENCIES = (PositionProvider,)

    def __init__(self, line: int) -> None:
        self.line = line
        self.done = False

    @override
    def leave_FunctionDef(self, original_node, updated_node):
        if self.done:
            return updated_node
        if self.get_metadata(PositionProvider, original_node).start.line == self.line:
            self.done = True
            return _strip_first_docstring(updated_node, original_node)
        return updated_node

    @override
    def leave_ClassDef(self, original_node, updated_node):
        if self.done:
            return updated_node
        if self.get_metadata(PositionProvider, original_node).start.line == self.line:
            self.done = True
            return _strip_first_docstring(updated_node, original_node)
        return updated_node


def fix_restating_docstring(source: str, line: int) -> str | None:
    return cst.MetadataWrapper(cst.parse_module(source)).visit(_StripDocstring(line)).code


    # lucidlint: ignore complexity the flatten dispatches over statement kinds — a dispatch table, not branching
def _flatten_stmts(stmts, out: list) -> None:
    """Source-ordered statement flatten: nested block bodies included, so a
    loop body repeated after the loop is one sequence (the Rust rule's
    mirror)."""
    for s in stmts:
        out.append(s)
        if isinstance(s, cst.If):
            _flatten_stmts(list(s.body.body), out)
            if s.orelse is not None:
                for cl in s.orelse:
                    if isinstance(cl.body, cst.IndentedBlock):
                        _flatten_stmts(list(cl.body.body), out)
        elif isinstance(s, (cst.For, cst.While)):
            _flatten_stmts(list(s.body.body), out)
            if s.orelse is not None:
                _flatten_stmts(list(s.orelse.body), out)
        elif isinstance(s, cst.Try):
            _flatten_stmts(list(s.body.body), out)
            for h in s.handlers:
                _flatten_stmts(list(h.body.body), out)
            if s.orelse is not None:
                _flatten_stmts(list(s.orelse.body), out)
            if s.finalbody is not None:
                _flatten_stmts(list(s.finalbody.body), out)
        elif isinstance(s, cst.With):
            _flatten_stmts(list(s.body.body), out)
        elif isinstance(s, cst.Match):
            for case in s.cases:
                _flatten_stmts(list(case.body.body), out)


    # the visitor classes ARE the libcst walk idiom — a class would scatter
    # one pass
    # lucidlint: ignore complexity,latent-class the visitor classes are the walk idiom
def fix_duplicate_block(source: str, line: int) -> str | None:
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
            pos = self.get_metadata(PositionProvider, node)
            if pos.start.line <= self.target <= pos.end.line:
                self.fn = node  # last (innermost) containing function wins

    finder = _FindFn(line)
    wrapper.visit(finder)
    if finder.fn is None:
        return None
    flat: list = []
    _flatten_stmts(list(finder.fn.body.body), flat)
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
    return wrapper.module.visit(_RemoveNodes(targets)).code


def _moved_imports(module: cst.Module, referenced: set[str]) -> list:
    """The origin's module-level imports binding a referenced name — what the
    new module needs (libcst wraps imports in SimpleStatementLine)."""
    moved: list = []
    for stmt in module.body:
        if not (isinstance(stmt, cst.SimpleStatementLine) and len(stmt.body) == 1):
            continue
        inner = stmt.body[0]
        if isinstance(inner, cst.Import):
            for alias in inner.names:
                bound = alias.asname.name.value if alias.asname else alias.name.value.split(".")[0]
                if bound in referenced:
                    moved.append(stmt)
                    break
        elif isinstance(inner, cst.ImportFrom):
            if isinstance(inner.names, cst.ImportStar):
                continue  # `from x import *` binds no importable name
            for alias in inner.names:
                if not isinstance(alias.name, cst.Name):
                    continue
                bound = alias.asname.name.value if alias.asname else alias.name.value
                if bound in referenced:
                    moved.append(stmt)
                    break
    return moved


def _origin_after_move(module: cst.Module, move: set[str], opts) -> list:
    """The origin's body after the split: the moved defs dropped, the
    re-export import inserted after the last import — every other file's
    `from origin import x` keeps working (the origin re-exports). The
    re-export is RELATIVE when the origin sits in a package (`houses/text.py`
    is not on sys.path as a top-level module) — review finding."""
    remaining = [
        s
        for s in module.body
        if not (isinstance(s, (cst.FunctionDef, cst.ClassDef)) and s.name.value in move)
    ]
    in_package = "/" in (opts.rel or "")
    reexport = cst.SimpleStatementLine(
        body=[
            cst.ImportFrom(
                module=cst.Name(opts.name),
                names=[cst.ImportAlias(name=cst.Name(n)) for n in opts.params],
                relative=[cst.Dot()] if in_package else (),
                lpar=None,
                rpar=None,
            )
        ]
    )
    insert_at = 0
    for i, s in enumerate(remaining):
        if isinstance(s, cst.SimpleStatementLine) and len(s.body) == 1 and isinstance(
            s.body[0], (cst.Import, cst.ImportFrom)
        ):
            insert_at = i + 1
    remaining.insert(insert_at, reexport)
    return remaining


    # lucidlint: ignore record-shape the two-source result is a wire seam — a class is ceremony for one return
def _extract_module_proposal(source: str, opts) -> tuple[str, str] | None:
    """(new origin source, new module source) for the extract-module split, or
    None when the split is not safe. Pure — no writes; the apply path writes
    both files. The moved defs are the --params names; the module is --name
    (same directory as the origin). Refuses when the moved code needs a
    non-moved origin def (a from-origin import in the new module would create
    the very cycle the review log §3.4 fixed) or a decorated def (registration
    semantics)."""
    if not opts.name or not opts.params:
        return None
    module = cst.parse_module(source)
    move = set(opts.params)
    top_defs = {
        stmt.name.value: stmt
        for stmt in module.body
        if isinstance(stmt, (cst.FunctionDef, cst.ClassDef))
    }
    if not move <= set(top_defs):
        return None  # a named member is not module-scope here
    for name in move:
        if top_defs[name].decorators:
            return None
    # free reads only: a name bound inside the moved function (parameter,
    # local, for/comprehension target) cannot reference a module-level
    # binding — counting it would falsely refuse safe splits (review finding)
    referenced: set[str] = set()
    for name in move:
        free = _FreeNames()
        top_defs[name].visit(free)
        referenced |= free.names - free.bound
    if referenced & (set(top_defs) - move):
        return None  # the moved code needs an origin def — cycle risk
    # module-level assignments/constants the moved code reads would be
    # missing from the new module (a runtime NameError) — refuse; moving
    # them would break the origin's remaining code (review finding)
    if referenced & _module_bindings(module):
        return None
    new_module = cst.Module(
        body=[*_moved_imports(module, referenced), *(top_defs[n] for n in opts.params)]
    )
    return cst.Module(body=_origin_after_move(module, move, opts)).code, new_module.code


def _module_bindings(module: cst.Module) -> set[str]:
    """The names module-level assignments/constants bind — the extract-module
    split refuses when moved code reads one (the new module would NameError;
    moving the binding would break the origin)."""
    bound: set[str] = set()
    for stmt in module.body:
        if not (isinstance(stmt, cst.SimpleStatementLine) and len(stmt.body) == 1):
            continue
        inner = stmt.body[0]
        if isinstance(inner, cst.Assign):
            for t in inner.targets:
                bound |= _target_names(t.target)
        elif isinstance(inner, cst.AnnAssign) and inner.target is not None:
            bound |= _target_names(inner.target)
    return bound


class _FreeNames(cst.CSTVisitor):
    """The names a function READS from enclosing scopes: every Name node in
    the tree minus the names bound anywhere inside it (parameters, locals,
    for/comprehension/with targets, nested def names). A name bound inside
    cannot be a reference to a module-level binding, so the extract-module
    module-binding check must exclude it (review finding: a moved function
    with a parameter named like a module constant was falsely refused)."""

    def __init__(self) -> None:
        self.names: set[str] = set()
        self.bound: set[str] = set()

    def _add_params(self, params) -> None:
        for p in params:
            if p.name is not None:
                self.bound.add(p.name.value)

    @override
    def visit_FunctionDef(self, node) -> None:
        self.bound.add(node.name.value)
        self._add_params(node.params.params)
        self._add_params(node.params.kwonly_params)
        if node.params.star_arg is not None and isinstance(node.params.star_arg.name, cst.Name):
            self.bound.add(node.params.star_arg.name.value)
        if node.params.star_kwarg is not None and isinstance(node.params.star_kwarg.name, cst.Name):
            self.bound.add(node.params.star_kwarg.name.value)

    @override
    def visit_Lambda(self, node) -> None:
        self._add_params(node.params.params)
        self._add_params(node.params.kwonly_params)

    @override
    def visit_Name(self, node) -> None:
        self.names.add(node.value)

    @override
    def visit_Assign(self, node) -> None:
        for t in node.targets:
            self.bound.update(_target_names(t.target))

    @override
    def visit_AnnAssign(self, node) -> None:
        if node.target:
            self.bound.update(_target_names(node.target))

    @override
    def visit_For(self, node) -> None:
        self.bound.update(_target_names(node.target))

    @override
    def visit_CompFor(self, node) -> None:
        self.bound.update(_target_names(node.target))

    @override
    def visit_With(self, node) -> None:
        for item in node.items:
            if item.optional_vars:
                self.bound.update(_target_names(item.optional_vars))

    @override
    def visit_ExceptHandler(self, node) -> None:
        if node.name is not None:
            self.bound.add(node.name.value)





def fix_extract_module(source: str, opts) -> str | None:
    """The apply side of extract-module — writes the new module and returns
    the origin's new source (fix_finding writes the origin)."""
    result = _extract_module_proposal(source, opts)
    if result is None:
        return None
    origin_source, new_module_source = result
    new_path = opts.repo / (opts.rel.rsplit("/", 1)[0] if "/" in opts.rel else "") / f"{opts.name}.py"
    if new_path.exists():
        return None  # never clobber an existing module
    new_path.parent.mkdir(parents=True, exist_ok=True)
    new_path.write_text(new_module_source, encoding="utf-8")
    return origin_source


class _FindFnLine(cst.CSTVisitor):
    """Locate the first FunctionDef whose def line equals the target — its
    node comes from the wrapper's (metadata-resolvable) tree."""

    METADATA_DEPENDENCIES = (PositionProvider,)

    def __init__(self, line: int) -> None:
        self.line = line
        self.found: cst.FunctionDef | None = None

    @override
    def visit_FunctionDef(self, node) -> None:
        if self.found is None and self.get_metadata(PositionProvider, node).start.line == self.line:
            self.found = node



def _fix_structural(kind: str, source: str, line: int, opts) -> str | None:
    """The name-driven transforms — the agent supplies the semantic bit."""
    if kind == "extract-method":
        if opts.name is None:
            return None
        return fix_extract_method(source, line, opts.name)
    if kind == "extract-class":
        return fix_extract_class(source, line, opts.name)
    if kind == "magic-number":
        if opts.name is None:
            return None
        return fix_magic_literal(source, line, opts.name)
    if kind == "vague-name":
        if opts.name is None:
            return None
        return fix_rename(source, line, opts.name)
    if kind == "long-param-list":
        if opts.name is None:
            return None
        return fix_parameter_object(source, line, opts.name)
    if kind == "dispatch-registry":
        return fix_dispatch_registry(source, line)
    if kind == "rule-table":
        return fix_rule_table(source, line)
    return None


def fix_finding(
    kind: str, rel: str, repo: Path, line: int, opts: FixOptions | None = None
) -> str | None:
    """Apply the transform for one finding. Returns a human description of
    what changed, or None when the finding was already gone (no edit).

    `opts` carries the agent-supplied semantic bits (the callee's parameter
    names for external/unresolved callees; the class name for extract-class)
    — the tool does the mechanical edit; the agent reads the signature once.
    """
    opts = opts or FixOptions()
    kind = KIND_ALIASES.get(kind, kind)
    path = repo / rel
    source = path.read_text(encoding="utf-8")
    opts.repo = repo
    opts.rel = rel
    if kind in MECHANICAL_KINDS:
        new_source = _fix_mechanical(kind, source, line, opts)
        description = MECHANICAL_KINDS[kind]
    elif kind == "extract-module":
        # two files change: the new module is created, the origin re-exports
        # the moved defs. Never clobbers an existing module.
        new_source = fix_extract_module(source, opts)
        description = STRUCTURAL_KINDS[kind]
    elif kind in STRUCTURAL_KINDS:
        new_source = _fix_structural(kind, source, line, opts)
        description = STRUCTURAL_KINDS[kind]
    else:
        raise ValueError(f"kind '{kind}' has no fix (mechanical or structural)")
    if new_source is None or new_source == source:
        return None  # nothing changed — the finding is stale or unlocatable
    path.write_text(new_source, encoding="utf-8")
    return description


def _callee_params_for_call(repo: Path, rel: str, source: str, line: int) -> list[str] | None:
    """The callee's param names for the call on `line`, resolved repo-wide.
    Mirrors the scanner's Name-callee rule: a method/builtin callee is not
    auto-fixable."""
    module = cst.parse_module(source)
    wrapper = cst.MetadataWrapper(module)
    callee = None

    class _FindCall(cst.CSTVisitor):
        METADATA_DEPENDENCIES = (PositionProvider,)

        @override
        def visit_Call(self, node) -> None:
            nonlocal callee
            if callee is not None:
                return
            pos = self.get_metadata(PositionProvider, node)
            if pos.start.line <= line <= pos.end.line and m.matches(node.func, m.Name()):
                callee = node.func.value

    wrapper.visit(_FindCall())
    if callee is None:
        return None
    return _repo_params(repo, rel, callee)


# --------------------------------------------------------------------------- dispatch-registry


class _BoundNames(cst.CSTVisitor):
    """Names ASSIGNED inside a node — the arm's locals (not free vars)."""

    def __init__(self) -> None:
        self.bound: set[str] = set()

    @override
    def visit_Assign(self, node: cst.Assign) -> None:
        for t in node.targets:
            self.bound.update(_target_names(t.target))

    @override
    def visit_AnnAssign(self, node: cst.AnnAssign) -> None:
        if node.target:
            self.bound.update(_target_names(node.target))

    @override
    def visit_For(self, node: cst.For) -> None:
        self.bound.update(_target_names(node.target))

    @override
    def visit_CompFor(self, node: cst.CompFor) -> None:
        self.bound.update(_target_names(node.target))

    @override
    def visit_With(self, node: cst.With) -> None:
        for item in node.items:
            if item.optional_vars:
                self.bound.update(_target_names(item.optional_vars))


def _target_names(target) -> set[str]:
    """The names a (possibly tuple) assignment target binds."""
    if isinstance(target, cst.Name):
        return {target.value}
    if isinstance(target, (cst.Tuple, cst.List)):
        return {n for elt in target.elements if isinstance(elt, cst.Element) for n in _target_names(elt.value)}
    return set()


# ambient names a dispatch arm may read without a handler parameter: the
# builtins + underscore dunders (the module's imports are a hand-apply case)
_AMBIENT = frozenset({
    "str", "int", "float", "bool", "list", "dict", "set", "tuple", "bytes", "bytearray",
    "len", "sorted", "min", "max", "sum", "any", "all", "range", "enumerate", "zip",
    "map", "filter", "next", "iter", "print", "isinstance", "issubclass", "repr",
    "abs", "round", "format", "hash", "id", "type", "object", "getattr", "setattr",
    "hasattr", "callable", "chr", "ord", "bin", "hex", "oct", "reversed", "slice",
    "Exception", "ValueError", "KeyError", "TypeError", "NotImplementedError",
    "RuntimeError", "None", "True", "False", "open", "staticmethod", "classmethod",
    "property", "super", "self",
})


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


# the dispatch-chain collections — named so the signatures are not bare
# record collections (the record-shape rule's escape hatch)
_DispatchArm = tuple[str, str, list[cst.BaseStatement]]
_DispatchChain = list[tuple[str, list[cst.BaseStatement]]]
_DispatchShape = tuple[list[cst.BaseStatement], _DispatchChain, str, cst.Return]
_DispatchArms = tuple[list | None, str | None]
_BatteryCheck = tuple[cst.BaseExpression, cst.BaseExpression]
_BatteryShape = tuple[str, list[_BatteryCheck], list, list]
_RuleBuild = tuple[list[cst.FunctionDef], list]
_DispatchBuild = tuple[list[cst.FunctionDef], cst.SimpleStatementLine]
_AccInit = tuple[str, int]
_RuleTableBuild = tuple[cst.SimpleStatementLine, cst.SimpleStatementLine]


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


def _dispatch_chain_shape(fn: cst.FunctionDef, body: list) -> _DispatchShape | None:
    """The dispatch-chain shape of a function: a PREAMBLE (locals computed
    before the chain), the >=3 arms over ONE selector, and a single trailing
    `return <default>`. None when the body is not that shape."""
    first_if = next((i for i, s in enumerate(body) if isinstance(s, cst.If)), None)
    if first_if is None:
        return None
    preamble = body[:first_if]
    chain, selector = _dispatch_arms(body[first_if:])
    if chain is None:
        return None
    if len(chain) < 3:
        return None  # >= 3 arms make a registry worth it
    tail = body[first_if + len(chain):]
    if len(tail) != 1 or not isinstance(tail[0], cst.SimpleStatementLine) \
            or not isinstance(tail[0].body[0], cst.Return):
        return None  # v1: a single trailing `return <default>` is the no-match path
    return preamble, chain, selector or "", tail[0].body[0]


def _dispatch_arms(stmts: list) -> _DispatchArms:
    """The contiguous dispatch arms over ONE selector from a statement run —
    (chain, selector) or (None, None) when an arm is not the table shape."""
    chain = []
    selector: str | None = None
    for stmt in stmts:
        if not isinstance(stmt, cst.If):
            break
        parsed = _parse_dispatch_arm(stmt)
        if parsed is None:
            return None, None
        sel, lit, arm_body = parsed
        if selector is None:
            selector = sel
        elif sel != selector:
            return None, None
        chain.append((lit, arm_body))
    return chain, selector


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


def _dispatch_lambda_mode(fn, wrapper, shaped: _DispatchShape, exprs) -> str:
    """The lambda-table rewrite of a single-expression dispatch: `_tools =
    {"lit": lambda: <expr>, ...}` inside the fn (the closures capture the
    scope) + a lookup dispatch. The fn is replaced in place — the table
    needs no module-level additions."""
    preamble, chain, selector, default = shaped
    table = _dispatch_lambda_table(chain, exprs)
    dispatch = _dispatch_lambda_call(selector, default)
    new_fn = fn.with_changes(body=fn.body.with_changes(body=preamble + [table] + dispatch))
    return cst.Module(body=[new_fn if s is fn else s for s in wrapper.module.body]).code


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
    scope = _dispatch_scope_analysis(chain, selector)
    if scope is None:
        return None  # sibling-bound read: not preservable — refuse
    bounds, reads, selector_read = scope
    union: list[str] = []
    for i, _unused in enumerate(chain):
        free = [n for n in reads[i] if n not in bounds[i] and n not in union]
        union.extend(free)
    if selector_read:
        union = [selector] + [v for v in union if v != selector]
    handlers, registry = _dispatch_build(chain, union, _module_names(wrapper))
    dispatch = _dispatch_call(selector, default, union)
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


# per-arm bound + read name analysis result — the fix's scope check
# (refused when sibling-bound reads are detected)
_DispatchScope = tuple[list[set[str]], list[set[str]], bool] | None


def _dispatch_scope_analysis(chain, selector) -> _DispatchScope:
    """Per-arm bound + read name analysis. Returns (bounds, reads, selector_read)
    or None when an arm reads a name bound in a sibling arm (the value does
    not exist at the uniform call site — the rewrite would crash every
    selector instead of only the broken one)."""
    bounds: list[set[str]] = []
    reads: list[set[str]] = []
    for _lit, arm_body in chain:
        bound = _BoundNames()
        for st in arm_body:
            st.visit(bound)
        bounds.append(bound.bound)
        reads.append(set(_read_names(arm_body)))
    for i in range(len(chain)):
        for j, b in enumerate(bounds):
            if i != j and reads[i] & b:
                return None
    return bounds, reads, any(selector in r for r in reads)


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


def _module_names(wrapper: cst.MetadataWrapper) -> set[str]:
    """The top-level def/class names — the collision guard's existing-name
    set for generated handler names."""
    names = set()
    for s in wrapper.module.body:
        if isinstance(s, (cst.FunctionDef, cst.ClassDef)):
            names.add(s.name.value)
    return names


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
        body=[cst.Assign(targets=[cst.AssignTarget(cst.Name("_REGISTRY"))], value=cst.Dict(elements=registry_entries))]
    )
    return handlers, registry


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
            body=[
                cst.Return(
                    cst.Call(func=cst.Name("handler"), args=[cst.Arg(cst.Name(v)) for v in union])
                )
            ]
        ),
    ]


def fix_dispatch_registry(source: str, line: int) -> str | None:
    """A dispatch chain (`if sel == "a": return f()  if sel == "b": ...`)
    is a handler REGISTRY in disguise — each arm is already a named handler.
    Rewrite it to a dict of selector -> handler functions and a one-line
    dispatch, extracting every arm's body as a private function named from
    its literal. The handler signature is the UNION of the arms' free
    variables (minus the selector) so the dispatch call is uniform."""
    module = cst.parse_module(source)
    wrapper = cst.MetadataWrapper(module)
    finder = _FindFnLine(line)
    wrapper.visit(finder)
    fn = finder.found
    if fn is None or not any(s is fn for s in wrapper.module.body):
        return None
    body = list(fn.body.body)
    shaped = _dispatch_chain_shape(fn, body)
    if shaped is None:
        return None
    preamble, chain, selector, default = shaped

    # the LAMBDA TABLE when every arm is a single expression — the pure data
    # form (selector adjacent to its expression, closures capture the scope,
    # no names, no plumbing — the rule-table principle). Multi-statement arms
    # fall back to named handlers (a lambda cannot hold a body).
    exprs = [_arm_single_expression(arm_body) for _lit, arm_body in chain]
    if all(e is not None for e in exprs):
        return _dispatch_lambda_mode(fn, wrapper, shaped, exprs)
    return _dispatch_named_mode(fn, wrapper, shaped)


# --------------------------------------------------------------------------- rule-checks


def _rule_battery_shape(fn: cst.FunctionDef, body: list) -> _BatteryShape | None:
    """The rule-battery shape: `acc = []`, >= 3 `if <cond>: acc.append(<v>)`
    checks (each a single append, no else), then anything as the tail. The
    conditions may read ANY enclosing name — the hoisted lambdas capture the
    scope. Returns (acc, [(cond, value), ...], preamble, tail)."""
    probe = _acc_init(body)
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
        value = _append_value(stmt.body, acc)
        if value is None:
            return None  # v1: one append per check
        checks.append((stmt.test, value))
        idx += 1
    if len(checks) < 3:
        return None  # fewer than 3 checks is not a battery
    # everything after the last check is the TAIL — kept verbatim (the
    # collector comprehension produces `acc`, then the tail uses it)
    return acc, checks, preamble, list(body[idx:])


def _acc_init(body: list) -> _AccInit | None:
    """The `acc = []` opener (plain or annotated `acc: list = []`) — (acc
    name, its index) or None. Preamble statements may precede it (the hoisted
    lambdas capture them)."""
    for i, stmt in enumerate(body):
        if not isinstance(stmt, cst.SimpleStatementLine) or len(stmt.body) != 1:
            continue
        a = stmt.body[0]
        if isinstance(a, cst.Assign) and isinstance(a.value, cst.List) \
                and len(a.value.elements) == 0 \
                and isinstance(a.targets[0].target, cst.Name):
            return a.targets[0].target.value, i
        if isinstance(a, cst.AnnAssign) and isinstance(a.value, cst.List) \
                and len(a.value.elements) == 0 \
                and isinstance(a.target, cst.Name):
            return a.target.value, i
    return None


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
    if not isinstance(call.func, cst.Attribute) \
            or call.func.attr.value != "append" \
            or not isinstance(call.func.value, cst.Name) \
            or call.func.value.value != acc or len(call.args) != 1:
        return None
    return call.args[0].value


def _rule_table_build(acc: str, checks: list) -> _RuleTableBuild:
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
    return table, collector


def fix_rule_table(source: str, line: int) -> str | None:
    """A rule battery (`if cond: violations.append(...)` repeated) is a
    LATENT DATA STRUCTURE — a table of (condition, violation) pairs. Hoist
    it: each check becomes a tuple `(lambda: <cond>, <violation>)`, the
    function collects the violations whose condition holds. The lambdas
    capture the enclosing scope (shared preamble locals need no plumbing)
    and need NO names — the naming problem dissolves. v1: each check is a
    single `acc.append(<value>)` with no else."""
    module = cst.parse_module(source)
    wrapper = cst.MetadataWrapper(module)
    finder = _FindFnLine(line)
    wrapper.visit(finder)
    fn = finder.found
    if fn is None or not any(s is fn for s in wrapper.module.body):
        return None
    body = list(fn.body.body)
    shaped = _rule_battery_shape(fn, body)
    if shaped is None:
        return None
    acc, checks, preamble, tail = shaped
    table, collector = _rule_table_build(acc, checks)
    new_fn = fn.with_changes(body=fn.body.with_changes(body=preamble + [table, collector] + tail))
    # the lambdas live IN the fn — no module-level additions
    out_body: list = [new_fn if stmt is fn else stmt for stmt in wrapper.module.body]
    return cst.Module(body=out_body).code

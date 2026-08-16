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
    "extract-method": "extract the best self-contained seam into a named function (preview, then --confirm)",
    "extract-class": "move the strewing free functions into a class, rewriting call sites",
    "magic-number": "Replace Magic Literal: introduce the named constant",
    "vague-name": "Rename the type and its references (same-file)",
    "long-param-list": "Introduce Parameter Object: bundle the params into a dataclass",
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

    originals = _collect_defs(module, set(fns))
    receiver_name = (
        originals[0].params.params[0].name.value
        if originals and originals[0].params.params
        else None
    )

    # pass 1: rewrite the call sites WITHOUT deleting — the moved bodies must
    # be collected from this pass so their inter-fn calls are method calls
    # (`_window_score(state, ...)` -> `state._window_score(...)`)
    call_rewritten = wrapper.visit(_MoveIntoClass(set(fns), delete=False)).code

    # pass 2: collect the moved fns from the rewritten module, drop the
    # receiver param (becomes `self`), and rename the old receiver name in
    # the bodies — `state._window_score(...)` -> `self._window_score(...)`
    methods = _collect_defs(cst.parse_module(call_rewritten), set(fns))
    new_methods = []
    for method in methods:
        params = method.params.params
        rest = params[1:] if params else []
        body = method.body
        if receiver_name is not None:
            body = body.visit(_RenameName(receiver_name, "self"))
        new_methods.append(
            method.with_changes(
                params=method.params.with_changes(
                    params=[cst.Param(name=cst.Name("self"), annotation=None), *rest]
                ),
                body=body,
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
    """Replace the target block with a call to the new function."""

    METADATA_DEPENDENCIES = (PositionProvider,)

    def __init__(self, fn_name: str, fn_line: int, block_indices: set[int], new_name: str,
            free_vars: list[str],
        ) -> None:
        self.fn_name = fn_name
        self.fn_line = fn_line
        self.block_indices = block_indices
        self.new_name = new_name
        self.free_vars = free_vars

    @override
    def leave_FunctionDef(self, original_node, updated_node):
        if updated_node.name.value != self.fn_name:
            return updated_node
        if self.get_metadata(PositionProvider, original_node).start.line != self.fn_line:
            return updated_node
        kept = []
        inserted_call = False
        for idx, stmt in enumerate(updated_node.body.body):
            if idx in self.block_indices:
                if not inserted_call:
                    kept.append(
                        cst.SimpleStatementLine(
                            body=[
                                cst.Expr(
                                    cst.Call(
                                        func=cst.Name(self.new_name),
                                        args=[cst.Arg(cst.Name(v)) for v in self.free_vars],
                                    )
                                )
                            ]
                        )
                    )
                    inserted_call = True
            else:
                kept.append(stmt)
        return updated_node.with_changes(body=updated_node.body.with_changes(body=kept))


class _InsertExtractedFn(cst.CSTTransformer):
    """Insert the extracted function after its source function, in the same
    container (module or class body)."""

    METADATA_DEPENDENCIES = (ParentNodeProvider,)

    def __init__(self, fn_name: str, fn_line: int, new_def: cst.FunctionDef) -> None:
        self.fn_name = fn_name
        self.fn_line = fn_line
        self.new_def = new_def
        self.done = False

    def _maybe_insert(self, body: list) -> list:
        if self.done:
            return body
        out = []
        for stmt in body:
            out.append(stmt)
            if not self.done and isinstance(stmt, cst.FunctionDef) and stmt.name.value == self.fn_name:
                out.append(self.new_def)
                self.done = True
        return out

    @override
    def leave_Module(self, original_node, updated_node):
        return updated_node.with_changes(body=self._maybe_insert(list(updated_node.body)))

    @override
    def leave_ClassDef(self, original_node, updated_node):
        return updated_node.with_changes(
            body=updated_node.body.with_changes(body=self._maybe_insert(list(updated_node.body.body)))
        )


class _FnBodyState:
    """The extract-method analysis state: the target function's body
    statements with per-statement spans, first-use contexts, writes, and
    control-flow flags."""

    def __init__(self, line: int) -> None:
        self.line = line
        self.fn_node: cst.FunctionDef | None = None
        self.stmt_ids: list[int] = []
        self.stmt_spans: dict[int, tuple[int, int]] = {}
        self.first_use: dict[int, dict[str, str]] = {}
        self.writes: dict[int, set[str]] = {}
        self.control_flow: dict[int, bool] = {}

    def _window_score(self, i: int, j: int, min_lines: int):
        """Score one candidate window: (free_count, -span, start) when it is
        a safe seam, or None. Free = names whose first use in the window is a
        read (builtins excluded); out-variables and control-flow exits
        disqualify."""
        if i == 0 and j == len(self.stmt_ids) - 1:
            return None  # extracting the whole body is not a refactoring
        if any(self.control_flow[self.stmt_ids[k]] for k in range(i, j + 1)):
            return None  # a return/break inside the seam changes control flow
        span = self.stmt_spans[self.stmt_ids[j]][1] - self.stmt_spans[self.stmt_ids[i]][0] + 1
        if span < min_lines:
            return None
        free: set[str] = set()
        seen: set[str] = set()
        writes_all: set[str] = set()
        for k in range(i, j + 1):
            sid = self.stmt_ids[k]
            for name, ctx in self.first_use[sid].items():
                if name not in seen:
                    seen.add(name)
                    if ctx == "read":
                        free.add(name)
            writes_all |= self.writes[sid]
        free -= _BUILTINS
        if self._window_has_outvars(j, writes_all) or not free:
            return None  # out-variable or no-input seam — skip
        return len(free), -span, i, sorted(free)

    def _window_has_outvars(self, j: int, writes_all: set[str]) -> bool:
        """Does any name written in the window get read after it?
        Out-variables would need a return value — the seam must be
        self-contained."""
        after: set[str] = set()
        for k in range(j + 1, len(self.stmt_ids)):
            for name, ctx in self.first_use[self.stmt_ids[k]].items():
                if ctx == "read":
                    after.add(name)
        return bool(writes_all & after)

    def best_seam(self, min_lines: int = 2):
        """The window whose free variables are the smallest subset of the
        function's variables (builtins excluded — they are available in any
        scope) and whose out-variables are empty. A name is free iff its
        FIRST use in the window is a read. Ties go to the larger window.
        Returns (block_ids, free_vars) or None. Out-variables are a smell —
        the seam must be self-contained; the whole body is not a seam."""
        n = len(self.stmt_ids)
        best = None  # (free_count, -lines, start, block_ids, free_vars)
        for i in range(n):
            for j in range(i, n):
                score = self._window_score(i, j, min_lines)
                if score is not None and (best is None or score < best[0]):
                    best = (score, list(range(i, j + 1)))
        if best is None:
            return None
        (_, _, _, free_vars), block_ids = best
        return list(block_ids), free_vars


class _Analyse(cst.CSTVisitor):
    """Collects the target function's body statement data. Module-level so
    the latent-class rule does not fire on the enclosing analysis function."""

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
        for s in node.body.body:
            p = self.get_metadata(PositionProvider, s)
            sid = id(s)
            self.state.stmt_spans[sid] = (p.start.line, p.end.line)
            self.state.stmt_ids.append(sid)
            self.state.first_use[sid] = {}
            self.state.writes[sid] = set()
            self.state.control_flow[sid] = isinstance(s, cst.SimpleStatementLine) and any(
                isinstance(st, (cst.Return, cst.Break, cst.Continue, cst.Raise, cst.Yield))
                for st in s.body
            )

    @override
    def visit_Name(self, node) -> None:
        if self.state.fn_node is None:
            return
        sid = None
        parent = self.get_metadata(ParentNodeProvider, node)
        while parent is not None and not isinstance(parent, cst.Module):
            if parent is self.state.fn_node:
                break
            if id(parent) in self.state.stmt_ids:
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
    return module, wrapper, state


_BUILTINS = frozenset(dir(builtins))





def extract_method_proposal(source: str, line: int, name: str):
    """Compute the best extraction seam and the resulting source, WITHOUT
    writing. Returns (new_source, seam_text) or (None, None) when no safe
    seam exists."""
    module, wrapper, state = _fn_seam_analysis(source, line)
    if state.fn_node is None or len(state.stmt_ids) < 2:
        return None, None
    seam = state.best_seam()
    if seam is None:
        return None, None
    block_indices, free_vars = seam
    block_sids = [state.stmt_ids[i] for i in block_indices]
    first_span = state.stmt_spans[block_sids[0]]
    new_def = cst.FunctionDef(
        name=cst.Name(name),
        params=cst.Parameters(params=[cst.Param(cst.Name(v)) for v in free_vars]),
        body=cst.IndentedBlock(
            body=[s for s in state.fn_node.body.body if id(s) in set(block_sids)]
        ),
        returns=None,
    )
    replaced = wrapper.visit(
        _ExtractMethodRewrite(state.fn_node.name.value, line, set(block_indices), name, free_vars)
    ).code
    inserted = cst.MetadataWrapper(cst.parse_module(replaced)).visit(
        _InsertExtractedFn(state.fn_node.name.value, line, new_def)
    ).code
    seam_text = source.splitlines()[first_span[0] - 1] if source else ""
    return inserted, f"line {first_span[0]}: {seam_text.strip()}"


def fix_extract_method(source: str, line: int, name: str) -> str | None:
    """Extract Function (applied): the best self-contained seam of the
    function at `line` becomes a new function named `name`."""
    new_source, _ = extract_method_proposal(source, line, name)
    return new_source


def propose_finding(kind: str, rel: str, repo: Path, line: int, opts: FixOptions | None = None):
    """Compute the fix WITHOUT writing — the preview surface. Returns
    (new_source, description) or (None, None) when nothing changes."""
    opts = opts or FixOptions()
    kind = KIND_ALIASES.get(kind, kind)
    path = repo / rel
    source = path.read_text(encoding="utf-8")
    if kind == "extract-method":
        if opts.name is None:
            return None, None
        new_source = fix_extract_method(source, line, opts.name)
        return (new_source, STRUCTURAL_KINDS["extract-method"]) if new_source else (None, None)
    return None, None


class _RenameName(cst.CSTTransformer):
    """Rename every Name node — the extract-class receiver rename."""

    def __init__(self, old: str, new: str) -> None:
        self.old = old
        self.new = new

    @override
    def leave_Name(self, original_node, updated_node):
        if updated_node.value == self.old:
            return updated_node.with_changes(value=self.new)
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


class _BodyParamRewrite(cst.CSTTransformer):
    """Rewrite body references of the bundled params to options.<param>."""

    def __init__(self, params: list[str]) -> None:
        self.params = set(params)

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
            func=cst.Attribute(
                value=cst.Name(self.class_name),
                attr=cst.Name(self.fn),
                dot=cst.Dot(),
            ),
            args=new_args,
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
    return None


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

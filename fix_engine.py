"""The auto-fix engine — libcst transforms for mechanical findings.

A finding carries (kind, file, line); each mechanical kind has a lossless
transform (libcst round-trips comments and formatting). The agent workflow:

    code_health.py fix --kind stale-suppression --file x.py --line 12

The transform edits the working tree; the gate re-run confirms the finding
is gone. libcst is optional (extra `fix`) — without it the engine reports
the missing dependency and exits non-zero; nothing is half-applied.

Transforms are deliberately MINIMAL: only the finding's node changes. The
structural tier (extract-class, split-function) needs a name and is not
here — those are agent-furnished (`--name`) and hand-verified.
"""

from __future__ import annotations

from pathlib import Path
from typing import override

import libcst as cst
import libcst.matchers as m
from libcst.metadata import PositionProvider

MECHANICAL_KINDS = {
    "stale-suppression": "delete the stale code-health: ignore comment",
    "noop-statement": "delete the dead statement",
    "unreachable": "delete the unreachable statement",
    "positional-literals": "keyword the literal arguments (same-file callee)",
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
    """Remove the `code-health: ignore` comment on the target line — both the
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
        if "code-health: ignore" not in updated_node.comment.value:
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
        if self.deleted or "code-health: ignore" not in updated_node.value:
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


# --------------------------------------------------------------------------- the fix surface

def fix_finding(kind: str, rel: str, repo: Path, line: int, params: list[str] | None = None) -> str | None:
    """Apply the transform for one finding. Returns a human description of
    what changed, or None when the finding was already gone (no edit).

    `params` (the callee's parameter names) lets the agent supply the semantic
    bit for external/unresolved callees — the tool does the mechanical edit
    across every finding; the agent reads the signature once."""
    path = repo / rel
    source = path.read_text(encoding="utf-8")
    if kind in ("noop-statement", "unreachable"):
        transformer = _DeleteStatement(line)
    elif kind == "stale-suppression":
        transformer = _DeleteComment(line)
    elif kind == "positional-literals":
        if params is None:
            params = _callee_params_for_call(repo, rel, source, line)
        if params is None:
            return None  # callee not resolvable — skip, no edit
        transformer = _KeywordArgs(line, params)
    else:
        raise ValueError(f"kind '{kind}' has no mechanical fix (structural fixes need --name)")
    wrapper = cst.MetadataWrapper(cst.parse_module(source))
    result = wrapper.visit(transformer)
    new_source = result.code
    if new_source == source:
        return None  # nothing changed — the finding is stale or unlocatable
    path.write_text(new_source, encoding="utf-8")
    return MECHANICAL_KINDS.get(kind, "applied")


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

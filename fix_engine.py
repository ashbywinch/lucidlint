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
import keyword
import re
from collections.abc import Sequence
from dataclasses import dataclass, field
from pathlib import Path
from typing import NamedTuple, TypeGuard, cast, override

import libcst as cst
import libcst.matchers as m
from libcst.metadata import CodePosition, CodeRange, ExpressionContextProvider, ParentNodeProvider, PositionProvider


def _as_range(pos: object) -> CodeRange:
    """The resolved metadata position. libcst's get_metadata types its result
    through an UNDEFINED sentinel default — narrow to the CodeRange that is
    always present for nodes visited inside a MetadataWrapper."""
    assert isinstance(pos, CodeRange)
    return pos


def _as_node(node: object) -> cst.CSTNode:
    """The resolved metadata node — the same UNDEFINED-sentinel widening as
    _as_range, for providers that return nodes (ParentNodeProvider)."""
    assert isinstance(node, cst.CSTNode)
    return node


MECHANICAL_KINDS = {
    "stale-suppression": "delete the stale lucidlint: ignore comment",
    "noop-statement": "delete the dead statement",
    "unreachable": "delete the unreachable statement",
    "positional-literals": "keyword the literal arguments (same-file callee)",
    "duplicate-def": "delete the unreferenced shadowing module-scope def (renames stay with the agent)",
    "restating-docstring": "delete the docstring that restates the body",
    "duplicate-block": "delete the second copy of the repeated statement block",
    "undeclared-attribute": "annotate the undeclared member: self.x: T = v (T inferred from the value)",
    "loop-pipeline": "Replace Loop with Pipeline: the single-mutation loop becomes a comprehension",
    "loop-sequence": (
        "Replace Loop Sequence with Pipeline: the shared-accumulator loop chain "
        "becomes concatenated comprehensions"
    ),
    "loop-hoist": (
        "Replace Loop with Hoist + Pipeline: the loop body becomes a per-item helper, "
        "the loop becomes a comprehension"
    ),
}


@dataclass
class FixOptions:
    """The agent-supplied bits a fix may need: the callee signature for
    positional-literals, the class name for extract-class."""

    params: list[str] | None = None
    name: str | None = None
    # the callee whose --params these are (external callees): binds the fix to
    # THAT call on nested lines instead of the innermost spanning one
    callee: str | None = None


# the structural fixers by kind — a dispatch registry: each arm's handler is
# already a named method; the dispatch is a table lookup, not an if-chain
_STRUCTURAL_FIXERS = {
    "extract-method": "fix_extract_method",
    "extract-class": "fix_extract_class",
    # the three shape-routed extract-class arms: the finding's SHAPE selects
    # the transform (a wide tuple -> a record, a shared param pair -> a
    # parameter object, a field-disjoint class -> a split), so no semantic
    # name is required — the name defaults from the shape and --name overrides.
    "wide-tuple": "fix_extract_class",
    "data-clump": "fix_extract_class",
    "partition": "fix_extract_class",
    "magic-number": "fix_magic_literal",
    "vague-name": "fix_rename",
    "long-param-list": "fix_parameter_object",
    "dispatch-registry": "fix_dispatch_registry",
    "rule-table": "fix_rule_table",
    "tuple-record": "fix_tuple_record",
    "extract-record-class": "fix_extract_record_class",
    "feature-envy": "fix_feature_envy",
    # the #27/#30/#31 house fixers — multi-file transactions (R8)
    "dissolve-husk": "fix_dissolve_husk",
    "collapse-chain": "fix_collapse_chain",
    "split-module": "fix_split_module",
}
_NAME_REQUIRED_KINDS = {
    "magic-number",
    "vague-name",
    "long-param-list",
    "tuple-record",
    "feature-envy",
    "extract-record-class",
    "extract-module",
    "loop-hoist",
}
# Every kind the fix command can actually apply. A finding message may
    # restrict advertising to what the fix command can actually apply: a
    # finding message may advertise "— fix: <kind>" ONLY when the kind is
    # here — suggesting a fix that cannot apply sends agents hand-editing
    # around working automation, or guessing at a seam. (Rust-file kinds are
    # guarded by the Rust binary's own refusal; Python kinds refused by the
    # gate's unknown-kind guard.)
FIXABLE_KINDS = set(MECHANICAL_KINDS) | set(_STRUCTURAL_FIXERS) | {"extract-module"}


class _ModuleProposal(NamedTuple):
    """The extract-module split result: the new origin source and the new
    module source — a named record instead of a bare tuple."""

    origin: str
    module: str


# lucidlint: ignore-file god-class the fix engine is ONE responsibility — 20
# lucidlint: ignore-file global-state the module constants are this tool's configuration tables —
# lucidlint: ignore-file global-state a config class is the eventual home
# lucidlint: ignore-file misplaced-method the repo helpers serve module-level callers too — moving them
# lucidlint: ignore-file misplaced-method onto _FixRequest would strand half
# lucidlint: ignore-file record-shape the libcst layer's tuple/dict shorthands ARE its wire records —
# cohesive methods over the request state; the partition rule finds no
# lucidlint: ignore-file record-shape a class per helper hop is ceremony
# field-disjoint split, so the size is a review signal, not a split order
@dataclass
class _FixRequest:
    """The whole fix context: which file (repo/rel), what kind, where
    (line), with what options, and the content. The (rel, repo), (line,
    source) and (opts, source) pairs travel together — a parameter object
    holds them; the entry fills `source` from the file when not supplied."""

    kind: str
    repo: Path
    rel: str
    line: int
    opts: FixOptions
    max_decisions: int | None = None
    source: str | None = None
    wrote: bool = False        # set by fix_finding: the file was actually rewritten
    decline: str | None = None  # set by fix_finding/fixers: why nothing was written
    col: int = 0  # schema-3 anchor column; disambiguates same-line twins
    # Multi-file transactions (R8): the fixer's OTHER writes beyond the
    # origin — (repo-relative path, new source) — and files to delete.
    # Applied by fix_finding after the origin write; the orchestrator then
    # verifies via the REPO-WIDE scan that the finding kind is gone.
    extra_writes: list[tuple[str, str]] = field(default_factory=list)
    deletes: list[str] = field(default_factory=list)

    def _loaded_source(self) -> str:
        """The file's text. propose_finding/fix_finding fill `source` from the
        file before any fix_* runs; a None here means dispatch was bypassed,
        and there is nothing to transform."""
        assert isinstance(self.source, str)
        return self.source

    def fix_extract_class(self) -> str | None:
        # the shape-routed arms: the finding's SHAPE (recovered from the
        # source at the anchor line — the wire record carries only
        # kind/file/line) selects the transform through this same entry point
        if self.kind == "wide-tuple":
            return _wide_tuple_fix(self)
        if self.kind == "data-clump":
            return _data_clump_fix(self)
        if self.kind == "partition":
            return _partition_fix(self)
        source, line = self._loaded_source(), self.line
        name = self.opts.name
        """Move the strewing group into a class named `name` (default: the shared
        leading type). Returns the new source, or None when the finding is stale
        or the group is not auto-fixable."""
        module = cst.parse_module(source)
        wrapper = cst.MetadataWrapper(module)
        found = _strewing_group(source, line)
        if found is None:
            if self.kind == "extract-class":
                # the wide-tuple/data-clump/partition directives ALSO name
                # extract-class (one entry point per family), and nothing but
                # the anchor's shape can tell them apart — try each arm in turn
                for arm in (_wide_tuple_fix, _data_clump_fix, _partition_fix):
                    self.decline = None
                    shaped = arm(self)
                    if shaped is not None:
                        return shaped
                self.decline = None  # a probe's reason is not this finding's
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
        receivers = {m.name.value: (m.params.params[0].name.value if m.params.params else None) for m in methods}
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

    def fix_parameter_object(self) -> str | None:
        source, line = self._loaded_source(), self.line
        name = self.opts.name
        # the dispatch gate (_fix_structural) refuses name-required kinds without one
        assert isinstance(name, str)
        """Introduce Parameter Object: bundle the function's params into a
        dataclass named `name`, change the signature to `options: name`, rewrite
        the body references and same-file call sites."""
        module = cst.parse_module(source)
        wrapper = cst.MetadataWrapper(module)
        target = _find_fn_at(wrapper, line)
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
        if total and all_params[0].name is not None and all_params[0].name.value in ("self", "cls"):
            total -= 1
        if total <= 5:
            return None  # not the long-param-list shape (threshold is > 5)
        params = [p for p in all_params if p.name is not None and p.name.value not in ("self", "cls")]
        param_names = [p.name.value for p in params]
        renamed = wrapper.visit(_FnBodyRewrite(target.name.value, line, param_names, name)).code
        call_fixed = cst.parse_module(renamed).visit(_CallSiteRewrite(target.name.value, param_names, name)).code
        module3 = cst.parse_module(call_fixed)
        body = list(module3.body)
        first = next(
            (i for i, s in enumerate(body) if isinstance(s, (cst.FunctionDef, cst.ClassDef))),
            len(body),
        )
        body.insert(first, _dataclass_def(name, params))
        return module3.with_changes(body=_ensure_dataclasses_import(body)).code

    def extract_method_proposal(self):
        source, line = self._loaded_source(), self.line
        name = self.opts.name
        # the _FixRequest refactor dropped the caller-supplied 13 — the
        # bound is the contract (an extracted helper must not still be
        # complex); restore it as the default (review bot)
        max_decisions = self.max_decisions if self.max_decisions is not None else 13
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
        module, wrapper, state = self._fn_seam_analysis()
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
        inserted = (
            cst.MetadataWrapper(cst.parse_module(replaced))
            .visit(_InsertExtractedFn(state.fn_node.name.value, line, new_def))
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

    def fix_magic_literal(self) -> str | None:
        source, line = self._loaded_source(), self.line
        name = self.opts.name
        # the dispatch gate (_fix_structural) refuses name-required kinds without one
        assert isinstance(name, str)
        """Replace Magic Literal: `f(10, ...)` -> `f(MAX_RETRIES, ...)` with
        `MAX_RETRIES = 10` inserted as a CLASS ATTRIBUTE of the enclosing
        class (third-pass 4: a module-top constant would be its own
        global-state finding — values are a class's private internals).
        A literal with no class to own its constant (module-level function)
        is REFUSED, never parked at module top."""
        module = cst.parse_module(source)
        wrapper = cst.MetadataWrapper(module)
        value: str | None = None

        finder = _FindLiteral(line, self.col)
        wrapper.visit(finder)
        if finder.value is None:
            return None
        value = finder.value
        replaced = wrapper.visit(_ReplaceLiteral(line, self.col, name)).code
        if replaced == source:
            return None
        module2 = cst.parse_module(replaced)
        # the refusal probe must run on a class OWNING the literal — the
        # replaced tree has the same line map, so one probe serves both
        probe = _ClassOwningLine(line)
        cst.MetadataWrapper(module2).visit(probe)
        if probe.owner is None:
            # no class to own the constant — refuse loudly (never module top)
            self.decline = (
                f"the literal at {self.rel}:{line} has no class to own its constant — "
                "move the function into a class first"
            )
            return None
        assignment = cst.SimpleStatementLine(
            body=[
                cst.Assign(
                    targets=[cst.AssignTarget(cst.Name(name))],
                    value=cst.parse_expression(value),
                )
            ]
        )
        wrapped = cst.MetadataWrapper(module2)
        result = wrapped.visit(_InsertClassConstant(line, assignment))
        return result.code

    def fix_rename(self) -> str | None:
        source, line = self._loaded_source(), self.line
        name = self.opts.name
        # the dispatch gate (_fix_structural) refuses name-required kinds without one
        assert isinstance(name, str)
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
                if old is None and _as_range(self.get_metadata(PositionProvider, node)).start.line == line:
                    old = node.name.value

        wrapper.visit(_Find())
        if old is None or old == name:
            return None
        renamed = wrapper.visit(_RenameClass(line, old, name)).code
        return None if renamed == source else renamed

# lucidlint: ignore closures the nested visitors are one-purpose probe walks — hoisting names nothing the domain owns
    def fix_dissolve_husk(self) -> str | None:
        source, line = self._loaded_source(), self.line
        """Dissolve a delegating-husk (#27): every method is exactly
        `return F(<all params>)` with F a module-level function. REWIRES the
        stateless call sites to the module functions (`Husk().m(x)` /
        `h.m(x)` -> `m(x)`), deletes the husk, and refuses when any reference
        or constructed instance escapes (imports, isinstance, annotations,
        subclassing, stored/passed instances) — never inlines."""
        module = cst.parse_module(source)
        wrapper = cst.MetadataWrapper(module)

        class _FindHusk(cst.CSTVisitor):
            METADATA_DEPENDENCIES = (PositionProvider,)

            def __init__(self, target_line):
                self.target_line = target_line
                self.cls: cst.ClassDef | None = None

            @override
            def visit_ClassDef(self, node):
                if (
                    self.cls is None
                    and _as_range(self.get_metadata(PositionProvider, node)).start.line == self.target_line
                ):
                    self.cls = node

        probe = _FindHusk(line)
        wrapper.visit(probe)
        cls = probe.cls
        if cls is None:
            return None  # stale anchor — R28 silence
        methods = [s for s in cls.body.body if isinstance(s, cst.FunctionDef)]
        if not methods:
            return None
        method_fns: dict[str, str] = {}
        for method in methods:
            callee = _forwarder_callee_of(method)
            if callee is None:
                return None  # not a pure forwarder — not the husk shape
            method_fns[method.name.value] = callee
        husk = cls.name.value
        # repo-wide analysis: every file's references must be rewireable
        # (direct `Husk().m(...)` calls or a `h = Husk()` + `h.m(...)` pattern)
        files = _py_files(self.repo)
        new_sources: dict[str, str] = {}
        for path in files:
            rel = path.relative_to(self.repo).as_posix()
            fs = path.read_text(encoding="utf-8")
            mod = cst.parse_module(fs)
            rewired = _rewire_husk_sites(mod, husk, method_fns)
            if rewired is None:
                self.decline = (
                    f"a reference to '{husk}' (or a constructed instance) escapes the rewireable "
                    "call pattern — imports, isinstance, annotations, subclassing, or a stored/passed "
                    "instance; dissolve by hand"
                )
                return None
            if rewired != fs:
                new_sources[rel] = rewired
        # build the origin's new source: removed husk class + rewired sites
        origin_rewired = new_sources.pop(self.rel, source)
        origin_mod = cst.parse_module(origin_rewired)
        removed = _RemoveHuskClass(husk)
        origin_new = origin_mod.visit(removed).code
        if origin_new == source and not new_sources:
            return None  # nothing rewired and nothing deleted — stale
        for rel, ns in new_sources.items():
            self.extra_writes.append((rel, ns))
        return origin_new

# lucidlint: ignore assembly-class the chain facts thread through module helpers — _FixRequest is the boundary
# lucidlint: ignore closures the nested visitors are one-purpose probe walks — hoisting names nothing owned
    def fix_collapse_chain(self) -> str | None:
        source, line = self._loaded_source(), self.line
        """Collapse a forwarding chain (#30): method M (class C) whose body is
        exactly `return F(<all params>)`, module fn F whose body is exactly
        `return N(<all F params>)`, N a method on ANOTHER class D (or a
        module fn at depth >= 2). REWIRE M to reach D.N directly and delete F
        when D is real (has state or non-forwarding members); PROMOTE N to a
        sibling method of M when D has no identity and dissolves. REFUSES
        when the modules are independent (the deliberate boundary), when F
        has other callers, on *args/**kwargs passthrough, on a
        `from mod import F` re-export, or when N has other callers — never
        blind inlining."""
        module = cst.parse_module(source)
        wrapper = cst.MetadataWrapper(module)

        class _FindMethod(cst.CSTVisitor):
            METADATA_DEPENDENCIES = (PositionProvider,)

            def __init__(self, target_line):
                self.target_line = target_line
                self.owner: tuple[str, cst.ClassDef, cst.FunctionDef] | None = None

            @override
            def visit_ClassDef(self, node):
                for stmt in node.body.body:
                    if not isinstance(stmt, cst.FunctionDef):
                        continue
                    pos = _as_range(self.get_metadata(PositionProvider, stmt))
                    if pos.start.line == self.target_line:
                        self.owner = (node.name.value, node, stmt)
                        return

        probe = _FindMethod(line)
        wrapper.visit(probe)
        if probe.owner is None:
            return None  # stale or not a method — R28 silence
        c_name, cls_node, method = probe.owner
        head_params = [p.name.value for p in method.params.params if p.name.value not in ("self", "cls")]
        head_callee, head_args = _pure_forward_of(method)
        if head_callee is None or head_args != head_params:
            return None  # M is not a pure forwarder to a module fn
        if not head_callee.is_name:
            return None
        # find F's definition repo-wide
        f_name = head_callee.name
        f_holder = _module_fn_definition(self.repo, f_name)
        if f_holder is None:
            return None
        f_rel, f_module = f_holder
        f_fn = _module_fn_node(f_module, f_name)
        if f_fn is None:
            return None
        f_params = [p.name.value for p in f_fn.params.params]
        target = _pure_forward_of(f_fn)
        if target is None:
            return None  # F does not forward — not a chain
        mid_callee, mid_args = target
        if not isinstance(mid_callee, _ForwardTarget):
            return None
        # a method callee absorbs the receiver param (`store.load(key)` passes
        # only `key`); a module-fn callee takes all params
        expected_args = (
            [p for p in f_params if p != mid_callee.attr_method[0]]
            if mid_callee.attr_method is not None
            else f_params
        )
        if mid_args != expected_args:
            return None
        # +1 caller check for F: the ONLY call must be M's
        if _call_sites(self.repo, f_name) != 1:
            self.decline = f"'{f_name}' has more than one caller — collapse by hand"
            return None
        if any("*" in p for p in f_params) or any("*" in p for p in head_params):
            self.decline = "the chain passes *args/**kwargs — collapse by hand"
            return None
        if _reexported(self.repo, f_name):
            self.decline = f"'from ... import {f_name}' re-exports the forwarder — collapse by hand"
            return None
        if mid_callee.is_name:
            # M -> F -> G at depth >= 2: G is a module fn terminal
            if not _depends_on(self.repo, self.rel, f_rel):
                self.decline = "the chain is the only coupling between the modules — a boundary; leave it"
                return None
            body = cst.SimpleStatementLine(
                body=[
                    cst.Return(
                        cst.Call(
                            func=cst.Name(mid_callee.name),
                            args=[cst.Arg(cst.Name(p)) for p in head_params],
                        )
                    )
                ]
            )
            origin_new = _replace_method_body(module, method, body).code
            self.extra_writes.append((f_rel, _remove_fn(f_module, f_name)))
            return origin_new
        # N is a method on another class D — resolve uniquely
        if mid_callee.attr_method is None:
            return None
        recv, n_name = mid_callee.attr_method
        arg_params = [p for p in f_params if p != recv]
        candidates = _methods_named(self.repo, n_name)
        matches = [
            (d_rel, d_cls, d_method)
            for d_rel, d_cls, d_method in candidates
            if d_cls != c_name
            and [p.name.value for p in d_method.params.params if p.name.value not in ("self", "cls")] == arg_params
        ]
        if len(matches) != 1:
            return None  # ambiguous — no structural certainty
        d_rel, d_cls, d_method = matches[0]
        if not _depends_on(self.repo, self.rel, d_rel):
            self.decline = "the chain is the only coupling between the modules — a boundary; leave it"
            return None
        if _call_sites(self.repo, n_name) > 1:
            self.decline = f"'{n_name}' has other callers beyond the chain — collapse by hand"
            return None
        d_has_state = _class_has_identity(d_cls)
        if d_has_state:
            # REWIRE: M reaches D.N directly; F deleted
            body = cst.SimpleStatementLine(
                body=[
                    cst.Return(
                        cst.Call(
                            func=cst.Attribute(value=cst.Name(recv), attr=cst.Name(n_name)),
                            args=[cst.Arg(cst.Name(p)) for p in head_params if p != recv],
                        )
                    )
                ]
            )
            origin_new = _replace_method_body(module, method, body, line).code
            if f_rel == self.rel:
                origin_new = cst.parse_module(origin_new).visit(_RemoveFn(f_name)).code
            else:
                self.extra_writes.append((f_rel, _remove_fn(f_module, f_name)))
            return origin_new
        # PROMOTE: N becomes a sibling method of M on C; D dissolves
        if not _zero_external_refs(self.repo, d_cls.name.value):
            self.decline = f"'{d_cls.name.value}' still has external references — dissolve D by hand"
            return None
        if _method_uses_self(d_method):
            self.decline = f"'{n_name}' touches its own instance state — promote by hand"
            return None
        promoted = d_method.with_changes(name=cst.Name(n_name))
        new_class = cls_node.with_changes(
            body=cls_node.body.with_changes(
                body=[*cls_node.body.body, promoted]
            )
        )
        origin_new = _replace_class_body(module, line, new_class).code
        # M's body becomes `return self.N(<args minus the recv param>)`
        m_body = cst.SimpleStatementLine(
            body=[
                cst.Return(
                    cst.Call(
                        func=cst.Attribute(value=cst.Name("self"), attr=cst.Name(n_name)),
                        args=[cst.Arg(cst.Name(p)) for p in head_params if p != recv],
                    )
                )
            ]
        )
        origin_new = _replace_method_body(cst.parse_module(origin_new), method, m_body, line).code
        if f_rel == self.rel:
            origin_new = cst.parse_module(origin_new).visit(_RemoveFn(f_name)).code
        else:
            self.extra_writes.append((f_rel, _remove_fn(f_module, f_name)))
        if d_rel != self.rel:
            d_new = _remove_class(_module_of(self.repo, d_rel), d_cls.name.value)
            if _module_empty(d_new):
                self.deletes.append(d_rel)
            else:
                self.extra_writes.append((d_rel, d_new))
        else:
            # D dissolves in the origin too (zero external refs by the gate)
            origin_new = cst.parse_module(origin_new).visit(_RemoveClass(d_cls.name.value)).code
        return origin_new

# lucidlint: ignore closures the nested visitors are one-purpose probe walks — hoisting names nothing the domain owns
    def fix_split_module(self) -> str | None:
        """Split a multi-class module (#31): the misplaced public classes move
        to files named after them — flat siblings when independent, a package
        when cohesive; the residual (module functions, ownerless constants,
        private classes, the stem-matching class) stays in the origin (FLAT)
        or in `mod/__init__.py` (PACKAGE); an origin that becomes empty is
        deleted. REFUSES (self.decline) on target collisions, import cycles
        in the produced layout, and executable statements between the
        classes; package layouts are refused when a module-level constant
        cannot move into a class (the __init__ holds no constants, BQ6), and
        relative imports in the origin refuse the package layout and fall
        the split back to flat."""
        rel = self.rel
        if not rel.endswith(".py") or rel.endswith("__init__.py"):
            return None  # the scanner never flags __init__ — nothing to split
        source = self._loaded_source()
        module = cst.parse_module(source)
        wrapper = cst.MetadataWrapper(module)
        # MetadataWrapper deep-copies the tree — every node MUST come from the
        # wrapper's copy or ParentNodeProvider lookups miss (the fixers hold
        # nodes across transforms; identity must stay within one tree)
        module = wrapper.module
        top = module.body
        origin_dir = rel.rsplit("/", 1)[0] if "/" in rel else ""
        dirs = origin_dir.split("/") if origin_dir else []
        in_package = bool(dirs)
        stem = rel.rsplit("/", 1)[-1][:-3]
        subjects = [
            s
            for s in top
            if isinstance(s, cst.ClassDef)
            and not s.name.value.startswith("_")
            and not _class_matches_stem(s.name.value, stem)
        ]
        if not subjects:
            return None  # stale — no misplaced public class remains
        subject_ids = {id(s) for s in subjects}
        subject_names = {s.name.value for s in subjects}
        origin_path = dirs + [stem]

        # REFUSAL: module-level executable statements BETWEEN the first and
        # last subject class would be reordered by the split (their original
        # position sits relative to the classes) — decline, never reorder
        bounds = [i for i, s in enumerate(top) if id(s) in subject_ids]
        for stmt in top[bounds[0] : bounds[-1] + 1]:
            if id(stmt) in subject_ids or isinstance(stmt, cst.EmptyLine):
                continue
            if isinstance(stmt, (cst.ClassDef, cst.FunctionDef)):
                continue
            if _stmt_is_plain_assign(stmt):
                continue
            self.decline = (
                "module-level executable statements between classes would be "
                "reordered by the split — split by hand"
            )
            return None

        # the constants the split can move: module-level assignments to plain
        # names, and their readers (per subject class / per residual statement)
        const_stmts = _module_constants(top)
        const_def_ids = {id(s) for _n, s in const_stmts}
        const_names = {n for n, _s in const_stmts}
        class_refs: dict[int, set[str]] = {}
        residual_refs: set[str] = set()
        for stmt in top:
            if id(stmt) in const_def_ids:
                continue
            names = _free_names_of(stmt) & const_names
            if isinstance(stmt, cst.ClassDef) and id(stmt) in subject_ids:
                class_refs[id(stmt)] = names
            else:
                residual_refs |= names
        external_consts = {
            k for k in const_names if _const_imported_elsewhere(self.repo, rel, module, k)
        }
        moved_consts: dict[int, set[str]] = {}
        for c in subjects:
            moved_consts[id(c)] = _consts_moving_into(
                const_stmts, class_refs, residual_refs, external_consts, c
            )
        moved_all = set().union(*moved_consts.values()) if moved_consts else set()

        # the residual: everything that stays behind, and the module-level
        # names the moved classes may need to import from the origin
        residual = [s for s in top if id(s) not in subject_ids and id(s) not in const_def_ids]
        residual_def_names = {
            s.name.value
            for s in residual
            if isinstance(s, (cst.FunctionDef, cst.ClassDef))
        }
        residual_def_names |= const_names - moved_all
        residual_subject_refs: set[str] = set()
        for s in residual:
            if isinstance(s, cst.SimpleStatementLine) and any(
                isinstance(i, (cst.Import, cst.ImportFrom)) for i in s.body
            ):
                continue  # imports bind their own names — not class references
            residual_subject_refs |= _free_names_of(s) & subject_names

        # cluster the subjects by cohesion edges: cross-class member
        # references OR shared class-level attributes
        free_of = {id(c): _class_free_names(c) for c in subjects}
        attrs_of = {id(c): _class_attr_names(c) for c in subjects}
        # a constant's VALUE may reference a moved class (`CACHE = {User: []}`)
        # — the residual keeps the constant, so the origin also imports the class
        for _n, stmt in const_stmts:
            residual_subject_refs |= _expr_names(_const_value(stmt)) & subject_names
        parent = {id(c): id(c) for c in subjects}

        def find(x: int) -> int:
            while parent[x] != x:
                parent[x] = parent[parent[x]]
                x = parent[x]
            return x

        for i, a in enumerate(subjects):
            for b in subjects[i + 1 :]:
                if (
                    b.name.value in free_of[id(a)]
                    or a.name.value in free_of[id(b)]
                    or attrs_of[id(a)] & attrs_of[id(b)]
                ):
                    parent[find(id(a))] = find(id(b))
        class_index = {id(s): i for i, s in enumerate(top)}
        comps: dict[int, list[cst.ClassDef]] = {}
        for c in subjects:
            comps.setdefault(find(id(c)), []).append(c)
        components = sorted(
            comps.values(), key=lambda cs: min(class_index[id(c)] for c in cs)
        )
        total = len(subjects)
        layouts: list[tuple[list[cst.ClassDef], str | None]] = []
        for comp in components:
            if len(comp) == 1:
                layouts.append((comp, None))
            elif len(comp) == total:
                layouts.append((comp, self.opts.name or stem))
            else:
                name = self.opts.name
                if not name:
                    self.decline = (
                        "the cohesive sub-cluster needs a package name — name the "
                        "package with a domain noun (--name)"
                    )
                    return None
                layouts.append((comp, name))
        any_pkg = any(name is not None for _comp, name in layouts)
        # relative imports in the origin change their resolution when the
        # module becomes a package (the path root moves) — refuse PACKAGE,
        # split flat (the class files are siblings, the imports stay valid)
        if any_pkg and _module_has_relative_imports(module):
            layouts = [(comp, None) for comp, _name in layouts]
            any_pkg = False
        whole_pkg = (
            len(layouts) == 1
            and layouts[0][1] is not None
            and len(layouts[0][0]) == total
        )

        # package layouts: no module-level constant may survive (BQ6) and no
        # residual code may evaluate a moved class at import time (the __init__
        # evaluates before its re-exports)
        if any_pkg and (const_names - moved_all):
            names = ", ".join(sorted(const_names - moved_all))
            self.decline = (
                f"the constant(s) {names} cannot move into a class; a package "
                "__init__ may hold no constants — split by hand"
            )
            return None
        if whole_pkg:
            for s in residual:
                refs = _exec_time_names(wrapper, s) & subject_names
                if refs:
                    self.decline = (
                        "module-level code evaluates " + ", ".join(sorted(refs))
                        + " at import time — the package __init__ runs before the "
                        "re-exports — split by hand"
                    )
                    return None

        # the new modules: flat files are snake_cased siblings; package members
        # live in `<pkg>/<Class>.py` and load via the package
        snake = {c.name.value: _snake_name(c.name.value) for c in subjects}
        targets: dict[str, list[str]] = {}
        for comp, name in layouts:
            for c in comp:
                targets[c.name.value] = [name] if name is not None else [snake[c.name.value]]

        # REFUSAL: target collisions (an existing sibling file, an existing
        # package dir — the stem package needs --name when taken)
        seen_files: set[str] = set()
        for comp, name in layouts:
            if name is None:
                for c in comp:
                    t = origin_dir + "/" + snake[c.name.value] + ".py" if origin_dir else snake[c.name.value] + ".py"
                    if t in seen_files:
                        self.decline = f"the split target '{t}' would collide — split by hand"
                        return None
                    seen_files.add(t)
                    if (self.repo / t).exists():
                        self.decline = f"'{t}' already exists — split by hand"
                        return None
            else:
                pkg_dir = origin_dir + "/" + name if origin_dir else name
                # the origin file itself is being replaced — only a DIFFERENT
                # `name.py` or an existing `name/` directory blocks the package
                if (self.repo / pkg_dir).exists() or (
                    (self.repo / f"{pkg_dir}.py").exists() and f"{pkg_dir}.py" != rel
                ):
                    if name == stem and self.opts.name is None:
                        self.decline = (
                            f"a package named '{name}' already exists — pass --name <name>"
                        )
                    else:
                        self.decline = (
                            f"the package name '{name}' is taken — pick another --name"
                        )
                    return None

        # REFUSAL: an import cycle in the produced layout — between the produced
        # class files (the __init__ re-export edge is exempt by construction),
        # or between a class file and the residual origin module (FLAT/sub)
        if any_pkg:
            for comp, name in layouts:
                if name is None:
                    continue
                nodes = [c.name.value for c in comp]
                edges = {n: set() for n in nodes}
                for c in comp:
                    for d in comp:
                        if d is not c and d.name.value in free_of[id(c)]:
                            edges[c.name.value].add(d.name.value)
                cyc = _graph_cycle(nodes, edges)
                if cyc:
                    self.decline = (
                        "the split would create an import cycle ("
                        + " -> ".join(cyc)
                        + ") — split by hand"
                    )
                    return None
        if not whole_pkg:
            bad = [
                c.name.value
                for c in subjects
                if (free_of[id(c)] & residual_def_names)
                and c.name.value in residual_subject_refs
            ]
            if bad:
                self.decline = (
                    "the split would create an import cycle between '"
                    + bad[0]
                    + "' and the origin module — split by hand"
                )
                return None
            if _module_star_imported(self.repo, rel, module):
                self.decline = (
                    "a file does 'from ... import *' on the origin — the split "
                    "cannot retarget it — split by hand"
                )
                return None

        # ---- build the new sources (pure — fix_finding applies the writes) ----
        extra_writes: list[tuple[str, str]] = []
        for comp, name in layouts:
            if name is None:
                for c in comp:
                    t = origin_dir + "/" + snake[c.name.value] + ".py" if origin_dir else snake[c.name.value] + ".py"
                    extra_writes.append(
                        (
                            t,
                            _split_class_source(
                                module,
                                wrapper,
                                c,
                                _SplitCtx(
                                    stem=stem,

                                    subjects=subject_names,
                                    residual_def_names=residual_def_names,
                                    moved=moved_consts.get(id(c), set()),
                                    package=None,
                                    origin_dots=1 if in_package else 0,
                                    member_dots=0,
                                ),
                            ),
                        )
                    )
            else:
                pkg_dir = origin_dir + "/" + name if origin_dir else name
                init_body = list(residual) + [
                    cst.SimpleStatementLine(body=[_from_import_node([c.name.value], [c.name.value], 1)])
                    for c in comp
                ]
                extra_writes.append((f"{pkg_dir}/__init__.py", cst.Module(body=init_body).code))
                for c in comp:
                    extra_writes.append(
                        (
                            f"{pkg_dir}/{c.name.value}.py",
                            _split_class_source(
                                module,
                                wrapper,
                                c,
                                _SplitCtx(
                                    stem=stem,

                                    subjects=subject_names,
                                    residual_def_names=residual_def_names,
                                    moved=moved_consts.get(id(c), set()),
                                    package=name,
                                    origin_dots=2 if in_package else 0,
                                    member_dots=1,
                                ),
                            ),
                        )
                    )

        # the origin residual: the kept statements in their original order plus
        # the from-imports the residual needs for the moved classes it uses
        moved_def_ids = {
            id(s) for n, s in const_stmts if n in moved_all
        }
        origin_kept = [s for s in top if id(s) not in subject_ids and id(s) not in moved_def_ids]
        origin_additions = [
            cst.SimpleStatementLine(body=[_from_import_node(targets[n], [n], 1 if in_package else 0)])
            for n in sorted(residual_subject_refs)
        ]
        origin_source = cst.Module(body=_insert_header_imports(origin_kept, origin_additions)).code

        # repo-wide rewrites for the layouts that move classes out of the
        # reachable origin (FLAT + sub-package): from-imports and `old.Cls`
        # attribute chains retarget to the class's new module
        rewritten: dict[str, str] = {}
        if not whole_pkg:
            for path in _py_files(self.repo):
                p_rel = path.relative_to(self.repo).as_posix()
                text = origin_source if p_rel == rel else path.read_text(encoding="utf-8")
                new = _split_rewrite_source(text, p_rel, origin_path, targets)
                if new is not None:
                    rewritten[p_rel] = new
            if rel in rewritten:
                origin_source = rewritten[rel]
            for p_rel, new in rewritten.items():
                if p_rel != rel:
                    extra_writes.append((p_rel, new))

        # safety: every produced file must parse, and every NEW target must not
        # exist (a rewritten file exists by definition — it is being edited)
        self.extra_writes = extra_writes
        for t, src in [(rel, origin_source)] + extra_writes:
            try:
                cst.parse_module(src)
            except Exception:
                self.decline = f"the computed split for '{t}' does not parse — split by hand"
                return None
            if t != rel and t not in rewritten and (self.repo / t).exists():
                self.decline = f"the split target '{t}' already exists — split by hand"
                return None

        # an origin that became empty is deleted only when no import names it
        # anywhere in the produced repo (BQ6(f)); a whole-module package always
        # replaces its origin file (the imports resolve through the package)
        if whole_pkg:
            self.deletes.append(rel)
            return ""
        final_sources = {p: s for p, s in rewritten.items()}
        final_sources.update({t: s for t, s in extra_writes})
        if _module_empty(origin_source) and not _origin_imported_anywhere(
            self.repo, final_sources, rel, origin_path
        ):
            self.deletes.append(rel)
            return ""
        return origin_source

    def _fn_seam_analysis(self):
        source, line = self._loaded_source(), self.line
        """Analyze the function at `line`: its body statements with per-statement
        spans, first-use contexts, writes, and control-flow flags."""
        module = cst.parse_module(source)
        wrapper = cst.MetadataWrapper(module)
        state = _FnBodyState(line)
        wrapper.visit(_Analyse(state))
        state.module_globals = _module_level_names(module)
        state.fn_writes = set().union(*(w for w in state.writes.values())) if state.writes else set()
        return module, wrapper, state

    def fix_duplicate_def(self) -> str | None:
        source, line = self._loaded_source(), self.line
        """Delete the shadowing (second) module-scope binding when nothing
        references it — proven by a repo-wide Name count (only the two def sites
        hit). Referenced shadows are renamed by the agent, never auto-deleted."""
        wrapper = cst.MetadataWrapper(cst.parse_module(source))
        probe = _FindModuleDef(line)
        wrapper.visit(probe)
        if probe.found is None or probe.name is None:
            return None
        if _name_occurrences(self.repo, probe.name) > 2:
            return None  # something references the name — a delete would break it
        return wrapper.module.visit(_RemoveNodes([probe.found])).code

    def fix_restating_docstring(self) -> str | None:
        source, line = self._loaded_source(), self.line
        return cst.MetadataWrapper(cst.parse_module(source)).visit(_StripDocstring(line)).code

    def fix_duplicate_block(self) -> str | None:
        source, line = self._loaded_source(), self.line
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
                pos = _as_range(self.get_metadata(PositionProvider, node))
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

    def _extract_module_proposal(self) -> _ModuleProposal | None:
        source = self._loaded_source()
        opts = self.opts
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
        top_defs = {stmt.name.value: stmt for stmt in module.body if isinstance(stmt, (cst.FunctionDef, cst.ClassDef))}
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
        new_module = cst.Module(body=[*_moved_imports(module, referenced), *(top_defs[n] for n in opts.params)])
        return _ModuleProposal(
            origin=cst.Module(body=_origin_after_move(module, move, opts, self.rel)).code,
            module=new_module.code,
        )

    def fix_extract_module(self) -> str | None:
        """The apply side of extract-module — writes the new module and returns
        the origin's new source (fix_finding writes the origin)."""
        result = self._extract_module_proposal()
        if result is None:
            return None
        origin_source, new_module_source = result
        repo, rel, module_name = self.repo, self.rel, self.opts.name
        new_path = repo / (rel.rsplit("/", 1)[0] if "/" in rel else "") / f"{module_name}.py"
        if new_path.exists():
            return None  # never clobber an existing module
        new_path.parent.mkdir(parents=True, exist_ok=True)
        new_path.write_text(new_module_source, encoding="utf-8")
        return origin_source

    def fix_dispatch_registry(self) -> str | None:
        source, line = self._loaded_source(), self.line
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

    def fix_rule_table(self) -> str | None:
        source, line = self._loaded_source(), self.line
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

    def _fix_mechanical(self) -> str | None:
        kind, source, line, opts = self.kind, self._loaded_source(), self.line, self.opts
        """The mechanical transforms — a changed source, or None when the callee
        is unresolvable (the retry protocol supplies params)."""
        if kind in ("noop-statement", "unreachable"):
            return cst.MetadataWrapper(cst.parse_module(source)).visit(_DeleteStatement(line)).code
        if kind == "stale-suppression":
            return cst.MetadataWrapper(cst.parse_module(source)).visit(_DeleteComment(line)).code
        if kind == "positional-literals":
            params = opts.params if opts.params is not None else self._callee_params_for_call()
            if params is None:
                self.decline = (
                    "the callee's parameter names could not be resolved from this file — "
                    "pass --params entries (comma,separated,parameter,names)"
                )
                return None
            xform = _KeywordArgs(line, params, opts.callee)
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
            return cst.MetadataWrapper(cst.parse_module(source)).visit(_DeclareMember(line, param_types)).code
        if kind in ("loop-pipeline", "loop-sequence"):
            xform = _LoopsIntoComprehensions(
                line, sequence=(kind == "loop-sequence"), fresh_name=_fresh_loop_name(source)
            )
            out = cst.MetadataWrapper(cst.parse_module(source)).visit(xform).code
            if xform.decline:
                self.decline = xform.decline
                return None
            return out if xform.applied else None
        if kind == "loop-hoist":
            if not opts.name:
                self.decline = (
                    "the hoisted helper needs a semantic name — pass --fix-name <Name> "
                    "(a domain noun for what one item contributes)"
                )
                return None
            xform = _LoopHoistExtractor(
                line,
                name=opts.name,
                helper_local=_fresh_loop_name(source, base="val"),
                element=_fresh_loop_name(source, base="part"),
            )
            out = cst.MetadataWrapper(cst.parse_module(source)).visit(xform).code
            if xform.decline:
                self.decline = xform.decline
                return None
            return out if xform.applied else None
        return None

    def fix_feature_envy(self) -> str | None:
        """Move the envied-receiver reads out of the method into a new method
        on the envied class (--name), replacing them with a call. The receiver
        is the local aliased from self.<attr> with the most field reads; the
        envied class comes from the owner's annotated `self.<attr>: <Class>` —
        the fix refuses when the class cannot be named (no annotation)."""
        source = self.source or ""
        method_name = self.opts.name
        if not method_name:
            return None
        module = cst.parse_module(source)
        wrapper = cst.MetadataWrapper(module)
        finder = _EnclosingFn(self.line)
        wrapper.visit(finder)
        method = finder.found
        if method is None or not isinstance(method, cst.FunctionDef):
            return None
        owner_cls = _EnclosingClass(self.line)
        wrapper.visit(owner_cls)
        owner = owner_cls.found
        if owner is None:
            return None
        aliases = _collaborator_aliases(method)
        if not aliases:
            return None
        alias, attr = max(aliases, key=lambda a: _alias_field_reads(method, a.alias))
        if _alias_field_reads(method, alias) < 4:
            return None
        envied = _find_envied_class(owner, attr)
        if envied is None:
            return None
        derived: set[str] = {alias}
        derived_order: list[str] = []
        movable: list = []
        movable_idx: list[int] = []
        remaining: list = []
        stmts = list(method.body.body)
        for i, stmt in enumerate(stmts):
            reads, bound, has_self = _stmt_analysis(stmt)
            external = reads - derived
            if not has_self and not external and bound:
                derived |= bound
                for n in bound:
                    if n not in derived_order:
                        derived_order.append(n)
                movable.append(stmt)
                movable_idx.append(i)
            else:
                remaining.append(stmt)
        if len(movable) < 1:
            return None
        remaining_reads: set[str] = set()
        for stmt in remaining:
            reads, _, _ = _stmt_analysis(stmt)
            remaining_reads |= reads
        outputs = [n for n in derived_order if n in remaining_reads]
        new_body: list = []
        for stmt in movable:
            new_body.append(_rewrite_receiver_to_self(stmt, alias))
        ret = cst.Return(value=cst.Tuple(elements=[cst.Element(cst.Name(n)) for n in outputs]) if outputs else None)
        new_body.append(cst.SimpleStatementLine(body=[ret]))
        new_method = cst.FunctionDef(
            name=cst.Name(method_name),
            params=cst.Parameters(params=[cst.Param(cst.Name("self"))]),
            body=cst.IndentedBlock(body=new_body),
        )
        wrapper_module = wrapper.module
        envied_cls = next(
            (c for c in wrapper_module.body if isinstance(c, cst.ClassDef) and c.name.value == envied),
            None,
        )
        if envied_cls is None:
            return None
        new_module_body = list(wrapper_module.body)
        cls_i = new_module_body.index(envied_cls)
        new_module_body[cls_i] = envied_cls.with_changes(
            body=envied_cls.body.with_changes(body=list(envied_cls.body.body) + [new_method])
        )
        call = cst.Call(func=cst.Attribute(value=cst.Name(alias), attr=cst.Name(method_name)))
        if outputs:
            call_stmt: cst.BaseStatement = cst.SimpleStatementLine(
                body=[
                    cst.Assign(
                        targets=[cst.AssignTarget(cst.Tuple(elements=[cst.Element(cst.Name(n)) for n in outputs]))],
                        value=call,
                    )
                ],
                leading_lines=[cst.EmptyLine()],
            )
        else:
            call_stmt = cst.SimpleStatementLine(body=[cst.Expr(call)])
        new_method_body: list = []
        for i, stmt in enumerate(stmts):
            if i in movable_idx:
                if i == movable_idx[0]:
                    new_method_body.append(call_stmt)
                continue
            new_method_body.append(stmt)
        new_method_def = method.with_changes(body=cst.IndentedBlock(body=new_method_body))
        owner_i = new_module_body.index(owner)
        new_module_body[owner_i] = owner.with_changes(
            body=owner.body.with_changes(body=[new_method_def if s is method else s for s in owner.body.body])
        )
        return cst.Module(body=new_module_body).code

    def _fix_structural(self) -> str | None:
        """The name-driven transforms — the agent supplies the semantic bit.
        A registry of kind -> fixer method (the conditional-polymorphism
        shape: a dispatch table, not branching)."""
        kind = self.kind
        if kind in _NAME_REQUIRED_KINDS and self.opts.name is None:
            return None
        fixer = _STRUCTURAL_FIXERS.get(kind)
        return getattr(self, fixer)() if fixer else None

    def _member_param_types(self, line: int) -> dict[str, str]:
        """The enclosing def's param name -> annotation map, for the member
        annotation inference."""
        module = cst.parse_module(self.source or "")
        wrapper = cst.MetadataWrapper(module)
        finder = _EnclosingFn(line)
        wrapper.visit(finder)
        fn = finder.found
        if fn is None:
            return {}
        result: dict[str, str] = {}
        for p in list(fn.params.posonly_params) + list(fn.params.params) + list(fn.params.kwonly_params):
            if p.annotation is not None:
                result[p.name.value] = cst.Module(body=[]).code_for_node(p.annotation.annotation)
        return result

    def fix_tuple_record(self) -> str | None:
        """The anonymous tuple record becomes a class: the build sites
        construct it, the constant-index reads and destructures become
        attribute reads, and the class definition is prepended to the
        module."""
        source = self._loaded_source()
        class_name = self.opts.name
        if not class_name:
            return None
        if not class_name.startswith("_"):
            class_name = "_" + class_name
        module = cst.parse_module(source)
        wrapper = cst.MetadataWrapper(module)
        # the record dict + the tuple arity from the build site
        build = _find_record_builds(module)
        record_dicts, elements = build.names, build.elements
        if elements is None or len(elements) < 2:
            return None
        field_names = []
        seen: set[str] = set()
        for i, el in enumerate(elements):
            base = _tuple_element_name(el)
            if base in seen or base == "value" and any(e != el for e in elements):
                base = f"field_{i}"
            seen.add(base)
            field_names.append(base)
        transformed = wrapper.visit(_RecordToClass(self.rel, record_dicts, class_name, field_names))
        # the class def plus an EmptyLine — byte-identical to the old
        # Expr(SimpleStatementLine) wrap, which codegen'd a trailing newline
        new_body: list = [_record_class_def(class_name, field_names), cst.EmptyLine()]
        new_body.extend(transformed.body)
        return cst.Module(body=new_body).code

    def fix_extract_record_class(self) -> str | None:
        """The constant-key dict literal becomes a class instance: the keys
        are the fields, string-subscript reads of the bound names become
        attribute reads, and the class definition is prepended to the module.
        The anchor (line, col) selects WHICH literal when several share a
        line; without col, the line must carry exactly one candidate."""
        source = self._loaded_source()
        raw_name = self.opts.name
        # a placeholder or non-identifier would generate `class _<Record>:`
        # — invalid Python; refuse before generating (review bot)
        if not raw_name or not raw_name.isidentifier():
            return None
        class_name = raw_name if raw_name.startswith("_") else "_" + raw_name
        wrapper = cst.MetadataWrapper(cst.parse_module(source))
        candidates = _find_record_dicts(wrapper, self.line)
        if self.col > 0:
            hits = [c for c in candidates if c[0] == self.line and c[1] == self.col]
        else:
            hits = [c for c in candidates if c[0] == self.line]
        if len(hits) != 1:
            # ambiguous without an anchor column — refusing beats guessing
            # which record becomes the class
            return None
        hit = hits[0]
        target_dict = hit.node
        raw_keys = _dict_constant_keys(target_dict)
        key_map: dict[str, str] = {}
        for i, k in enumerate(raw_keys):
            if not k.isidentifier():
                # a non-identifier key would change the wire format on
                # conversion — out of scope for a mechanical fix
                return None
            field = k
            while field in key_map.values():
                field = f"{field}_{i}"
            key_map[k] = field
        bind_names = _record_bind_names(wrapper, target_dict)
        rewritten = wrapper.visit(
            _DictToClass(self.line, self.col, class_name, key_map, frozenset(bind_names))
        )
        new_body: list = [_record_class_def(class_name, list(key_map.values())), cst.EmptyLine()]
        new_body.extend(rewritten.body)
        return cst.Module(body=new_body).code

    def _repo_params(self, callee: str) -> list[str] | None:
        repo, rel = self.repo, self.rel
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
            params = _params_of_any_def(source, callee)
            if params is not None:
                return params
        return None

    def _callee_params_for_call(self) -> list[str] | None:
        source, line = self._loaded_source(), self.line
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
                pos = _as_range(self.get_metadata(PositionProvider, node))
                if pos.start.line <= line <= pos.end.line and isinstance(node.func, cst.Name):
                    callee = node.func.value

        wrapper.visit(_FindCall())
        if callee is None:
            return None
        return self._repo_params(callee)

    def propose_finding(self):
        kind, rel, repo = self.kind, self.rel, self.repo
        opts = self.opts or FixOptions()
        source = self.source or (repo / rel).read_text(encoding="utf-8")
        self.source = source
        """Compute the fix WITHOUT writing — the preview surface for every
        structural kind. Returns (new_source, description) or (None, None) when
        nothing changes or the agent's semantic bit is missing."""
        opts = opts or FixOptions()
        kind = KIND_ALIASES.get(kind, kind)
        self.kind = kind
        path = repo / rel
        source = path.read_text(encoding="utf-8")
        if kind not in STRUCTURAL_KINDS:
            return None, None  # mechanical kinds apply directly, no preview
        if kind == "extract-method":
            # name-free preview: the seam is shown with a placeholder name the
            # agent replaces — naming AFTER seeing the diff, not before
            new_source, seam = self.extract_method_proposal()
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
            )
            preview = _FixRequest(
                kind=self.kind,
                repo=self.repo,
                rel=self.rel,
                line=self.line,
                opts=preview_opts,
                source=self.source,
            )
            result = preview._extract_module_proposal()
            if result is None or result[0] == source:
                return None, None
            return result[0], (
                f"extract-module: moves {', '.join(opts.params)} into a new module (name the module to apply)"
            )
        new_source = self._fix_structural()
        if new_source is None or new_source == source:
            return None, None
        return new_source, STRUCTURAL_KINDS[kind]

    def fix_finding(self) -> str | None:
        kind, rel, repo = self.kind, self.rel, self.repo
        opts = self.opts or FixOptions()
        source = self.source or (repo / rel).read_text(encoding="utf-8")
        self.source = source
        """Apply the transform for one finding. Returns a human description of
        what changed, or None when the finding was already gone (no edit).

        `opts` carries the agent-supplied semantic bits (the callee's parameter
        names for external/unresolved callees; the class name for extract-class)
        — the tool does the mechanical edit; the agent reads the signature once.
        """
        opts = opts or FixOptions()
        kind = KIND_ALIASES.get(kind, kind)
        self.kind = kind
        path = repo / rel
        source = path.read_text(encoding="utf-8")
        if kind in MECHANICAL_KINDS:
            new_source = self._fix_mechanical()
            description = MECHANICAL_KINDS[kind]
        elif kind == "extract-module":
            # two files change: the new module is created, the origin re-exports
            # the moved defs. Never clobbers an existing module.
            new_source = self.fix_extract_module()
            description = STRUCTURAL_KINDS[kind]
        elif kind in STRUCTURAL_KINDS:
            new_source = self._fix_structural()
            description = STRUCTURAL_KINDS[kind]
        else:
            raise ValueError(f"kind '{kind}' has no fix (mechanical or structural)")
        if new_source is None or new_source == source:
            return None  # nothing changed — the finding is stale or unlocatable (R28: silent)
        path.write_text(new_source, encoding="utf-8")
        self.wrote = True
        # multi-file transaction (R8): extra writes + deletes apply after the
        # origin, so partial failure leaves the origin... nothing is
        # transactional yet — writes are applied in order and a failure
        # surfaces loudly (fail-fast) rather than being swallowed
        for rel, extra_source in self.extra_writes:
            extra_path = repo / rel
            extra_path.parent.mkdir(parents=True, exist_ok=True)
            extra_path.write_text(extra_source, encoding="utf-8")
        for rel in self.deletes:
            dele = repo / rel
            if dele.exists() and not dele.is_dir():
                dele.unlink()
        return description
STRUCTURAL_KINDS = {
    "extract-method": "extract the seam into a private function (preview without a name, apply with --fix-name)",
    "extract-class": "move the strewing free functions into a class, rewriting call sites",
    "wide-tuple": (
        "introduce a record class for the fixed-arity tuple (--name overrides the name "
        "derived from the annotated variable), rewriting the annotations and build sites"
    ),
    "data-clump": (
        "thread the shared parameter pair as one parameter object (--name overrides the "
        "name derived from the pair), rewriting the clump's signatures, bodies, and call sites"
    ),
    "partition": (
        "split the field-disjoint class into one class per method group (--name is the "
        "shared prefix), each keeping the shared base and its own fields"
    ),
    "extract-module": (
        "move the named module-scope defs (--params) into a new module (--name), re-exported from the origin"
    ),
    "magic-number": "Replace Magic Literal: introduce the named constant",
    "tuple-record": "make the anonymous record a class (--name), rewriting the positional reads",
    "extract-record-class": (
        "make the constant-key dict a class (--name); its string-subscript reads become attribute reads"
    ),
    "feature-envy": (
        "move the envied-receiver reads into a method on the envied class (--name), replacing them with a call"
    ),
    "vague-name": "Rename the type and its references (same-file)",
    "long-param-list": "Introduce Parameter Object: bundle the params into a dataclass",
    "dispatch-registry": "convert the if/elif dispatch chain into a dict of selector -> handler functions",
    "rule-table": "hoist the latent data structure: the if/append battery becomes a (condition, violation) table",
    "dissolve-husk": (
        "dissolve the delegating-husk: rewire its callers to the module functions, delete the husk"
    ),
    "collapse-chain": (
        "collapse the forwarding chain: rewire the head method to the terminal (rewire or promote, "
        "never blind inline), delete the middle forwarder"
    ),
    "split-module": (
        "split the module so every public class lives in a file named after it (flat files, or a "
        "package when the misplaced classes are cohesive)"
    ),
}


# structural fixes whose result is genuinely novel (a class split, a new
# function, a bundled signature) preview a diff before --confirm; the
# obvious ones (a constant inserted, a rename) apply directly
PREVIEW_KINDS = {
    "extract-method",
    "extract-class",
    "wide-tuple",
    "data-clump",
    "partition",
    "extract-module",
    "long-param-list",
    "dispatch-registry",
    "rule-table",
    "dissolve-husk",
    "collapse-chain",
    "split-module",
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


class _DeclareMember(cst.CSTTransformer):
    """Annotate the undeclared `self.x = v` member: `self.x: T = v` with T
    inferred from the assigned value. Pure — no behavior change."""

    METADATA_DEPENDENCIES = (PositionProvider,)

    def __init__(self, target_line: int, param_types: dict[str, str]):
        self.target_line: int = target_line
        self.param_types: dict[str, str] = param_types

    @override
    def leave_Assign(self, original_node, updated_node):
        pos = _as_range(self.get_metadata(PositionProvider, original_node))
        if pos.start.line != self.target_line:
            return updated_node
        if len(updated_node.targets) != 1:
            return updated_node
        target = updated_node.targets[0].target
        if not isinstance(target, cst.Attribute):
            return updated_node
        if not isinstance(target.value, cst.Name) or target.value.value != "self":
            return updated_node
        annotation = _infer_member_type(updated_node.value, self.param_types)
        if annotation is None:
            return updated_node
        return cst.AnnAssign(
            target=target,
            annotation=cst.Annotation(cst.parse_expression(annotation)),
            value=updated_node.value,
            equal=cst.AssignEqual(),
            semicolon=updated_node.semicolon,
        )


def _tuple_element_name(elem: cst.BaseExpression) -> str:
    """A field name for one tuple position: a plain Name element keeps its
    name; a title(...)-style extraction is the record's name; anything else
    falls back to the positional name (the agent renames the class it
    asked for)."""
    if isinstance(elem, cst.Name):
        return elem.value
    if isinstance(elem, cst.Call) and isinstance(elem.func, cst.Name):
        return {"title": "name"}.get(elem.func.value, elem.func.value)
    return "value"


class _RecordToClass(cst.CSTTransformer):
    """Turn an anonymous tuple record into a class: the build sites construct
    the class, the positional reads become attribute reads, the destructures
    read the fields explicitly. The class definition is prepended."""

    METADATA_DEPENDENCIES = (PositionProvider,)

    def __init__(self, record_name: str, record_dicts: set[str], class_name: str, field_names: list[str]):
        self.record_name: str = record_name
        self.record_dicts: set[str] = record_dicts
        self.class_name: str = class_name
        self.field_names: list[str] = field_names

    def _is_record_base(self, e: cst.BaseExpression) -> bool:
        """The base name of an attribute/subscript chain — one of the record
        dicts?"""
        v = e
        while isinstance(v, (cst.Attribute, cst.Subscript)):
            v = v.value
        return isinstance(v, cst.Name) and v.value in self.record_dicts

    @override
    def leave_Subscript(self, original_node, updated_node):
        # X[k][N] -> X[k].field_N when the outer index is a constant position
        if not isinstance(updated_node.slice, tuple) or len(updated_node.slice) != 1:
            return updated_node
        el = updated_node.slice[0]
        if not isinstance(el, cst.SubscriptElement) or not isinstance(el.slice, cst.Index):
            return updated_node
        idx_node = el.slice.value
        if not isinstance(idx_node, cst.Integer):
            return updated_node
        idx = int(idx_node.value)
        if idx >= len(self.field_names) or not isinstance(updated_node.value, cst.Subscript):
            return updated_node
        if not self._is_record_base(updated_node.value.value):
            return updated_node
        return cst.Attribute(value=updated_node.value, attr=cst.Name(self.field_names[idx]))

    @override
    def leave_Dict(self, original_node, updated_node):
        return updated_node

    @override
    def leave_DictComp(self, original_node, updated_node):
        if isinstance(updated_node.value, cst.Tuple) and len(updated_node.value.elements) == len(self.field_names):
            return updated_node.with_changes(
                value=cst.Call(
                    func=cst.Name(self.class_name),
                    args=[cst.Arg(e.value) for e in updated_node.value.elements],
                )
            )
        return updated_node

    @override
    def leave_Assign(self, original_node, updated_node):
        # a, b = X[k] -> a, b = X[k].f0, X[k].f1
        if not isinstance(updated_node.value, cst.Subscript):
            return updated_node
        if not self._is_record_base(updated_node.value.value):
            return updated_node
        reads = [cst.Attribute(value=updated_node.value, attr=cst.Name(n)) for n in self.field_names]
        return updated_node.with_changes(value=cst.Tuple(elements=[cst.Element(r) for r in reads]))

    @override
    def leave_For(self, original_node, updated_node):
        # for k, (a, b) in X.items() -> for k, v in X.items(): a, b = v.f0, v.f1
        if not isinstance(updated_node.iter, cst.Call) or not isinstance(updated_node.iter.func, cst.Attribute):
            return updated_node
        if updated_node.iter.func.attr.value != "items":
            return updated_node
        if not self._is_record_base(updated_node.iter.func.value):
            return updated_node
        t = updated_node.target
        if not isinstance(t, cst.Tuple) or len(t.elements) != 2 or not isinstance(t.elements[1].value, cst.Tuple):
            return updated_node
        outer = t.elements[0].value
        inner = t.elements[1].value
        if len(inner.elements) != len(self.field_names):
            return updated_node
        binds = [e.value for e in inner.elements]
        reads = [cst.Attribute(value=cst.Name("v"), attr=cst.Name(n)) for n in self.field_names]
        unpack = cst.SimpleStatementLine(
            body=[
                cst.Assign(
                    targets=[cst.AssignTarget(cst.Tuple(elements=[cst.Element(b) for b in binds]))],
                    value=cst.Tuple(elements=[cst.Element(r) for r in reads]),
                )
            ],
            leading_lines=[cst.EmptyLine()],
        )
        new_body = list(updated_node.body.body)
        new_body.insert(0, unpack)
        return updated_node.with_changes(
            target=cst.Tuple(elements=[cst.Element(outer), cst.Element(cst.Name("v"))]),
            body=updated_node.body.with_changes(body=new_body),
        )


@dataclass
class _RecordBuild:
    """The record's dict names + the first build's elements (for the field
    names) — a named return instead of a bare tuple."""

    names: set[str]
    elements: list[cst.BaseExpression] | None


def _find_record_builds(module: cst.Module) -> _RecordBuild:
    """The dict names built with tuple values + the first build's elements
    (for the field-name inference)."""
    names: set[str] = set()
    elements: list[cst.BaseExpression] | None = None
    for stmt in module.body:
        if not isinstance(stmt, cst.SimpleStatementLine) or len(stmt.body) != 1:
            continue
        assign = stmt.body[0]
        if not isinstance(assign, cst.Assign) or len(assign.targets) != 1:
            continue
        target = assign.targets[0].target
        if not isinstance(target, cst.Name):
            continue
        value = assign.value
        if isinstance(value, cst.DictComp):
            value = value.value
        if isinstance(value, cst.Tuple) and len(value.elements) >= 2:
            names.add(target.value)
            if elements is None:
                elements = [e.value for e in value.elements]
            elif len(value.elements) != len(elements):
                return _RecordBuild(set(), None)
    return _RecordBuild(names, elements)


def _record_class_def(class_name: str, field_names: list[str]) -> cst.ClassDef:
    """The record class: __init__ taking the fields, storing them. A class,
    not a NamedTuple — it can grow behavior without a migration."""
    body = [
        cst.SimpleStatementLine(
            body=[
                cst.Assign(
                    targets=[cst.AssignTarget(cst.Attribute(cst.Name("self"), cst.Name(f)))],
                    value=cst.Name(f),
                )
            ]
        )
        for f in field_names
    ]
    init = cst.FunctionDef(
        name=cst.Name("__init__"),
        params=cst.Parameters(params=[cst.Param(cst.Name("self"))] + [cst.Param(cst.Name(f)) for f in field_names]),
        body=cst.IndentedBlock(body=body),
    )
    return cst.ClassDef(
        name=cst.Name(class_name),
        body=cst.IndentedBlock(body=[init]),
    )


class _RecordDictHit(NamedTuple):
    """One candidate literal: anchor position + the node + its const keys."""

    line: int
    col: int
    node: cst.Dict
    keys: list[str]


class _FindRecordDicts(cst.CSTVisitor):
    """Constant-key dict literals anchored on one line — cols disambiguate
    same-line twins."""

    METADATA_DEPENDENCIES = (PositionProvider,)

    def __init__(self, line: int) -> None:
        self.line: int = line
        self.hits: list[_RecordDictHit] = []

    @override
    def visit_Dict(self, node) -> None:
        pos = _as_range(self.get_metadata(PositionProvider, node))
        if pos.start.line != self.line:
            return
        keys: list[str] = []
        for el in node.elements:
            if not isinstance(el, cst.DictElement):
                continue
            k = el.key
            if isinstance(k, cst.SimpleString):
                val = k.evaluated_value
                keys.append(val if isinstance(val, str) else str(val))
        if len(node.elements) >= 2 and len(keys) == len(node.elements):
            self.hits.append(_RecordDictHit(pos.start.line, pos.start.column + 1, node, keys))


def _find_record_dicts(
    wrapper: cst.MetadataWrapper, line: int
) -> list[_RecordDictHit]:
    finder = _FindRecordDicts(line)
    wrapper.visit(finder)
    return finder.hits


def _dict_constant_keys(d: cst.Dict) -> list[str]:
    keys: list[str] = []
    for el in d.elements:
        if not isinstance(el, cst.DictElement):
            continue
        k = el.key
        if isinstance(k, cst.SimpleString):
            val = k.evaluated_value
            keys.append(val if isinstance(val, str) else str(val))
    return keys


def _record_bind_names(
    wrapper: cst.MetadataWrapper, target_dict: cst.Dict
) -> set[str]:
    """Names bound to THIS dict literal by assignment (`payload = {...}`)."""
    names: set[str] = set()

    class _Binds(cst.CSTVisitor):
        @override
        def visit_Assign(self, node) -> None:
            if node.value is not target_dict:
                return
            for tgt in node.targets:
                inner = getattr(tgt, "target", None)
                if isinstance(inner, cst.Name):
                    names.add(inner.value)

    wrapper.visit(_Binds())
    return names


class _DictToClass(cst.CSTTransformer):
    """Replace the flagged constant-key dict with the record constructor and
    rewrite string-subscript reads of its bound names to attribute reads."""

    METADATA_DEPENDENCIES = (PositionProvider,)

    def __init__(
        self,
        line: int,
        col: int,
        class_name: str,
        key_map: dict[str, str],
        bind_names: frozenset[str],
    ) -> None:
        self.line: int = line
        self.col: int = col
        self.class_name: str = class_name
        self.key_map: dict[str, str] = key_map
        self.bind_names: frozenset[str] = bind_names

    @override
    def leave_Dict(self, original_node, updated_node):
        pos = _as_range(self.get_metadata(PositionProvider, original_node))
        if pos.start.line != self.line or (self.col > 0 and pos.start.column + 1 != self.col):
            # col==0 keeps the legacy line-only contract; a transported col
            # pins the exact literal among same-line twins
            return updated_node
        keywords = []
        for el in updated_node.elements:
            if not isinstance(el, cst.DictElement):
                continue
            k = el.key
            if not isinstance(k, cst.SimpleString):
                continue
            raw = k.evaluated_value
            field = self.key_map.get(raw if isinstance(raw, str) else str(raw))
            if field is None:
                continue
            keywords.append(
                cst.Arg(keyword=cst.Name(field), value=el.value, equal=cst.AssignEqual(
                    whitespace_before=cst.SimpleWhitespace(""),
                    whitespace_after=cst.SimpleWhitespace(""),
                ))
            )
        return cst.Call(func=cst.Name(self.class_name), args=keywords)

    @override
    def leave_Subscript(self, original_node, updated_node):
        slices = updated_node.slice
        if (
            isinstance(updated_node.value, cst.Name)
            and updated_node.value.value in self.bind_names
            and len(slices) == 1
        ):
            idx = slices[0].slice
            if not isinstance(idx, cst.Index) or not isinstance(idx.value, cst.SimpleString):
                return updated_node
            raw = idx.value.evaluated_value
            field = self.key_map.get(raw if isinstance(raw, str) else str(raw))
            if field:
                return cst.Attribute(value=updated_node.value, attr=cst.Name(field))
        return updated_node


class _Collaborator(NamedTuple):
    """One `graph = self.graph` alias: the local name and the owner field it
    aliases — a named return instead of a bare tuple."""

    alias: str
    attr: str


class _StmtReads(NamedTuple):
    """One top-level statement's analysis for the feature-envy move: the
    names it reads (bound names excluded), the names it binds, and whether
    it touches self — a named return instead of a bare tuple."""

    reads: set[str]
    bound: set[str]
    has_self: bool


class _EnclosingClass(cst.CSTVisitor):
    """The innermost ClassDef containing the line — the method's owner."""

    METADATA_DEPENDENCIES = (PositionProvider,)

    def __init__(self, line: int) -> None:
        self.line: int = line
        self.found: cst.ClassDef | None = None

    @override
    def visit_ClassDef(self, node) -> None:
        pos = _as_range(self.get_metadata(PositionProvider, node))
        if pos.start.line <= self.line <= pos.end.line:
            self.found = node


def _collaborator_aliases(method: cst.FunctionDef) -> list[_Collaborator]:
    """The (alias, attr) pairs: `graph = self.graph` assignments in the
    method — the collaborators the method may envy."""
    out: list[_Collaborator] = []
    for stmt in method.body.body:
        if not isinstance(stmt, cst.SimpleStatementLine) or len(stmt.body) != 1:
            continue
        assign = stmt.body[0]
        if not isinstance(assign, cst.Assign) or len(assign.targets) != 1:
            continue
        target = assign.targets[0].target
        if not isinstance(target, cst.Name):
            continue
        value = assign.value
        if isinstance(value, cst.Attribute) and isinstance(value.value, cst.Name) and value.value.value == "self":
            out.append(_Collaborator(target.value, value.attr.value))
    return out


def _alias_field_reads(method: cst.FunctionDef, alias: str) -> int:
    """The `alias.field` reads in the method."""
    count = 0

    class _V(cst.CSTVisitor):
        @override
        def visit_Attribute(self, node) -> None:
            nonlocal count
            if isinstance(node.value, cst.Name) and node.value.value == alias:
                count += 1

    method.visit(_V())
    return count


def _stmt_analysis(stmt: cst.BaseStatement | cst.BaseSmallStatement) -> _StmtReads:
    reads: set[str] = set()
    bound: set[str] = set()
    attr_names: set[str] = set()
    has_self = False

    class _V(cst.CSTVisitor):
        @override
        def visit_Name(self, node) -> None:
            reads.add(node.value)

        @override
        def visit_Attribute(self, node) -> None:
            nonlocal has_self
            attr_names.add(node.attr.value)
            if isinstance(node.value, cst.Name) and node.value.value == "self":
                has_self = True

        @override
        def visit_Assign(self, node) -> None:
            for t in node.targets:
                bound.update(_hoist_target_names(t.target))

        @override
        def visit_AnnAssign(self, node) -> None:
            if node.target:
                bound.update(_hoist_target_names(node.target))

        @override
        def visit_For(self, node) -> None:
            bound.update(_hoist_target_names(node.target))

        @override
        def visit_CompFor(self, node) -> None:
            bound.update(_hoist_target_names(node.target))

        @override
        def visit_With(self, node) -> None:
            for item in node.items:
                if item.asname is not None:
                    bound.update(_hoist_target_names(item.asname.name))

    stmt.visit(_V())
    return _StmtReads(reads - bound - attr_names, bound, has_self)


def _find_envied_class(owner: cst.ClassDef, attr: str) -> str | None:
    """The annotated type of `self.<attr>` in the owner's __init__."""
    for stmt in owner.body.body:
        if not isinstance(stmt, cst.FunctionDef) or stmt.name.value != "__init__":
            continue
        for s in stmt.body.body:
            if not isinstance(s, cst.SimpleStatementLine) or len(s.body) != 1:
                continue
            assign = s.body[0]
            if not isinstance(assign, cst.AnnAssign) or assign.target is None:
                continue
            target = assign.target
            if (
                isinstance(target, cst.Attribute)
                and isinstance(target.value, cst.Name)
                and target.value.value == "self"
                and target.attr.value == attr
                and isinstance(assign.annotation.annotation, cst.Name)
            ):
                return assign.annotation.annotation.value
    return None


def _rewrite_receiver_to_self(
    stmt: cst.BaseStatement | cst.BaseSmallStatement, alias: str
) -> cst.BaseStatement | cst.BaseSmallStatement | cst.RemovalSentinel:
    """receiver.field -> self.field in the statement (attribute reads on the
    alias become self reads)."""

    class _T(cst.CSTTransformer):
        @override
        def leave_Attribute(self, original_node, updated_node):
            if isinstance(updated_node.value, cst.Name) and updated_node.value.value == alias:
                return updated_node.with_changes(value=cst.Name("self"))
            return updated_node

    result = stmt.visit(_T())
    # only Attribute reads are rewritten — no sentinel can come back
    assert not isinstance(result, (cst.RemovalSentinel, cst.FlattenSentinel))
    return result


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
        pos = _as_range(self.get_metadata(PositionProvider, original_node))
        if pos.start.line <= self.target_line <= pos.end.line:
            self.deleted = True
            return cst.RemoveFromParent()
        return updated_node


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
        pos = _as_range(self.get_metadata(PositionProvider, original_node))
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
            pos = _as_range(self.get_metadata(PositionProvider, original_node))
            if pos.start.line == self.target_line:
                self.deleted = True
                return cst.RemoveFromParent()
            return updated_node
        return super().on_leave(original_node, updated_node)


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
        pos = _as_range(self.get_metadata(PositionProvider, node))
        if not (pos.start.line <= self.target_line <= pos.end.line):
            return True
        self.target_start, self.target_end = pos.start, pos.end
        return True

    @override
    def leave_Call(self, original_node, updated_node):
        pos = _as_range(self.get_metadata(PositionProvider, original_node))
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
            if arg.keyword is not None or i not in positional or i < first_kw:
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


def _call_mutation_plan(node: cst.Expr) -> _LoopPipelinePlan | None:
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
        return _LoopPipelinePlan(acc=acc, collection="list", element=value, key=None, flatten=None, if_test=None)
    if method == "add":
        return _LoopPipelinePlan(acc=acc, collection="set", element=value, key=None, flatten=None, if_test=None)
    if method == "extend":
        return _LoopPipelinePlan(acc=acc, collection="list", element=None, key=None, flatten=value, if_test=None)
    return None


def _augassign_mutation_plan(node: cst.AugAssign) -> _LoopPipelinePlan | None:
    """`name += [a, b]` (list/tuple literal concatenation): one element is
    a direct element, several flatten through the literal."""
    aug = node
    if (
        not isinstance(aug.target, cst.Name)
        or not isinstance(aug.operator, cst.AddAssign)
        or not isinstance(aug.value, (cst.List, cst.Tuple))
    ):
        return None
    elts = aug.value.elements
    if len(elts) == 1:
        return _LoopPipelinePlan(
            acc=aug.target.value, collection="list", element=elts[0].value, key=None, flatten=None, if_test=None
        )
    return _LoopPipelinePlan(
        acc=aug.target.value, collection="list", element=None, key=None, flatten=aug.value, if_test=None
    )


def _subscript_mutation_plan(node: cst.Assign) -> _LoopPipelinePlan | None:
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
    return _LoopPipelinePlan(
        acc=target.value.value, collection="dict", element=node.value, key=inner.value, flatten=None, if_test=None
    )


def _mutation_plan(stmt: cst.BaseStatement | cst.BaseSmallStatement) -> _LoopPipelinePlan | None:
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
        return _call_mutation_plan(node)
    if isinstance(node, cst.AugAssign):
        return _augassign_mutation_plan(node)
    if isinstance(node, cst.Assign):
        return _subscript_mutation_plan(node)
    return None


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
            def_line = _as_range(self.get_metadata(PositionProvider, parent)).start.line
            if def_line != self.line:
                return None
            first = next((i for i, s in enumerate(stmts) if not isinstance(s, cst.EmptyLine)), None)
            if first is None or not isinstance(self._small_stmt(stmts[first]), (cst.Assign, cst.AnnAssign)):
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
            first_idx = next(
                (
                    i
                    for i, s in enumerate(stmts)
                    if isinstance(s, cst.For)
                    and _as_range(self.get_metadata(PositionProvider, s)).start.line == self.line
                ),
                None,
            )
            if first_idx is None:
                return None
            loop_count = 1
        return self._rewrite_plan(stmts, first_idx, loop_count)


    def _chain_length(self, stmts, start: int) -> int:
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
        init_small = self._small_stmt(init)
        init_value = init_small.value if isinstance(init_small, (cst.Assign, cst.AnnAssign)) else None
        if isinstance(loops[0].asynchronous, cst.Asynchronous):
            self.decline = "an async for-loop cannot become a comprehension (comprehensions are synchronous)"
            return None
        raw_plans: list[_LoopPipelinePlan | None] = [self._loop_plan(f) for f in loops]
        if any(p is None for p in raw_plans):
            self.decline = (
                "a loop in the chain is not a single collection mutation (append/add/extend/"
                "`+= [..]`/subscript-store) — a comprehension cannot express it; refactor by hand"
            )
            return None
        plans: list[_LoopPipelinePlan] = []
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
        if not self._empty_init(init_value, collection):
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
    def _small_stmt(stmt):
        """The small statement inside a statement line (blocks hold
        SimpleStatementLine wrappers) — or the statement itself when it is
        already compound/small."""
        if isinstance(stmt, cst.SimpleStatementLine) and len(stmt.body) == 1:
            return stmt.body[0]
        return stmt


    def _loop_plan(self, for_node: cst.For) -> _LoopPipelinePlan | None:
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
        plan = _mutation_plan(stmt)
        if plan is None:
            return None
        plan.if_test = if_test
        return plan

    def _comprehension(self, for_node: cst.For, plan: _LoopPipelinePlan):
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

    @staticmethod
    def _empty_init(value, collection: str) -> bool:
        if collection == "list":
            return isinstance(value, cst.List) and not value.elements
        if collection == "dict":
            return isinstance(value, cst.Dict) and not value.elements
        return (
            isinstance(value, cst.Call)
            and isinstance(value.func, cst.Name)
            and value.func.value == "set"
            and not value.args
        )



_HOIST_MUTATORS = {
    "append", "add", "extend", "appendleft", "update", "setdefault", "add_update", "discard", "remove",
}
_OTHER_MUTATORS = _HOIST_MUTATORS | {
    "pop", "insert", "sort", "reverse", "clear", "put", "push", "enqueue", "__setitem__",
}

class _NameRewriter(cst.CSTTransformer):
    """Rename every occurrence of one name — the hoist re-targets the
    accumulator's mutation receiver into the helper's fresh local."""

    def __init__(self, old: str, new: str) -> None:
        self.old = old
        self.new = new

    @override
    def leave_Name(self, original_node, updated_node):
        if updated_node.value == self.old:
            return updated_node.with_changes(value=self.new)
        return updated_node
def _iter_nodes(nodes):
    """Every node in the subtree, parents before children; accepts a node
    or an iterable of nodes."""
    if isinstance(nodes, cst.CSTNode):
        nodes = [nodes]
    for n in nodes:
        yield n
        for c in n.children:
            yield from _iter_nodes(c)


def _iter_nodes_pruned(nodes):
    """Like `_iter_nodes` but never descends into nested scopes — a `def`
    or `class` inside the walked body owns its locals; their writes are
    not the body's writes."""
    if isinstance(nodes, cst.CSTNode):
        nodes = [nodes]
    for n in nodes:
        yield n
        if isinstance(n, (cst.FunctionDef, cst.ClassDef)):
            continue
        for c in n.children:
            yield from _iter_nodes_pruned(c)


def _hoist_target_names(t) -> set[str]:
    """The names a binding TARGET binds — Name/Tuple/List/Starred (a
    subscript/attribute target is a WRITE, not a binding)."""
    if isinstance(t, cst.AssignTarget):
        return _hoist_target_names(t.target)
    if isinstance(t, cst.Name):
        return {t.value}
    if isinstance(t, (cst.Tuple, cst.List)):
        out: set[str] = set()
        for el in t.elements:
            out.update(_hoist_target_names(el.value))
        return out
    if isinstance(t, cst.StarredElement):
        return _hoist_target_names(t.value)
    return set()


def _expr_names(e) -> set[str]:
    return {n.value for n in _iter_nodes([e]) if isinstance(n, cst.Name)}


def _stmt_seq(x) -> list:
    if isinstance(x, cst.IndentedBlock):
        return list(x.body)
    return list(x or [])


def _bind_small(small, bound: set[str]) -> None:
    """The names one small statement binds."""
    if isinstance(small, cst.AnnAssign):
        bound.update(_hoist_target_names(small.target))
    elif isinstance(small, cst.Assign):
        for t in small.targets:
            bound.update(_hoist_target_names(t))
    elif isinstance(small, cst.AugAssign):
        bound.update(_hoist_target_names(small.target))


def _bind_children(stmt, bound: set[str]) -> None:
    """The names a compound statement binds across its child blocks."""
    seqs: list = []
    if isinstance(stmt, cst.For):
        bound.update(_expr_names(stmt.target))
        seqs = _stmt_seq(stmt.body) + _stmt_seq(stmt.orelse)
    elif isinstance(stmt, (cst.While, cst.If)):
        seqs = _stmt_seq(stmt.body) + _stmt_seq(stmt.orelse)
    elif isinstance(stmt, cst.Try):
        seqs = _stmt_seq(stmt.body) + _stmt_seq(stmt.orelse) + _stmt_seq(stmt.finalbody)
        for h in stmt.handlers:
            seqs += _stmt_seq(h.body)
    elif isinstance(stmt, cst.With):
        for item in stmt.items:
            optional = getattr(item, "optional_vars", None)
            if optional is not None:
                bound.update(_expr_names(optional))
        seqs = _stmt_seq(stmt.body)
    elif isinstance(stmt, cst.Match):
        for case in stmt.cases:
            seqs += _stmt_seq(case.body)
    for b in seqs:
        _bind_names(b, bound)


def _bind_names(stmt, bound: set[str]) -> None:
    """The names ONE statement binds — control-flow descend, nested defs
    excluded. `counts[x] = v` is a WRITE to counts, not a binding."""
    if isinstance(stmt, cst.SimpleStatementLine):
        for small in stmt.body:
            _bind_small(small, bound)
        return
    _bind_children(stmt, bound)

def _loop_body_bound(stmts: list[cst.BaseStatement]) -> set[str]:
    """The names BOUND inside a loop body (temps) — the temps the hoist
    rewrite may keep inside the helper, as opposed to outer state writes."""
    bound: set[str] = set()
    for s in stmts:
        _bind_names(s, bound)
    return bound


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
        if _extraction_is_private() and not name.startswith("_"):
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
        loop_idx = next(
            (
                i
                for i, s in enumerate(stmts)
                if isinstance(s, cst.For)
                and _as_range(self.get_metadata(PositionProvider, s)).start.line == self.line
            ),
            None,
        )
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
        init_small = _LoopsIntoComprehensions._small_stmt(init)
        plan = self._hoist_plan(list(loop.body.body), loop.target.value)
        if plan is None:
            return None
        acc, collection = plan
        if acc is None or collection is None:
            return None  # unreachable (the plan declines, never returns Nones) — the checker needs the guard
        init_value = init_small.value if isinstance(init_small, (cst.Assign, cst.AnnAssign)) else None
        if not _LoopsIntoComprehensions._empty_init(init_value, collection):
            self.decline = (
                f"the accumulator `{acc}` must be initialized to an empty "
                f"{'[]' if collection == 'list' else 'set()'}"
                f" directly before the loop — the hoist cannot preserve prior contents"
            )
            return None
        rewriter = _NameRewriter(acc, self.helper_local)
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
        bound = _loop_body_bound(body_stmts)
        bound.add(target_name)
        acc: str | None = None
        collection: str | None = None
        receivers: set[int] = set()  # Name node ids that ARE the append/add receiver
        nodes = list(_iter_nodes(body_stmts))
        for n in nodes:
            if not isinstance(n, cst.Call) or not isinstance(n.func, cst.Attribute):
                continue
            base = n.func.value
            if not isinstance(base, cst.Name) or base.value in bound:
                continue
            method = n.func.attr.value
            if method not in _HOIST_MUTATORS:
                if method in _OTHER_MUTATORS:
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
                    f"the body accumulates BOTH `{acc}` and `{base.value}` — the hoist "
                    "rewrite needs one accumulator"
                )
                return None
            receivers.add(id(base))
        if acc is None:
            self.decline = "no outer collection mutation found in the loop body"
            return None
        for n in nodes:
            if isinstance(n, cst.Name) and n.value == acc and id(n) not in receivers:
                self.decline = (
                    f"the body READS `{acc}` mid-build — a comprehension has no partial "
                    "state to read; refactor by hand"
                )
                return None
        for s in body_stmts:
            for n in _iter_nodes_pruned([s]):
                if isinstance(n, cst.AnnAssign):
                    target_list: list = [n.target]
                elif isinstance(n, cst.Assign):
                    target_list = list(n.targets)
                elif isinstance(n, cst.AugAssign):
                    target_list = [n.target]
                else:
                    continue
                for t in target_list:
                    for sub in _iter_nodes(t):
                        if isinstance(sub, cst.Name) and sub.value not in bound and sub.value != acc:
                            self.decline = (
                                f"the body also writes `{sub.value}` — the hoist rewrite "
                                "only moves a single-accumulator loop"
                            )
                            return None
        return acc, collection


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
    except Exception:
        return base
    candidate = base
    i = 2
    while candidate in names:
        candidate = f"{base}_{i}"
        i += 1
    return candidate


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


# --------------------------------------------------------------------------- extract-class (strewing)


class _MoveIntoClass(cst.CSTTransformer):
    """Delete the strewing free functions from module scope; rewrite
    same-file call sites `fn(recv, ...)` -> `recv.fn(...)`."""

    METADATA_DEPENDENCIES = (PositionProvider,)

    def __init__(self, fns: set[str], delete: bool = True) -> None:
        self.fns: set[str] = fns
        self.delete: bool = delete

    @override
    def leave_FunctionDef(self, original_node, updated_node):
        if self.delete and updated_node.name.value in self.fns:
            return cst.RemoveFromParent()
        return updated_node

    @override
    def leave_Call(self, original_node, updated_node):
        if not isinstance(updated_node.func, cst.Name):
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
    if isinstance(ann, cst.Name):
        return ann.value
    return None
@dataclass
class StrewingGroup:
    """The free-function group extract-class moves — shared leading type,
    the fn names in source order, the anchor line."""

    shared: str
    fns: list[str] = field(default_factory=list)
    anchor: int = 0

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
            pos = _as_range(self.get_metadata(PositionProvider, node))
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


class _InsertExtractedFn(cst.CSTTransformer):
    """Insert the extracted function after its source function, in the same
    container (module or class body)."""

    METADATA_DEPENDENCIES = (ParentNodeProvider,)

    def __init__(self, fn_name: str, fn_line: int, new_def: cst.FunctionDef) -> None:
        self.fn_name: str = fn_name
        self.fn_line: int = fn_line
        self.new_def: cst.FunctionDef = new_def
        self.done: bool = False

    def _maybe_insert(self, body: list, blank_lines: int) -> list:
        if self.done:
            return body
        out = []
        for stmt in body:
            out.append(stmt)
            if not self.done and isinstance(stmt, cst.FunctionDef) and stmt.name.value == self.fn_name:
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


def _for_target_names(node) -> set[str]:
    """The names a for-loop's target binds (single name, tuple/starred
    destructuring) — the ONLY names the loop statement re-binds each pass.
    A container that is not a for-loop binds nothing here."""
    if not isinstance(node, cst.For):
        return set()
    return _target_bound_names(node.target)


def _target_bound_names(t) -> set[str]:
    """Recursively collect the names an assignment target binds."""
    if isinstance(t, cst.Name):
        return {t.value}
    if isinstance(t, (cst.Tuple, cst.List)):
        out: set[str] = set()
        for el in t.elements:
            elem = el.value if isinstance(el, cst.StarredElement) else el
            out |= _target_bound_names(elem)
        return out
    return set()


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
        re_bound = _for_target_names(container_node)
        window_subtree = set(self._window_sids(i, j))
        after: set[str] = set()
        for k in range(j + 1, len(self.flat)):
            sid = self.flat[k][0]
            if sid in window_subtree:
                continue  # inside the window's own subtree, not sequential-after
            same_container = self.flat[k][1] == container
            for dsid in self._subtree_sids(sid):
                for name, ctx in self.first_use[dsid].items():
                    if ctx == "read" and same_container and name in re_bound:
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
        self.state: _FnBodyState = state

    @override
    def visit_FunctionDef(self, node) -> None:
        if (
            self.state.fn_node is not None
            or _as_range(self.get_metadata(PositionProvider, node)).start.line != self.state.line
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
            p = _as_range(self.get_metadata(PositionProvider, s))
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
                p = _as_node(self.get_metadata(ParentNodeProvider, n))
                ok = False
                while p is not None and not isinstance(p, cst.Module):
                    if isinstance(p, (cst.For, cst.While)):
                        ok = id(p) in loops
                        break
                    p = _as_node(self.get_metadata(ParentNodeProvider, p))
                if not ok:
                    return True
        return False

    @override
    def visit_Name(self, node) -> None:
        if self.state.fn_node is None:
            return
        sid = None
        parent = _as_node(self.get_metadata(ParentNodeProvider, node))
        if _is_keyword_name(node, parent):
            return  # a keyword-argument name (f(a=1)) is not a variable ref
        while parent is not None and not isinstance(parent, cst.Module):
            if parent is self.state.fn_node:
                break
            if id(parent) in self.state.nodes:
                sid = id(parent)
                break
            parent = _as_node(self.get_metadata(ParentNodeProvider, parent))
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


def _import_base_name(name: cst.Attribute | cst.Name) -> str:
    """The first component of a dotted module path: `a.b.c` binds `a`."""
    base: cst.BaseExpression = name
    while isinstance(base, cst.Attribute):
        base = base.value
    assert isinstance(base, cst.Name)  # an import's dotted path bottoms at a Name
    return base.value


def collect_stmt_names(add_target, line_stmt, names):
    stmts = line_stmt.body if isinstance(line_stmt, cst.SimpleStatementLine) else [line_stmt]
    for stmt in stmts:
        if isinstance(stmt, cst.Import):
            _collect_alias_names(names, stmt)
        elif isinstance(stmt, cst.ImportFrom):
            if isinstance(stmt.names, cst.ImportStar):
                continue  # `from x import *` binds no importable name
            for a in stmt.names:
                if a.asname is not None:
                    names.update( _hoist_target_names(a.asname.name))
                elif isinstance(a.name, cst.Name) and a.name.value != "*":
                    names.add(a.name.value)
        elif isinstance(stmt, (cst.Assign, cst.AnnAssign)):
            targets = stmt.targets if isinstance(stmt, cst.Assign) else [stmt.target]
            for t in targets:
                add_target(t)
        elif isinstance(stmt, (cst.FunctionDef, cst.ClassDef)):
            names.add(stmt.name.value)


def _collect_alias_names(names, stmt):
    for a in stmt.names:
        if a.asname is not None:
            names.update( _hoist_target_names(a.asname.name))
        else:
            names.add(_import_base_name(a.name))


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
    total = sum(state.decisions[sid] for sid, container, _ in state.flat if container == fn_sid)
    return max(0, total - 14) if total > 14 else 0


def _extraction_is_private() -> bool:
    """Is an extracted function private (leading underscore)? Yes — a fresh
    helper cannot have external callers (it did not exist before the fix),
    so by construction any extraction is an implementation detail of its
    source function. The convention propagates automatically: the seam's
    name gets the underscore whether the agent supplies it or not."""

    return True


class _CollectStores(cst.CSTVisitor):
    """The names each function assigns anywhere in its body — those are
    locals, never the receiver (a shadowing `state = state.line` must not
    have its loads renamed to `self`). One pass over the whole module,
    tracking the current function."""

    METADATA_DEPENDENCIES = (ExpressionContextProvider,)

    def __init__(self) -> None:
        self.per_fn: dict[str, set[str]] = {}
        self._current: str | None = None
        self._in_param: bool = False
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
        self.receivers: dict[str, str | None] = receivers  # fn name -> its receiver param name
        self.shadowed: StoredNames = shadowed  # fn name -> names stored in its body
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
        self.class_name: str = class_name
        self.classdef: cst.ClassDef = classdef
        self.methods: list[cst.FunctionDef] = methods

    @override
    def leave_ClassDef(self, original_node, updated_node):
        if updated_node.name.value == self.class_name:
            existing = list(updated_node.body.body)
            existing.extend(self.methods)
            return updated_node.with_changes(body=updated_node.body.with_changes(body=existing))
        return updated_node

    @override
    def leave_Module(self, original_node, updated_node):
        if self.class_name in {s.name.value for s in updated_node.body if isinstance(s, cst.ClassDef)}:
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


def _dataclass_def(name: str, params: list[cst.Param]) -> cst.ClassDef:
    """The bundled-params dataclass — one AnnAssign field per param."""
    field_lines = []
    for p in params:
        ann = p.annotation.annotation if p.annotation is not None else cst.Name("object")
        field_lines.append(
            cst.SimpleStatementLine(body=[cst.AnnAssign(target=cst.Name(p.name.value), annotation=cst.Annotation(ann))])
        )
    return cst.ClassDef(
        name=cst.Name(name),
        bases=[],
        decorators=[cst.Decorator(cst.Name("dataclass"))],
        body=cst.IndentedBlock(body=field_lines),
    )


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


def _ensure_dataclasses_import(body: list) -> list:
    """Prepend `from dataclasses import dataclass` when missing."""
    if any(_is_dataclass_import(s) for s in body):
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
                and _as_range(self.get_metadata(PositionProvider, node)).start.line == line
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
        self.fn_name: str = fn_name
        self.line: int = line
        self.params: set[str] = set(params)
        self.class_name: str = class_name

    @override
    def leave_FunctionDef(self, original_node, updated_node):
        if updated_node.name.value != self.fn_name:
            return updated_node
        if _as_range(self.get_metadata(PositionProvider, original_node)).start.line != self.line:
            return updated_node
        new_body = updated_node.body.visit(_BodyParamRewrite(list(self.params)))
        receiver = [p for p in updated_node.params.params if p.name is not None and p.name.value in ("self", "cls")]
        options_param = cst.Param(
            name=cst.Name("options"),
            annotation=cst.Annotation(cst.Name(self.class_name)),
        )
        return updated_node.with_changes(
            params=updated_node.params.with_changes(params=[*receiver, options_param]),
            body=new_body,
        )



class _ClassOwningLine(cst.CSTVisitor):
    """The innermost ClassDef whose span contains `line` — magic-number's
    constant home (third-pass 4). None when the literal lives outside any
    class (module-level function): the fixer refuses instead of parking the
    constant at module top (its own global-state finding)."""

    METADATA_DEPENDENCIES = (PositionProvider,)

    def __init__(self, line: int) -> None:
        self.line: int = line
        self.depth: int = 0
        self.owner: cst.ClassDef | None = None
        self.owner_depth: int = -1

    @override
    def visit_ClassDef(self, node) -> None:
        self.depth += 1
        pos = _as_range(self.get_metadata(PositionProvider, node))
        if pos.start.line <= self.line <= pos.end.line and self.depth > self.owner_depth:
            self.owner = node
            self.owner_depth = self.depth

    @override
    def leave_ClassDef(self, original_node) -> None:
        self.depth -= 1

class _FindLiteral(cst.CSTVisitor):
    """The numeric literal anchored at (line, col) — the schema-3 col pins
    same-line twins so the fix never rewrites the wrong operand."""

    METADATA_DEPENDENCIES = (PositionProvider,)

    def __init__(self, line: int, col: int) -> None:
        self.line: int = line
        self.col: int = col
        self.value: str | None = None

    def _match(self, node) -> bool:
        if self.value is not None:
            return False
        pos = _as_range(self.get_metadata(PositionProvider, node)).start
        if pos.line != self.line:
            return False
        if self.col > 0 and pos.column + 1 != self.col:
            return False
        self.value = node.value
        return True

    @override
    def visit_Integer(self, node) -> None:
        self._match(node)

    @override
    def visit_Float(self, node) -> None:
        self._match(node)


class _InsertClassConstant(cst.CSTTransformer):
    """Insert the magic-number constant as a CLASS ATTRIBUTE of the
    innermost class owning the literal — after the docstring, opens the real
    body. Leaves the innermost owner only (leave order is innermost-first),
    so nested classes do not double-receive."""

    METADATA_DEPENDENCIES = (PositionProvider,)

    def __init__(self, line: int, assignment: cst.SimpleStatementLine) -> None:
        self.line: int = line
        self.assignment: cst.SimpleStatementLine = assignment
        self.done: bool = False

    @override
    def leave_ClassDef(self, original_node, updated_node):
        if self.done:
            return updated_node
        pos = _as_range(self.get_metadata(PositionProvider, original_node))
        if not (pos.start.line <= self.line <= pos.end.line):
            return updated_node
        self.done = True
        class_body = list(updated_node.body.body)
        insert_at = 0
        if (
            class_body
            and isinstance(class_body[0], cst.SimpleStatementLine)
            and len(class_body[0].body) == 1
            and isinstance(class_body[0].body[0], cst.Expr)
            and isinstance(class_body[0].body[0].value, cst.SimpleString)
        ):
            insert_at = 1
        class_body.insert(insert_at, self.assignment)
        return updated_node.with_changes(body=updated_node.body.with_changes(body=class_body))


class _ReplaceLiteral(cst.CSTTransformer):
    """Replace the numeric literal on the target line with a name."""

    METADATA_DEPENDENCIES = (PositionProvider,)

    def __init__(self, target_line: int, col: int, name: str) -> None:
        self.target_line: int = target_line
        self.col: int = col
        self.name: str = name
        self.replaced: bool = False

    def _match(self, original_node) -> bool:
        if self.replaced:
            return False
        pos = _as_range(self.get_metadata(PositionProvider, original_node)).start
        return pos.line == self.target_line and (self.col == 0 or pos.column + 1 == self.col)

    @override
    def leave_Integer(self, original_node, updated_node):
        if self._match(original_node):
            self.replaced = True
            return cst.Name(self.name)
        return updated_node

    @override
    def leave_Float(self, original_node, updated_node):
        if self._match(original_node):
            self.replaced = True
            return cst.Name(self.name)
        return updated_node


class _RenameClass(cst.CSTTransformer):
    """Rename the class at the line + every Name reference in the file."""

    METADATA_DEPENDENCIES = (PositionProvider,)

    def __init__(self, target_line: int, old: str, new: str) -> None:
        self.target_line: int = target_line
        self.old: str = old
        self.new: str = new

    @override
    def leave_ClassDef(self, original_node, updated_node):
        pos = _as_range(self.get_metadata(PositionProvider, original_node))
        if pos.start.line == self.target_line and updated_node.name.value == self.old:
            return updated_node.with_changes(name=cst.Name(self.new))
        return updated_node

    @override
    def leave_Name(self, original_node, updated_node):
        if updated_node.value == self.old:
            return updated_node.with_changes(value=self.new)
        return updated_node


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


# --------------------------------------------------------------------------- review-log fixes
# duplicate-def (delete the unreferenced shadow), restating-docstring (delete
# the docstring), duplicate-block (delete the second copy), extract-module
# (move a domain's defs to a new module) — the family-album review log §10/§11.

_REPO_SKIP_DIRS = {
    ".git",
    ".venv",
    "venv",
    "node_modules",
    "__pycache__",
    ".lucidlint-cache",
    ".ruff_cache",
    ".pytest_cache",
    ".mypy_cache",
    ".pyrefly-cache",
    "htmlcov",
    "dist",
    "build",
    "target",
    ".code-review-graph",
    ".tox",
    ".eggs",
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
        self.name: str = name
        self.n: int = 0

    @override
    def visit_Name(self, node) -> None:
        if node.value == self.name:
            self.n += 1


# the orchestrator
# lucidlint: ignore data-clump the shared params are the scan layer's operations on one file — the class boundary is
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
        self.line: int = line
        self.depth: int = 0
        self.found: cst.CSTNode | None = None
        self.name: str | None = None

    @override
    def visit_FunctionDef(self, node) -> None:
        pos = _as_range(self.get_metadata(PositionProvider, node))
        if self.depth == 0 and self.found is None and pos.start.line == self.line:
            self.found, self.name = node, node.name.value
        self.depth += 1

    @override
    def leave_FunctionDef(self, original_node) -> None:
        self.depth -= 1

    @override
    def visit_ClassDef(self, node) -> None:
        pos = _as_range(self.get_metadata(PositionProvider, node))
        if self.depth == 0 and self.found is None and pos.start.line == self.line:
            self.found, self.name = node, node.name.value
        self.depth += 1

    @override
    def leave_ClassDef(self, original_node) -> None:
        self.depth -= 1


class _RemoveNodes(cst.CSTTransformer):
    """Remove exactly the given nodes from the tree (identity)."""

    def __init__(self, targets) -> None:
        self.targets: set[cst.CSTNode] = set(targets)

    @override
    def on_leave(self, original_node, updated_node):
        if original_node in self.targets:
            return cst.RemoveFromParent()
        return updated_node


def _strip_first_docstring(updated: cst.FunctionDef | cst.ClassDef, original: object) -> cst.FunctionDef | cst.ClassDef:
    if not isinstance(updated.body, cst.IndentedBlock):
        return updated  # a one-line `def f(): ...` body has no docstring statement
    body = updated.body.body
    first = body[0] if body else None
    if isinstance(first, cst.SimpleStatementLine) and len(first.body) == 1:
        stmt = first.body[0]
        if isinstance(stmt, cst.Expr) and isinstance(stmt.value, cst.SimpleString):
            return updated.with_changes(body=updated.body.with_changes(body=list(body[1:])))
    return updated


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
        if _as_range(self.get_metadata(PositionProvider, original_node)).start.line == self.line:
            self.done = True
            return _strip_first_docstring(updated_node, original_node)
        return updated_node

    @override
    def leave_ClassDef(self, original_node, updated_node):
        if self.done:
            return updated_node
        if _as_range(self.get_metadata(PositionProvider, original_node)).start.line == self.line:
            self.done = True
            return _strip_first_docstring(updated_node, original_node)
        return updated_node


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
                # an elif chain nests the next If directly; a plain else wraps
                # its suite in an Else — flatten either into its statements
                nested = [s.orelse] if isinstance(s.orelse, cst.If) else list(s.orelse.body.body)
                _flatten_stmts(nested, out)
        elif isinstance(s, (cst.For, cst.While)):
            _flatten_stmts(list(s.body.body), out)
            if s.orelse is not None:
                _flatten_stmts(list(s.orelse.body.body), out)
        elif isinstance(s, cst.Try):
            _flatten_stmts(list(s.body.body), out)
            for h in s.handlers:
                _flatten_stmts(list(h.body.body), out)
            if s.orelse is not None:
                _flatten_stmts(list(s.orelse.body.body), out)
        elif isinstance(s, cst.With):
            _flatten_stmts(list(s.body.body), out)
        elif isinstance(s, cst.Match):
            for case in s.cases:
                _flatten_stmts(list(case.body.body), out)

# lucidlint: ignore complexity the dispatch is a parser-shape table, not branch decision logic
def _forwarder_callee_of(method: cst.FunctionDef) -> str | None:
    """The module function a method's body exactly forwards to:
    body == `return F(<all params minus the receiver>)` -> F. None otherwise
    (not the pure-forwarder shape the husk rules demand)."""
    params = [p for p in method.params.params if p.name is not None and p.name.value not in ("self", "cls")]
    body = method.body.body if isinstance(method.body, cst.IndentedBlock) else []
    if len(body) != 1 or not isinstance(body[0], cst.SimpleStatementLine):
        return None
    stmts = body[0].body
    if len(stmts) != 1 or not isinstance(stmts[0], cst.Return):
        return None
    ret = stmts[0]
    if not isinstance(ret.value, cst.Call) or not isinstance(ret.value.func, cst.Name):
        return None
    args = list(ret.value.args)
    if len(args) != len(params):
        return None
    for arg, p in zip(args, params, strict=True):
        if not isinstance(arg.value, cst.Name) or arg.value.value != p.name.value:
            return None
    return ret.value.func.value


# lucidlint: ignore closures the nested visitors are one-purpose probe walks — hoisting names nothing the domain owns
def _rewire_husk_sites(module: cst.Module, husk: str, method_fns: dict[str, str]) -> str | None:
    """Rewrite a file's rewireable husk sites (`Husk().m(x)` / `h.m(x)` ->
    `m(x)`, `h = Husk()` deleted). None when any reference or constructed
    instance escapes the rewireable pattern — dissolve by hand."""

    class _HuskProbe(cst.CSTVisitor):
        """Collect instance variables (`h = Husk()`) and flag escapes."""

        def __init__(self):
            self.instances: set[str] = set()
            self.statements: list[cst.SimpleStatementLine] = []
            self.refused: str | None = None

        @override
        def visit_SimpleStatementLine(self, node):
            for stmt in node.body:
                if (
                    isinstance(stmt, cst.Assign)
                    and len(stmt.targets) == 1
                    and isinstance(stmt.targets[0].target, cst.Name)
                    and isinstance(stmt.value, cst.Call)
                    and isinstance(stmt.value.func, cst.Name)
                    and stmt.value.func.value == husk
                    and not stmt.value.args
                ):
                    self.instances.add(stmt.targets[0].target.value)
                    self.statements.append(node)
            return True

    probe = _HuskProbe()
    module.visit(probe)
    instances = probe.instances

    class _HuskRewire(cst.CSTTransformer):
        def __init__(self):
            self.bad: str | None = None

        @override
        def leave_Call(self, original_node, updated_node):
            func = updated_node.func
            if isinstance(func, cst.Attribute) and isinstance(func.value, cst.Call):
                # Husk().m(<args>) -> Fm(<args>)
                inner = func.value
                if (
                    isinstance(inner.func, cst.Name)
                    and inner.func.value == husk
                    and not inner.args
                    and func.attr.value in method_fns
                ):
                    return updated_node.with_changes(func=cst.Name(method_fns[func.attr.value]))
            # h.m(<args>) -> Fm(<args>) for a proven instance variable
            if (
                isinstance(func, cst.Attribute)
                and isinstance(func.value, cst.Name)
                and func.value.value in instances
                and func.attr.value in method_fns
            ):
                return updated_node.with_changes(func=cst.Name(method_fns[func.attr.value]))
            return updated_node

    rewired = module.visit(_HuskRewire())

    class _Leftover(cst.CSTVisitor):
        """Any remaining reference to the husk or an instance variable —
        EXCEPT inside the husk class's own subtree (deleted afterwards)."""

        def __init__(self):
            self.names: list[str] = []
            self.in_husk: int = 0

        @override
        def visit_ClassDef(self, node):
            if node.name.value == husk:
                self.in_husk += 1

        @override
        def leave_ClassDef(self, original_node):
            if original_node.name.value == husk:
                self.in_husk -= 1

        @override
        def visit_Name(self, node):
            if self.in_husk == 0 and (node.value == husk or node.value in instances):
                self.names.append(node.value)

    # drop the `h = Husk()` assignments: identity removal needs nodes OF the
    # rewired tree (the rewire produced new objects), so re-probe it
    drop_probe = _HuskProbe()
    rewired.visit(drop_probe)
    if drop_probe.statements:
        rewired = rewired.visit(_RemoveNodes(drop_probe.statements))
    # the husk class itself is deleted by the caller — scan leftovers on the
    # tree WITHOUT it (the class's own subtree must not count as a reference)
    scanned = rewired.visit(_RemoveHuskClass(husk))
    left_scan = _Leftover()
    scanned.visit(left_scan)
    leftover_names = list(left_scan.names)
    if leftover_names:
        return None  # a reference escaped the rewireable pattern
    return rewired.code


class _RemoveHuskClass(cst.CSTTransformer):
    """Delete the husk class (by name) from the origin after rewiring."""

    def __init__(self, husk: str) -> None:
        self.husk: str = husk

    @override
    def leave_ClassDef(self, original_node, updated_node):
        if original_node.name.value == self.husk:
            return cst.RemoveFromParent()
        return updated_node

def _moved_imports(module: cst.Module, referenced: set[str]) -> list:
    """The origin's module-level imports binding a referenced name — what the
    new module needs (libcst wraps imports in SimpleStatementLine)."""
    moved: list = []
    for stmt in module.body:
        if not isinstance(stmt, cst.SimpleStatementLine):
            continue
        # a line can hold several imports (`import os; import sys`) — emit
        # each matching import as its own statement, or the new module
        # misses a name the moved def needs and NameErrors (review bot)
        for inner in stmt.body:
            if isinstance(inner, cst.Import):
                for alias in inner.names:
                    bound = (
                         _hoist_target_names(alias.asname.name)
                        if alias.asname is not None
                        else {_import_base_name(alias.name)}
                    )
                    if bound & referenced:
                        moved.append(cst.SimpleStatementLine(body=[inner]))
                        break
            elif isinstance(inner, cst.ImportFrom):
                if isinstance(inner.names, cst.ImportStar):
                    continue  # `from x import *` binds no importable name
                for alias in inner.names:
                    if not isinstance(alias.name, cst.Name):
                        continue
                    bound = (
                         _hoist_target_names(alias.asname.name)
                        if alias.asname is not None
                        else {alias.name.value}
                    )
                    if bound & referenced:
                        moved.append(cst.SimpleStatementLine(body=[inner]))
                        break
    return moved


# the orchestrator
# lucidlint: ignore data-clump the shared params are the scan layer's operations on one file — the class boundary is
def _origin_after_move(module: cst.Module, move: set[str], opts, rel: str) -> list:
    """The origin's body after the split: the moved defs dropped, the
    re-export import inserted after the last import — every other file's
    `from origin import x` keeps working (the origin re-exports). The
    re-export is RELATIVE when the origin sits in a package (`houses/text.py`
    is not on sys.path as a top-level module) — review finding."""
    remaining = [
        s for s in module.body if not (isinstance(s, (cst.FunctionDef, cst.ClassDef)) and s.name.value in move)
    ]
    in_package = "/" in (rel or "")
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
        if (
            isinstance(s, cst.SimpleStatementLine)
            and len(s.body) == 1
            and isinstance(s.body[0], (cst.Import, cst.ImportFrom))
        ):
            insert_at = i + 1
    remaining.insert(insert_at, reexport)
    return remaining


@dataclass(frozen=True)
class _ForwardTarget:
    """F/M's forwarder callee: a module fn (is_name) or a method on another
    object ((recv-param, method))."""

    is_name: bool
    name: str = ""
    attr_method: tuple[str, str] | None = None


def _pure_forward_of(fn: cst.FunctionDef) -> tuple[_ForwardTarget | None, list[str]]:
    """The forwarder call + arg names when `fn`'s body is exactly
    `return <callee>(<arg names>)` — the #30 re-derivation of the pure
    forwarder shape (pinned by the scanner's own rule tests)."""
    if not isinstance(fn.body, cst.IndentedBlock) or len(fn.body.body) != 1:
        return None, []
    line = fn.body.body[0]
    if not isinstance(line, cst.SimpleStatementLine) or len(line.body) != 1:
        return None, []
    ret = line.body[0]
    if not isinstance(ret, cst.Return) or not isinstance(ret.value, cst.Call):
        return None, []
    call = ret.value
    argnames = []
    for a in call.args:
        if not isinstance(a.value, cst.Name):
            return None, []
        argnames.append(a.value.value)
    if isinstance(call.func, cst.Name):
        return _ForwardTarget(is_name=True, name=call.func.value), argnames
    if isinstance(call.func, cst.Attribute) and isinstance(call.func.value, cst.Name):
        return _ForwardTarget(is_name=False, attr_method=(call.func.value.value, call.func.attr.value)), argnames
    return None, []


# the orchestrator
# lucidlint: ignore data-clump the shared params are the scan layer's operations on one file — the class boundary is
def _module_fn_node(module: cst.Module, name: str) -> cst.FunctionDef | None:
    for stmt in module.body:
        if isinstance(stmt, cst.FunctionDef) and stmt.name.value == name:
            return stmt
    return None


def _module_fn_definition(repo: Path, name: str) -> tuple[str, cst.Module] | None:
    for path in _py_files(repo):
        mod = cst.parse_module(path.read_text(encoding="utf-8"))
        if _module_fn_node(mod, name) is not None:
            return path.relative_to(repo).as_posix(), mod
    return None


def _call_sites(repo: Path, name: str) -> int:
    total = 0
    for path in _py_files(repo):
        mod = cst.parse_module(path.read_text(encoding="utf-8"))

        class _Count(cst.CSTVisitor):
            def __init__(self):
                self.n = 0

            @override
            def visit_Call(self, node):
                if isinstance(node.func, cst.Name) and node.func.value == name:
                    self.n += 1

        c = _Count()
        mod.visit(c)
        total += c.n
    return total


def _reexported(repo: Path, name: str) -> bool:
    for path in _py_files(repo):
        mod = cst.parse_module(path.read_text(encoding="utf-8"))
        for stmt in mod.body:
            if isinstance(stmt, cst.SimpleStatementLine):
                for inner in stmt.body:
                    if not isinstance(inner, cst.ImportFrom) or isinstance(inner.names, cst.ImportStar):
                        continue
                    for alias in inner.names:
                        if isinstance(alias.name, cst.Name) and alias.name.value == name:
                            return True
    return False


# the orchestrator
# lucidlint: ignore data-clump the shared params are the scan layer's operations on one file — the class boundary is
def _depends_on(repo: Path, rel: str, d_rel: str) -> bool:
    if rel == d_rel:
        return True
    mod = cst.parse_module((repo / rel).read_text(encoding="utf-8"))
    target_stem = Path(d_rel).stem
    for stmt in mod.body:
        if not isinstance(stmt, cst.SimpleStatementLine):
            continue
        for inner in stmt.body:
            if isinstance(inner, cst.ImportFrom) and inner.module is not None:
                mod_name = inner.module if isinstance(inner.module, str) else (
                    inner.module.value if hasattr(inner.module, "value") else ""
                )
                if str(mod_name).split(".")[-1] == target_stem:
                    return True
    return False


def _methods_named(repo: Path, name: str) -> list[tuple[str, cst.ClassDef, cst.FunctionDef]]:
    out: list[tuple[str, cst.ClassDef, cst.FunctionDef]] = []
    for path in _py_files(repo):
        mod = cst.parse_module(path.read_text(encoding="utf-8"))
        for stmt in mod.body:
            if not isinstance(stmt, cst.ClassDef):
                continue
            for mem in stmt.body.body:
                if isinstance(mem, cst.FunctionDef) and mem.name.value == name:
                    out.append((path.relative_to(repo).as_posix(), stmt, mem))
    return out


def _class_has_identity(cls: cst.ClassDef) -> bool:
    writes_self = False

    class _Scan(cst.CSTVisitor):
        @override
        def visit_Assign(self, node):
            nonlocal writes_self
            for t in node.targets:
                if (
                    isinstance(t.target, cst.Attribute)
                    and isinstance(t.target.value, cst.Name)
                    and t.target.value.value == "self"
                ):
                    writes_self = True

        @override
        def visit_AnnAssign(self, node):
            nonlocal writes_self
            if (
                isinstance(node.target, cst.Attribute)
                and isinstance(node.target.value, cst.Name)
                and node.target.value.value == "self"
            ):
                writes_self = True

    cls.visit(_Scan())
    for stmt in cls.body.body:
        if isinstance(stmt, cst.AnnAssign):
            return True  # declared class-level field
        if isinstance(stmt, cst.FunctionDef) and _pure_forward_of(stmt)[0] is None:
            return True  # a non-forwarding member — D is real
    return writes_self


def _method_uses_self(method: cst.FunctionDef) -> bool:
    uses = False

    class _Scan(cst.CSTVisitor):
        @override
        def visit_Attribute(self, node):
            nonlocal uses
            if isinstance(node.value, cst.Name) and node.value.value == "self":
                uses = True

    method.visit(_Scan())
    return uses


def _zero_external_refs(repo: Path, class_name: str) -> bool:
    total = 0
    for path in _py_files(repo):
        mod = cst.parse_module(path.read_text(encoding="utf-8"))
        for node in _module_name_nodes(mod):
            if node.value == class_name:
                total += 1
    return total <= 1  # only the definition itself


def _module_name_nodes(module: cst.Module) -> list[cst.Name]:
    class _AllNames(cst.CSTVisitor):
        def __init__(self):
            self.names: list[cst.Name] = []
        @override
        def visit_Name(self, node):
            self.names.append(node)

    v = _AllNames()
    module.visit(v)
    return v.names


class _MethodBodyRewriteByLine(cst.CSTTransformer):
    METADATA_DEPENDENCIES = (PositionProvider,)

    def __init__(self, line: int, name: str, body: list) -> None:
        self.line = line
        self.name = name
        self.body = body

    @override
    def leave_FunctionDef(self, original_node, updated_node):
        if updated_node.name.value == self.name:
            pos = _as_range(self.get_metadata(PositionProvider, original_node))
            if pos.start.line == self.line:
                return updated_node.with_changes(body=updated_node.body.with_changes(body=self.body))
        return updated_node


# lucidlint: ignore data-clump the (module, positions, line) triple is the analyzer's input record
def _replace_method_body(
    module: cst.Module, method: cst.FunctionDef, body: cst.SimpleStatementLine, line: int | None = None
) -> cst.Module:
    if line is None:
        line = _as_range(cst.MetadataWrapper(module).resolve(PositionProvider)[method]).start.line
    return cst.MetadataWrapper(module).visit(_MethodBodyRewriteByLine(line, method.name.value, [body]))


class _ReplaceClassByLine(cst.CSTTransformer):
    METADATA_DEPENDENCIES = (PositionProvider,)

    def __init__(self, line: int, new_class: cst.ClassDef) -> None:
        self.line = line
        self.new_class = new_class
        self.done = False

    @override
    def leave_ClassDef(self, original_node, updated_node):
        if self.done:
            return updated_node
        pos = _as_range(self.get_metadata(PositionProvider, original_node))
        if pos.start.line == self.line:
            self.done = True
            return self.new_class
        return updated_node


def _replace_class_body(module: cst.Module, line: int, new_class: cst.ClassDef) -> cst.Module:
    return cst.MetadataWrapper(module).visit(_ReplaceClassByLine(line, new_class))


class _RemoveFn(cst.CSTTransformer):
    def __init__(self, name: str) -> None:
        self.name = name

    @override
    def leave_FunctionDef(self, original_node, updated_node):
        if updated_node.name.value == self.name and not updated_node.decorators:
            return cst.RemoveFromParent()
        return updated_node


def _remove_fn(module: cst.Module, name: str) -> str:
    return module.visit(_RemoveFn(name)).code


class _RemoveClass(cst.CSTTransformer):
    def __init__(self, name: str) -> None:
        self.name = name

    @override
    def leave_ClassDef(self, original_node, updated_node):
        if updated_node.name.value == self.name:
            return cst.RemoveFromParent()
        return updated_node


def _remove_class(module: cst.Module, name: str) -> str:
    return module.visit(_RemoveClass(name)).code


def _module_of(repo: Path, rel: str) -> cst.Module:
    return cst.parse_module((repo / rel).read_text(encoding="utf-8"))


# --------------------------------------------------------------------------- split-module (#31) helpers


def _class_matches_stem(name: str, stem: str) -> bool:
    """A class name matches the file stem case-insensitively, with the
    plural-underscore form tolerated (`APIKey` in api_key.py)."""
    n = name.lower()
    return n == stem or n == stem.replace("_", "")


def _snake_name(name: str) -> str:
    """CamelCase -> snake_case, acronym-aware (`APIKey` -> api_key, not
    a_p_i_key) — the flat sibling file's stem for a moved class."""
    s = re.sub(r"(?<=[a-z0-9])(?=[A-Z])", "_", name)
    s = re.sub(r"(?<=[A-Z])(?=[A-Z][a-z])", "_", s)
    return s.lower()


def _class_free_names(cls: cst.ClassDef) -> set[str]:
    """The names a class READS from the enclosing module: every Name in its
    subtree minus the names bound inside it and minus its own name."""
    probe = _FreeNames()
    cls.visit(probe)
    return probe.names - probe.bound - {cls.name.value}


def _free_names_of(stmt) -> set[str]:
    """The names one module-level statement reads from the module scope
    (bindings inside it excluded) — the residual's references to moved
    classes and constants."""
    probe = _FreeNames()
    stmt.visit(probe)
    return probe.names - probe.bound


def _class_attr_names(cls: cst.ClassDef) -> set[str]:
    """The class-level attribute names a class binds directly in its body —
    the shared-class-level-attribute cohesion edge."""
    names: set[str] = set()
    for stmt in cls.body.body:
        if not isinstance(stmt, cst.SimpleStatementLine):
            continue
        for inner in stmt.body:
            if isinstance(inner, cst.Assign):
                for t in inner.targets:
                    if isinstance(t.target, cst.Name):
                        names.add(t.target.value)
            elif isinstance(inner, cst.AnnAssign) and isinstance(inner.target, cst.Name):
                names.add(inner.target.value)
    return names


def _stmt_is_plain_assign(stmt) -> bool:
    """An import / pass / plain-name assignment (a constant) — the module
    statements allowed between the classes of a module being split."""
    if not isinstance(stmt, cst.SimpleStatementLine):
        return False
    for inner in stmt.body:
        if isinstance(inner, (cst.Import, cst.ImportFrom, cst.Pass)):
            continue
        if isinstance(inner, cst.Assign) and all(
            isinstance(t.target, cst.Name) for t in inner.targets
        ):
            continue
        if isinstance(inner, cst.AnnAssign) and isinstance(inner.target, cst.Name):
            continue
        return False
    return True


def _module_constants(top: Sequence) -> list[tuple[str, cst.SimpleStatementLine]]:
    """The module-level constants in source order: single-target Assign /
    AnnAssign to plain names (the BQ6 subject set)."""
    out: list[tuple[str, cst.SimpleStatementLine]] = []
    for stmt in top:
        if not isinstance(stmt, cst.SimpleStatementLine) or len(stmt.body) != 1:
            continue
        inner = stmt.body[0]
        if isinstance(inner, cst.Assign) and len(inner.targets) == 1 and isinstance(
            inner.targets[0].target, cst.Name
        ):
            out.append((inner.targets[0].target.value, stmt))
        elif isinstance(inner, cst.AnnAssign) and inner.value is not None and isinstance(
            inner.target, cst.Name
        ):
            out.append((inner.target.value, stmt))
    return out


def _const_value(stmt: cst.SimpleStatementLine) -> cst.BaseExpression:
    inner = stmt.body[0]
    if isinstance(inner, cst.Assign):
        return inner.value
    if isinstance(inner, cst.AnnAssign) and inner.value is not None:
        return inner.value
    return cst.Name("")


def _module_has_relative_imports(module: cst.Module) -> bool:
    return any(
        isinstance(s, cst.SimpleStatementLine)
        and any(isinstance(i, cst.ImportFrom) and i.relative for i in s.body)
        for s in module.body
    )


def _class_body_binds(cls: cst.ClassDef, name: str) -> bool:
    """Does the class's body bind `name` at class-body level (an attribute
    with the same name as the constant about to move in)?"""
    for stmt in cls.body.body:
        if isinstance(stmt, (cst.FunctionDef, cst.ClassDef)):
            if stmt.name.value == name:
                return True
        elif isinstance(stmt, cst.SimpleStatementLine):
            for inner in stmt.body:
                if isinstance(inner, cst.Assign) and any(
                    name in _hoist_target_names(t.target) for t in inner.targets
                ):
                    return True
                if isinstance(inner, cst.AnnAssign) and inner.target and name in _hoist_target_names(
                    inner.target
                ):
                    return True
                if isinstance(inner, (cst.Import, cst.ImportFrom)):
                    bound: set[str] = set()
                    _collect_alias_names(bound, inner)
                    if name in bound:
                        return True
    return False



# lucidlint: ignore complexity the dispatch is a parser-shape table, not branch decision logic
def _name_ref_kind(wrapper: cst.MetadataWrapper, node: cst.Name) -> str:
    """A Name reference's execution context: 'skip' (a binding or a
    non-reference position), 'deferred' (evaluates at call time — inside a
    function body), or 'immediate' (evaluates at import/class-definition
    time — module statements, class bodies, defaults, decorators)."""
    parents = wrapper.resolve(ParentNodeProvider)
    parent = parents.get(node)
    if parent is None:
        return "skip"
    if isinstance(parent, cst.AssignTarget):
        return "skip"
    if isinstance(parent, cst.AnnAssign) and parent.target is node:
        return "skip"
    if isinstance(parent, cst.Param) and parent.name is node:
        return "skip"
    if isinstance(parent, cst.For) and parent.target is node:
        return "skip"
    if isinstance(parent, cst.CompFor) and parent.target is node:
        return "skip"
    if isinstance(parent, cst.AsName):
        return "skip"
    if isinstance(parent, cst.ImportAlias):
        return "skip"
    if isinstance(parent, cst.NamedExpr) and parent.target is node:
        return "skip"
    if isinstance(parent, cst.Arg) and parent.keyword is node:
        return "skip"
    if isinstance(parent, cst.Attribute) and parent.attr is node:
        return "skip"
    if isinstance(parent, cst.ClassDef) and parent.name is node:
        return "skip"
    if isinstance(parent, cst.FunctionDef) and parent.name is node:
        return "skip"
    cur = parent
    while cur is not None:
        if isinstance(cur, cst.IndentedBlock):
            p = parents.get(cur)
            if isinstance(p, cst.FunctionDef) and p.body is cur:
                return "deferred"
        if isinstance(cur, cst.FunctionDef):
            return "immediate"
        cur = parents.get(cur)
    return "immediate"


def _exec_time_names(wrapper: cst.MetadataWrapper, node) -> set[str]:
    """The names `node` reads at evaluation time (function bodies deferred) —
    the residual check that refuses package layouts whose import-time code
    touches a moving class."""

    class _ExecNames(cst.CSTVisitor):
        def __init__(self) -> None:
            self.names: set[str] = set()

        @override
        def visit_Name(self, node) -> None:
            if _name_ref_kind(wrapper, node) == "immediate":
                self.names.add(node.value)

    probe = _ExecNames()
    node.visit(probe)
    return probe.names

# lucidlint: ignore complexity the dispatch is a parser-shape table, not branch decision logic
def _consts_moving_into(
    const_stmts: list[tuple[str, cst.SimpleStatementLine]],
    class_refs: dict[int, set[str]],
    residual_refs: set[str],
    external_consts: set[str],
    cls: cst.ClassDef,
) -> set[str]:
    """The constants that may move INTO this class as class attributes (BQ6):
    every reader is inside the class's own subtree (a residual or foreign
    reader, or an external `from mod import CONST`, keeps it in the origin),
    the class does not already bind the name, and the value references only
    literals, builtins, or earlier co-moving constants (a class body
    evaluates sequentially — a later constant is not yet bound, and a
    constant that STAYS in the origin must not read a moved one)."""
    cls_id = id(cls)
    own = class_refs.get(cls_id, set())
    others = {n for cid, refs in class_refs.items() if cid != cls_id for n in refs}
    order = {n: i for i, (n, _s) in enumerate(const_stmts)}
    value_of = {n: _const_value(s) for n, s in const_stmts}
    value_refs_of = {n: _expr_names(v) - _BUILTINS for n, v in value_of.items()}
    readers_via_value: dict[str, set[str]] = {}
    for reader, vrefs in value_refs_of.items():
        for k in vrefs:
            readers_via_value.setdefault(k, set()).add(reader)
    candidates = {
        n
        for n, _s in const_stmts
        if n in own
        and n not in residual_refs
        and n not in others
        and n not in external_consts
        and not _class_body_binds(cls, n)
    }
    while True:
        changed = False
        for n in list(candidates):
            # every constant that READS n through its value must co-move
            if any(m not in candidates for m in readers_via_value.get(n, ())):
                candidates.discard(n)
                changed = True
                continue
            vrefs = value_refs_of[n]
            if not vrefs <= candidates or any(order.get(m, -1) >= order[n] for m in vrefs):
                candidates.discard(n)
                changed = True
        if not changed:
            break
    if not (candidates & own):
        return set()  # nothing the class directly reads — ownerless constants stay
    return candidates


class _RewriteMovedConstRefs(cst.CSTTransformer):
    """Method-body references to a constant that moved into the class become
    `Cls.CONST` (a bare name does not resolve through the method scope);
    class-body references stay bare (the class scope resolves them), and
    binding positions are never touched."""

    def __init__(self, wrapper: cst.MetadataWrapper, moved: set[str], cls_name: str) -> None:
        self.wrapper = wrapper
        self.moved = moved
        self.cls_name = cls_name

    @override
    def leave_Name(self, original_node, updated_node) -> cst.BaseExpression:
        if updated_node.value not in self.moved:
            return updated_node
        try:
            kind = _name_ref_kind(self.wrapper, original_node)
        except KeyError:
            return updated_node  # a freshly inserted node — not a pre-split reference
        if kind != "deferred":
            return updated_node
        return cst.Attribute(value=cst.Name(self.cls_name), attr=updated_node)


def _is_docstring_stmt(
    stmt: cst.BaseSmallStatement | cst.BaseStatement,
) -> TypeGuard[cst.SimpleStatementLine]:
    return (
        isinstance(stmt, cst.SimpleStatementLine)
        and len(stmt.body) == 1
        and isinstance(stmt.body[0], cst.Expr)
        and isinstance(stmt.body[0].value, (cst.SimpleString, cst.ConcatenatedString))
    )


def _class_with_moved_consts(
    wrapper: cst.MetadataWrapper, cls: cst.ClassDef, moved: set[str], cls_name: str
) -> cst.ClassDef:
    """Insert the moved constants as class attributes (in their original
    source order, after the docstring) and rewrite the method-body references
    to them as `Cls.CONST`."""
    if not moved:
        return cls
    attrs = [s for n, s in _module_constants(wrapper.module.body) if n in moved]
    body = list(cls.body.body)
    insert = 1 if body and _is_docstring_stmt(body[0]) else 0
    new_body = body[:insert] + attrs + body[insert:]
    cls2 = cls.with_changes(body=cls.body.with_changes(body=new_body))
    return cast(cst.ClassDef, cls2.visit(_RewriteMovedConstRefs(wrapper, moved, cls_name)))


class _SplitCtx(NamedTuple):
    """Everything a class-file build needs to route the class's free names to
    the right imports: the origin identity, the other subjects, the residual
    members, the constants moving in — and the layout's import forms."""

    stem: str

    subjects: set[str]
    residual_def_names: set[str]
    moved: set[str]
    package: str | None  # None = flat sibling; else the package directory
    origin_dots: int     # relative dots for `from <origin> import X`
    member_dots: int     # relative dots for `from .<Class> import <Class>`


def _from_import_node(
    module_comps: list[str], names: list[str] | list[cst.ImportAlias], dots: int
) -> cst.ImportFrom:
    """`from <module> import <names>` with `dots` relative dots; module
    components are a dotted Name/Attribute chain when present."""
    module: cst.BaseExpression | None = None
    if module_comps:
        module = cst.Name(module_comps[0])
        for part in module_comps[1:]:
            module = cst.Attribute(value=module, attr=cst.Name(part))
    aliases = [
        a if isinstance(a, cst.ImportAlias) else cst.ImportAlias(name=cst.Name(a))
        for a in names
    ]
    if not isinstance(module, (cst.Name, cst.Attribute)):
        module = cst.Name(str(module))
    return cst.ImportFrom(
        module=module,
        names=aliases,
        relative=[cst.Dot()] * dots,
        lpar=None,
        rpar=None,
    )


def _split_class_source(
    module: cst.Module,
    wrapper: cst.MetadataWrapper,
    cls: cst.ClassDef,
    ctx: _SplitCtx,
) -> str:
    """One moved class's new file: the imports it needs (verbatim copies of
    the origin's imports binding its free names; a from-origin import for a
    residual member; a from-`.Class` import for a sibling subject) + the
    class itself with its constants and references rewritten."""
    free = _class_free_names(cls) - ctx.moved - _BUILTINS
    imports = _moved_imports(module, free)  # verbatim copies for import-bound names
    constructed: list = []
    for n in sorted(free):
        if n in ctx.subjects and n != cls.name.value:
            if ctx.package is None:
                # unreachable via the cohesion edges (a reference would merge
                # the components) — emit the sibling-file form anyway
                constructed.append(_from_import_node([_snake_name(n)], [n], ctx.origin_dots))
            else:
                constructed.append(_from_import_node([n], [n], ctx.member_dots))
        elif n in ctx.residual_def_names:
            if ctx.package is None:
                constructed.append(_from_import_node([ctx.stem], [n], ctx.origin_dots))
            else:
                constructed.append(_from_import_node([], [n], 1))  # from . import X
    seen: set[str] = set()
    body: list = []
    for stmt in imports + [cst.SimpleStatementLine(body=[i]) for i in constructed]:
        code = cst.Module(body=[stmt]).code
        if code in seen:
            continue
        seen.add(code)
        body.append(stmt)
    cls_node = _class_with_moved_consts(wrapper, cls, ctx.moved, cls.name.value)
    return cst.Module(body=body + [cls_node]).code


def _dotted_expr(parts: list[str]) -> cst.Name | cst.Attribute:
    if len(parts) == 1:
        return cst.Name(parts[0])
    expr: cst.Name | cst.Attribute = cst.Name(parts[0])
    for part in parts[1:]:
        expr = cst.Attribute(value=expr, attr=cst.Name(part))
    return expr


def _resolve_dotted(expr) -> list[str] | None:
    """A plain Name/Attribute chain's dotted components, or None."""
    parts: list[str] = []
    cur = expr
    while isinstance(cur, cst.Attribute):
        parts.append(cur.attr.value)
    if isinstance(cur, cst.Name):
        parts.append(cur.value)
        return list(reversed(parts))
    return None


def _module_alias_map(module: cst.Module, origin_path: list[str]) -> dict[str, list[str]]:
    """`import <origin> [as x]` bindings in one file: the bound name -> the
    origin's dotted path (for attribute-base resolution)."""
    out: dict[str, list[str]] = {}
    for stmt in module.body:
        if not isinstance(stmt, cst.SimpleStatementLine):
            continue
        for inner in stmt.body:
            if not isinstance(inner, cst.Import):
                continue
            for alias in inner.names:
                parts = _resolve_dotted(alias.name)
                if parts is None:
                    continue
                if parts != origin_path:
                    continue
                bound = alias.asname.name if alias.asname is not None else parts[0]
                out[str(bound)] = list(parts)
    return out


def _import_refers_to_origin(
    ifrom: cst.ImportFrom, importer_dir: list[str], origin_path: list[str]
) -> bool:
    """Does this ImportFrom resolve to the origin module (absolute full path,
    relative dots resolving to the origin's directory)?"""
    comps = _resolve_dotted(ifrom.module) if ifrom.module is not None else []
    if comps is None:
        return False
    if ifrom.relative:
        dots = len(ifrom.relative)
        prefix = len(importer_dir) - (dots - 1)
        if prefix < 0:
            return False
        return importer_dir[:prefix] + comps == origin_path
    return comps == origin_path


class _SplitImportRewrite(cst.CSTTransformer):
    """The FLAT/sub-package repo-wide rewrite: `from prod_mod import Cls`
    splices per subject into `from <new> import Cls` (the other aliases stay
    on the origin), and `prod_mod.Cls` attribute chains become `<new>.Cls`,
    registering the retargeted modules for `import <new>` injection."""

    def __init__(
        self,
        origin_path: list[str],
        targets: dict[str, list[str]],
        alias_map: dict[str, list[str]],
        importer_dir: list[str],
    ) -> None:
        self.origin_path = origin_path
        self.targets = targets
        self.alias_map = alias_map
        self.importer_dir = importer_dir
        self.added: set[str] = set()

    def _base_is_origin(self, parts: list[str]) -> bool:
        if parts == self.origin_path:
            return True
        return (
            len(parts) == 1
            and parts[0] in self.alias_map
            and self.alias_map[parts[0]] == self.origin_path
        )

    @override
    def leave_SimpleStatementLine(self, original_node, updated_node):
        new_inners: list[cst.BaseSmallStatement] = []
        changed = False
        for inner in updated_node.body:
            if isinstance(inner, cst.ImportFrom) and not isinstance(inner.names, cst.ImportStar):
                spliced = self._splice_import_from(inner)
                if spliced is not None:
                    new_inners.extend(spliced)
                    changed = True
                    continue
            new_inners.append(inner)
        if not changed:
            return updated_node
        return updated_node.with_changes(body=new_inners)

    def _splice_import_from(self, ifrom: cst.ImportFrom) -> list[cst.ImportFrom] | None:
        if not _import_refers_to_origin(ifrom, self.importer_dir, self.origin_path):
            return None
        if isinstance(ifrom.names, cst.ImportStar):
            return None
        keep: list[cst.ImportAlias] = []
        subj: list[cst.ImportAlias] = []
        for alias in ifrom.names:
            if isinstance(alias.name, cst.Name) and alias.name.value in self.targets:
                subj.append(alias)
            else:
                keep.append(alias)
        if not subj:
            return None
        out: list[cst.ImportFrom] = []
        if keep:
            out.append(ifrom.with_changes(names=keep))
        for alias in subj:
            if not isinstance(alias.name, cst.Name):
                continue
            comps = self.targets[alias.name.value]
            module_comps = comps if ifrom.relative else self.origin_path[:-1] + comps
            # a FRESH alias: the original carries its comma/whitespace from the
            # multi-alias line (`User, Team`) — reusing it renders `User, ` alone
            fresh = cst.ImportAlias(name=alias.name, asname=alias.asname)
            out.append(_from_import_node(module_comps, [fresh], len(ifrom.relative)))
        return out

    @override
    def leave_Attribute(self, original_node, updated_node):
        attr = updated_node.attr.value
        if attr not in self.targets:
            return updated_node
        parts = _resolve_dotted(updated_node.value)
        if parts is None or not self._base_is_origin(parts):
            return updated_node
        full = self.origin_path[:-1] + self.targets[attr]
        self.added.add(".".join(full))
        return updated_node.with_changes(value=_dotted_expr(full))


def _insert_header_imports(body: list, additions: list) -> list:
    """Insert the added imports after any module docstring — imports must sit
    before the statements that reference the names they bind."""
    i = 1 if body and _is_docstring_stmt(body[0]) else 0
    return body[:i] + additions + body[i:]


def _split_rewrite_source(
    text: str,
    p_rel: str,
    origin_path: list[str],
    targets: dict[str, list[str]],
) -> str | None:
    """One file's rewrite for the split: retargeted from-imports, retargeted
    `old.Cls` attribute chains, and the `import <new>` injection for those
    chains. None when the file is unchanged."""
    try:
        module = cst.parse_module(text)
    except Exception:  # parse errors in other files are their own findings
        return None
    alias_map = _module_alias_map(module, origin_path)
    importer_dir = p_rel.split("/")[:-1]
    tx = _SplitImportRewrite(origin_path, targets, alias_map, importer_dir)
    new = module.visit(tx).code
    if tx.added:
        existing: set[str] = set()
        for stmt in module.body:
            if not isinstance(stmt, cst.SimpleStatementLine):
                continue
            for inner in stmt.body:
                if isinstance(inner, cst.Import):
                    for alias in inner.names:
                        parts = _resolve_dotted(alias.name)
                        if parts is not None:
                            existing.add(".".join(parts))
        if tx.added - existing:
            new = _add_module_imports(cst.parse_module(new), tx.added - existing).code
    return new if new != text else None


def _add_module_imports(module: cst.Module, dotted_paths: set[str]) -> cst.Module:
    """`import <dotted>` statements for the retargeted attribute-chain bases,
    inserted after the docstring — `prod_mod.User` became `user.User`, which
    needs the module binding."""
    lines = [
        cst.SimpleStatementLine(
            body=[cst.Import(names=[cst.ImportAlias(name=_dotted_expr(p.split(".")))])]
        )
        for p in sorted(dotted_paths)
    ]
    return module.with_changes(body=_insert_header_imports(list(module.body), lines))



def _const_imported_elsewhere(
    repo: Path, rel: str, module: cst.Module, name: str
) -> bool:
    """Is the constant imported or attribute-read from another file? A moved
    constant must be sole-owned by its class (BQ6) — an external reader would
    break."""
    origin_path = rel.split("/")[:-1] + [rel.rsplit("/", 1)[-1][:-3]]
    for path in _py_files(repo):
        if path.relative_to(repo).as_posix() == rel:
            continue
        try:
            mod = cst.parse_module(path.read_text(encoding="utf-8"))
        except Exception:
            continue
        importer_dir = path.relative_to(repo).as_posix().split("/")[:-1]
        alias_map = _module_alias_map(mod, origin_path)

        probe = _ConstImportScan(importer_dir, alias_map, origin_path, name)
        mod.visit(probe)
        if probe.hit:
            return True
    return False


class _ConstImportScan(cst.CSTVisitor):
    """One file's external readers of a constant: `from <origin> import C`
    (or a star import) and `<origin>.C` attribute chains."""

    def __init__(
        self,
        importer_dir: list[str],
        alias_map: dict[str, list[str]],
        origin_path: list[str],
        name: str,
    ) -> None:
        self.importer_dir = importer_dir
        self.alias_map = alias_map
        self.origin_path = origin_path
        self.name = name
        self.hit = False

    @override
    def visit_ImportFrom(self, node) -> None:
        if not _import_refers_to_origin(node, self.importer_dir, self.origin_path):
            return
        if isinstance(node.names, cst.ImportStar) or any(
            isinstance(a.name, cst.Name) and a.name.value == self.name
            for a in node.names
        ):
            self.hit = True

    @override
    def visit_Attribute(self, node) -> None:
        if node.attr.value != self.name:
            return
        parts = _resolve_dotted(node.value)
        if parts is None:
            return
        if parts == self.origin_path or (
            len(parts) == 1
            and parts[0] in self.alias_map
            and self.alias_map[parts[0]] == self.origin_path
        ):
            self.hit = True


def _module_star_imported(repo: Path, rel: str, module: cst.Module) -> bool:
    """Does any other file `from <origin> import *`? The flat rewrite cannot
    retarget a star import — the moved classes would silently vanish from it."""
    origin_path = rel.split("/")[:-1] + [rel.rsplit("/", 1)[-1][:-3]]
    for path in _py_files(repo):
        if path.relative_to(repo).as_posix() == rel:
            continue
        try:
            mod = cst.parse_module(path.read_text(encoding="utf-8"))
        except Exception:
            continue
        importer_dir = path.relative_to(repo).as_posix().split("/")[:-1]
        for stmt in mod.body:
            if not isinstance(stmt, cst.SimpleStatementLine):
                continue
            for inner in stmt.body:
                if (
                    isinstance(inner, cst.ImportFrom)
                    and isinstance(inner.names, cst.ImportStar)
                    and _import_refers_to_origin(inner, importer_dir, origin_path)
                ):
                    return True
    return False


def _origin_imported_anywhere(
    repo: Path, sources: dict[str, str], rel: str, origin_path: list[str]
) -> bool:
    """BQ6(f): an emptied origin is deleted only when no import statement in
    the produced repo names it anymore."""
    for p_rel, source in sources.items():
        if p_rel == rel:
            continue
        try:
            mod = cst.parse_module(source)
        except Exception:
            continue
        importer_dir = p_rel.split("/")[:-1]
        for stmt in mod.body:
            if not isinstance(stmt, cst.SimpleStatementLine):
                continue
            for inner in stmt.body:
                if isinstance(inner, cst.ImportFrom) and not isinstance(
                    inner.names, cst.ImportStar
                ):
                    if _import_refers_to_origin(inner, importer_dir, origin_path):
                        return True
                elif isinstance(inner, cst.Import):
                    for alias in inner.names:
                        parts = _resolve_dotted(alias.name)
                        if parts == origin_path:
                            return True
    return False


def _graph_cycle(nodes: list[str], edges: dict[str, set[str]]) -> list[str] | None:
    """A directed cycle as the node path, or None — the import-cycle refusal
    for the produced class files."""
    state: dict[str, int] = {}

    def dfs(n: str, path: list[str]) -> list[str] | None:
        state[n] = 1
        path.append(n)
        for nxt in sorted(edges[n]):
            if state.get(nxt) == 1:
                cyc = path[path.index(nxt) :] + [nxt]
                return cyc
            if state.get(nxt) is None:
                r = dfs(nxt, path)
                if r:
                    return r
        path.pop()
        state[n] = 2
        return None

    for n in nodes:
        if state.get(n) is None:
            r = dfs(n, [])
            if r:
                return r
    return None


def _module_empty(new_source: str) -> bool:
    mod = cst.parse_module(new_source)
    return all(
        not isinstance(s, (cst.FunctionDef, cst.ClassDef, cst.SimpleStatementLine)) for s in mod.body
    )

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
                bound |=  _hoist_target_names(t.target)
        elif isinstance(inner, cst.AnnAssign) and inner.target is not None:
            bound |=  _hoist_target_names(inner.target)
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
        self._add_params(node.params.posonly_params)
        self._add_params(node.params.params)
        self._add_params(node.params.kwonly_params)
        # a bare `*` keyword-only separator is ParamStar — no name; the
        # guard must check the NODE type before touching `.name` (review
        # finding: the isinstance on `.name` crashed first)
        if isinstance(node.params.star_arg, cst.Param):
            self.bound.add(node.params.star_arg.name.value)
        if isinstance(node.params.star_kwarg, cst.Param):
            self.bound.add(node.params.star_kwarg.name.value)

    @override
    def visit_Lambda(self, node) -> None:
        self._add_params(node.params.posonly_params)
        self._add_params(node.params.params)
        self._add_params(node.params.kwonly_params)

    @override
    def visit_Name(self, node) -> None:
        self.names.add(node.value)

    @override
    def visit_Assign(self, node) -> None:
        for t in node.targets:
            self.bound.update(_hoist_target_names(t.target))

    @override
    def visit_AnnAssign(self, node) -> None:
        if node.target:
            self.bound.update(_hoist_target_names(node.target))

    @override
    def visit_For(self, node) -> None:
        self.bound.update(_hoist_target_names(node.target))

    @override
    def visit_CompFor(self, node) -> None:
        self.bound.update(_hoist_target_names(node.target))

    @override
    def visit_With(self, node) -> None:
        for item in node.items:
            if item.asname is not None:
                self.bound.update(_hoist_target_names(item.asname.name))

    @override
    def visit_ExceptHandler(self, node) -> None:
        # libcst's ExceptHandler.name is an AsName — the alias is `.name`,
        # not `.value` (review finding: a crash on any `except ... as e:`)
        if isinstance(node.name, cst.AsName) and isinstance(node.name.name, cst.Name):
            self.bound.add(node.name.name.value)


class _EnclosingFn(cst.CSTVisitor):
    """The innermost FunctionDef whose span contains the line — the member
    assignment's enclosing constructor, for the annotation inference."""

    METADATA_DEPENDENCIES = (PositionProvider,)

    def __init__(self, line: int) -> None:
        self.line: int = line
        self.found: cst.FunctionDef | None = None

    @override
    def visit_FunctionDef(self, node) -> None:
        pos = _as_range(self.get_metadata(PositionProvider, node))
        if pos.start.line <= self.line <= pos.end.line:
            self.found = node


class _FindFnLine(cst.CSTVisitor):
    """Locate the first FunctionDef whose def line equals the target — its
    node comes from the wrapper's (metadata-resolvable) tree."""

    METADATA_DEPENDENCIES = (PositionProvider,)

    def __init__(self, line: int) -> None:
        self.line: int = line
        self.found: cst.FunctionDef | None = None

    @override
    def visit_FunctionDef(self, node) -> None:
        if self.found is None and _as_range(self.get_metadata(PositionProvider, node)).start.line == self.line:
            self.found = node


# --------------------------------------------------------------------------- dispatch-registry


class _BoundNames(cst.CSTVisitor):
    """Names ASSIGNED inside a node — the arm's locals (not free vars)."""

    def __init__(self) -> None:
        self.bound: set[str] = set()

    @override
    def visit_Assign(self, node: cst.Assign) -> None:
        for t in node.targets:
            self.bound.update(_hoist_target_names(t.target))

    @override
    def visit_AnnAssign(self, node: cst.AnnAssign) -> None:
        if node.target:
            self.bound.update(_hoist_target_names(node.target))

    @override
    def visit_For(self, node: cst.For) -> None:
        self.bound.update(_hoist_target_names(node.target))

    @override
    def visit_CompFor(self, node: cst.CompFor) -> None:
        self.bound.update(_hoist_target_names(node.target))

    @override
    def visit_With(self, node: cst.With) -> None:
        for item in node.items:
            if item.asname is not None:
                self.bound.update(_hoist_target_names(item.asname.name))




# ambient names a dispatch arm may read without a handler parameter: the
# builtins + underscore dunders (the module's imports are a hand-apply case)
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
_DispatchArm = tuple[str, str | bytes, list[cst.BaseStatement]]
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
    tail = body[first_if + len(chain) :]
    if (
        len(tail) != 1
        or not isinstance(tail[0], cst.SimpleStatementLine)
        or not isinstance(tail[0].body[0], cst.Return)
    ):
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
            body=[cst.Return(cst.Call(func=cst.Name("handler"), args=[cst.Arg(cst.Name(v)) for v in union]))]
        ),
    ]


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
        if (
            isinstance(a, cst.Assign)
            and isinstance(a.value, cst.List)
            and len(a.value.elements) == 0
            and isinstance(a.targets[0].target, cst.Name)
        ):
            return a.targets[0].target.value, i
        if (
            isinstance(a, cst.AnnAssign)
            and isinstance(a.value, cst.List)
            and len(a.value.elements) == 0
            and isinstance(a.target, cst.Name)
        ):
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
    if (
        not isinstance(call.func, cst.Attribute)
        or call.func.attr.value != "append"
        or not isinstance(call.func.value, cst.Name)
        or call.func.value.value != acc
        or len(call.args) != 1
    ):
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


# ===========================================================================
# The shape-routed extract-class arms: wide-tuple / data-clump / partition.
#
# Their wire record carries only (kind, file, line) — no seam_members, no
# message — so each arm RECOVERS the shape from the source at the anchor line
# and declines with a specific reason when it cannot be resolved safely:
# never a silent no-op, never a partial write.
# ===========================================================================


def _pascal_case(text: str) -> str:
    """`folder_fingerprint` -> `FolderFingerprint` (the derived class name)."""
    return "".join(part[:1].upper() + part[1:] for part in re.split(r"[^0-9A-Za-z]+", text) if part)


def _snake_case(text: str) -> str:
    """`FolderFingerprint` -> `folder_fingerprint` (the threaded parameter)."""
    return re.sub(r"(?<=[a-z0-9])(?=[A-Z])", "_", text).lower()


def _annotation_text(expr: cst.BaseExpression) -> str:
    """The annotation's generated text, so two spellings of one shape compare
    equal."""
    return cst.Module(body=[]).code_for_node(expr)
class _BindsName(cst.CSTVisitor):
    """Does the module already bind `name` (class, def, or assignment target)?
    A generated class of that name would shadow or duplicate it."""

    def __init__(self, name: str) -> None:
        self.name: str = name
        self.found: bool = False

    @override
    def visit_ClassDef(self, node) -> None:
        if node.name.value == self.name:
            self.found = True

    @override
    def visit_FunctionDef(self, node) -> None:
        if node.name.value == self.name:
            self.found = True

    @override
    def visit_Assign(self, node) -> None:
        for target in node.targets:
            if isinstance(target.target, cst.Name) and target.target.value == self.name:
                self.found = True


def _binds_name(module: cst.Module, name: str) -> bool:
    finder = _BindsName(name)
    module.visit(finder)
    return finder.found


# --------------------------------------------------------------------------- wide-tuple


class _WideTupleSite(NamedTuple):
    """One fixed-arity tuple annotation on the anchor line: where it lives,
    the annotated name, its arity, and the build site's element expressions
    (None when the annotation has no matching tuple literal)."""

    where: str
    var: str
    arity: int
    annotation: cst.BaseExpression
    elements: tuple[cst.BaseExpression, ...] | None


class _FindWideTuples(cst.CSTVisitor):
    """Every wide-tuple annotation anchored on the finding's line — the fixer
    declines when the line carries more than one (it cannot pick)."""

    METADATA_DEPENDENCIES = (PositionProvider,)

    def __init__(self, line: int) -> None:
        self.line: int = line
        self.sites: list[_WideTupleSite] = []

    @override
    def visit_AnnAssign(self, node) -> None:
        if _as_range(self.get_metadata(PositionProvider, node)).start.line != self.line:
            return
        if not isinstance(node.target, cst.Name):
            return
        arity = _fixed_tuple_arity(node.annotation.annotation)
        if arity is None or arity < 3:
            return
        elements = None
        if isinstance(node.value, cst.Tuple) and len(node.value.elements) == arity and all(
            isinstance(element, cst.Element) for element in node.value.elements
        ):
            elements = tuple(element.value for element in node.value.elements)
        self.sites.append(
            _WideTupleSite("assign", node.target.value, arity, node.annotation.annotation, elements)
        )

    @override
    def visit_FunctionDef(self, node) -> None:
        if _as_range(self.get_metadata(PositionProvider, node)).start.line != self.line:
            return
        for param in [*node.params.posonly_params, *node.params.params, *node.params.kwonly_params]:
            if param.annotation is None:
                continue
            arity = _fixed_tuple_arity(param.annotation.annotation)
            if arity is not None and arity >= 3:
                self.sites.append(
                    _WideTupleSite("param", param.name.value, arity, param.annotation.annotation, None)
                )
        if node.returns is not None:
            arity = _fixed_tuple_arity(node.returns.annotation)
            if arity is not None and arity >= 3:
                self.sites.append(
                    _WideTupleSite("return", node.name.value, arity, node.returns.annotation, None)
                )


def _fixed_tuple_arity(node: cst.BaseExpression | None) -> int | None:
    """The arity of a fixed-arity `tuple[...]`/`Tuple[...]` annotation; None
    for anything else (including the variadic `tuple[int, ...]`)."""
    if not isinstance(node, cst.Subscript) or not (
        isinstance(node.value, cst.Name) and node.value.value in ("tuple", "Tuple")
    ):
        return None
    elements = node.slice if isinstance(node.slice, tuple) else (node.slice,)
    for element in elements:
        if not isinstance(element, cst.SubscriptElement):
            return None
        index = element.slice
        if not isinstance(index, cst.Index) or isinstance(index.value, cst.Ellipsis):
            return None
    return len(elements)


def _record_field_names(elements: tuple[cst.BaseExpression, ...] | None, arity: int) -> list[str]:
    """The record's field names: a Name element keeps its name, an attribute
    access takes the attribute (the value IS that field), and a literal, a
    keyword, or a duplicate falls back to `f<i>` — the agent renames what the
    shape alone cannot name."""
    names: list[str] = []
    for i in range(arity):
        element = elements[i] if elements is not None and i < len(elements) else None
        name = ""
        if isinstance(element, cst.Name):
            name = element.value
        elif isinstance(element, cst.Attribute):
            name = element.attr.value
        if not name.isidentifier() or keyword.iskeyword(name) or name in names:
            name = f"f{i}"
        names.append(name)
    return names


class _WideTupleToRecord(cst.CSTTransformer):
    """Retarget every annotation in the file spelling the same tuple shape at
    the record class, and every matching tuple build site at its constructor
    — the shape (arity + element types) is the proof of sameness. A function
    annotated as returning the tuple has its own matching return/assignment
    literals retargeted too (position proves them); elsewhere nothing
    changes."""

    METADATA_DEPENDENCIES = (ParentNodeProvider,)

    def __init__(self, ann_text: str, arity: int, class_name: str) -> None:
        self.ann_text: str = ann_text
        self.arity: int = arity
        self.class_name: str = class_name
        self._returns_shape: list[bool] = []

    def _matches(self, annotation: cst.Annotation) -> bool:
        return _annotation_text(annotation.annotation) == self.ann_text

    def _retarget(self, annotation: cst.Annotation) -> cst.Annotation:
        return annotation.with_changes(annotation=cst.Name(self.class_name))

    def _constructor(self, value: cst.BaseExpression) -> cst.BaseExpression | None:
        if not isinstance(value, cst.Tuple) or len(value.elements) != self.arity:
            return None
        args = [cst.Arg(element.value) for element in value.elements if isinstance(element, cst.Element)]
        if len(args) != self.arity:
            return None
        return cst.Call(func=cst.Name(self.class_name), args=args)

    @override
    def leave_AnnAssign(self, original_node, updated_node):
        if not self._matches(updated_node.annotation):
            return updated_node
        annotation = self._retarget(updated_node.annotation)
        if updated_node.value is None:
            return updated_node.with_changes(annotation=annotation)
        value = self._constructor(updated_node.value)
        if value is None:
            return updated_node.with_changes(annotation=annotation)
        return updated_node.with_changes(annotation=annotation, value=value)

    @override
    def leave_Param(self, original_node, updated_node):
        if updated_node.annotation is None or not self._matches(updated_node.annotation):
            return updated_node
        return updated_node.with_changes(annotation=self._retarget(updated_node.annotation))

    @override
    def visit_FunctionDef(self, node) -> None:
        self._returns_shape.append(node.returns is not None and self._matches(node.returns))

    @override
    def leave_FunctionDef(self, original_node, updated_node):
        self._returns_shape.pop()
        if updated_node.returns is None or not self._matches(updated_node.returns):
            return updated_node
        return updated_node.with_changes(returns=self._retarget(updated_node.returns))

    @override
    def leave_Tuple(self, original_node, updated_node):
        if not self._returns_shape or not self._returns_shape[-1]:
            return updated_node
        parent = self.get_metadata(ParentNodeProvider, original_node)
        if not isinstance(parent, (cst.Return, cst.Assign, cst.AnnAssign)):
            return updated_node
        return self._constructor(updated_node) or updated_node


# lucidlint: ignore strewing these free functions ARE the shape router: one per family, dispatched by kind
def _wide_tuple_fix(req: _FixRequest) -> str | None:
    """Introduce the record class for the flagged fixed-arity tuple: the
    annotation (and every same-shape annotation in the file) becomes the
    class, matching tuple build sites construct it, and the class is
    prepended. Declines on an ambiguous anchor, an unusable name, or a name
    the file already binds."""
    source, line = req._loaded_source(), req.line
    wrapper = cst.MetadataWrapper(cst.parse_module(source))
    finder = _FindWideTuples(line)
    wrapper.visit(finder)
    if not finder.sites:
        req.decline = (
            f"no fixed-arity tuple annotation (3+ named positions) at {req.rel}:{line} — "
            "the anchor is stale, or the annotation is variadic (tuple[T, ...])"
        )
        return None
    if len(finder.sites) > 1:
        req.decline = (
            f"{len(finder.sites)} wide-tuple annotations anchor at {req.rel}:{line} — the "
            "anchor is ambiguous; put each on its own line, or retarget one by hand"
        )
        return None
    site = finder.sites[0]
    # a bare ANNOTATION carries no variable to name the record after: the
    # parameter's own name, or `<Fn>Result` for a return annotation
    derived = _pascal_case(site.var) + ("" if site.where != "return" else "Result")
    class_name = req.opts.name or derived
    if not class_name.isidentifier() or keyword.iskeyword(class_name):
        req.decline = f"'{class_name}' is not a usable record class name — pass --name <Name>"
        return None
    if _binds_name(wrapper.module, class_name):
        req.decline = f"'{class_name}' is already bound in {req.rel} — pass another --name"
        return None
    fields = _record_field_names(site.elements, site.arity)
    transformed = wrapper.visit(_WideTupleToRecord(_annotation_text(site.annotation), site.arity, class_name))
    body: list = [_record_class_def(class_name, fields), cst.EmptyLine(), *transformed.body]
    return cst.Module(body=body).code


# --------------------------------------------------------------------------- data-clump


class _Clump(NamedTuple):
    """The recovered clump: the shared pair (in the anchor's signature order)
    and the module-level defs carrying it, in source order."""

    pair: tuple[str, str]
    fns: tuple[cst.FunctionDef, ...]


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
        finder = _ImportedNames()
        tree.visit(finder)
        if finder.names & names:
            return str(path.relative_to(repo))
    return None


def _plain_args(fn: cst.FunctionDef) -> list[str]:
    """The regular positional parameter names — the only set the scanner's
    data-clump rule pairs over."""
    return [param.name.value for param in fn.params.params]


# lucidlint: ignore complexity the shape analyzers dispatch over libcst node kinds — splitting scatters one table
def _clump_at(module: cst.Module, line: int, positions) -> _Clump | None:
    """The clump anchored at `line`: the module-level defs sharing a regular-
    parameter pair (>= 3 of them, the scanner's rule), the pair in the
    anchor's signature order. None when no pair qualifies there (a stale
    anchor)."""
    fns = [stmt for stmt in module.body if isinstance(stmt, cst.FunctionDef)]
    pairs: dict[tuple[str, str], list[cst.FunctionDef]] = {}
    for fn in fns:
        names = _plain_args(fn)
        for i in range(len(names)):
            for j in range(i + 1, len(names)):
                left, right = names[i], names[j]
                key: tuple[str, str] = (left, right) if left <= right else (right, left)
                pairs.setdefault(key, []).append(fn)
    anchor = next((f for f in fns if _as_range(positions[f]).start.line == line), None)
    if anchor is None:
        return None
    qualifying = sorted(pair for pair, group in pairs.items() if len(group) >= 3 and anchor in group)
    if not qualifying:
        return None
    pair = qualifying[0]
    order = _plain_args(anchor)
    kept = {id(f) for f in pairs[pair]}
    ordered = tuple(sorted(pair, key=order.index))
    return _Clump((ordered[0], ordered[1]), tuple(f for f in fns if id(f) in kept))


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

    def _own_params(self, node: cst.FunctionDef) -> set[str]:
        return {
            p.name.value
            for p in [*node.params.posonly_params, *node.params.params, *node.params.kwonly_params]
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
            params=updated_node.params.with_changes(
                params=[cst.Param(cst.Name(self.param_name)), *params]
            )
        )

    @override
    def leave_Name(self, original_node, updated_node):
        fn = self._current or ""
        if fn not in self.index or updated_node.value not in self.pair:
            return updated_node
        if updated_node.value in self._shadow_stack[-1]:
            return updated_node
        parent = self.get_metadata(ParentNodeProvider, original_node)
        if _is_keyword_name(original_node, parent):
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
                f"the call to '{fn}' does not pass both '{self.pair[0]}' and '{self.pair[1]}' "
                "— thread the pair by hand"
            )
            return updated_node
        a_val, b_val = a_arg.value, b_arg.value
        if (
            isinstance(a_val, cst.Attribute)
            and isinstance(b_val, cst.Attribute)
            and isinstance(a_val.value, cst.Name)
            and isinstance(b_val.value, cst.Name)
            and a_val.value.value == self.param_name
            and b_val.value.value == self.param_name
            and (a_val.attr.value, b_val.attr.value) == self.pair
        ):
            # the caller already holds the instance (a forwarded pair) — pass
            # it on instead of rebuilding it from its own fields
            replacement: cst.BaseExpression = cst.Name(self.param_name)
        else:
            replacement = cst.Call(
                func=cst.Name(self.class_name), args=[cst.Arg(a_val), cst.Arg(b_val)]
            )
        # the untouched args keep their own nodes: their comments and
        # formatting survive the rewrite
        kept = [arg for i, arg in enumerate(pos_args) if i not in (idx_a, idx_b)]
        kept.insert(min(idx_a, idx_b), cst.Arg(replacement))
        new_args = list(kept)
        new_args.extend(
            arg
            for arg in updated_node.args
            if arg.keyword is not None and arg.keyword.value not in self.pair
        )
        return updated_node.with_changes(args=new_args)


# lucidlint: ignore complexity the shape analyzers dispatch over libcst node kinds — splitting scatters one table
def _data_clump_fix(req: _FixRequest) -> str | None:
    """Thread the clump's shared pair through one parameter object: the class
    is prepended, the clump's signatures and bodies change, and module-scope
    call sites construct the instance. Declines when the pair cannot be
    recovered, a caller lives in another module, or a call cannot be
    resolved."""
    source, line = req._loaded_source(), req.line
    # the wrapper's own tree: metadata resolves by NODE IDENTITY, so the
    # module inspected here must be the wrapper's copy, not a parallel parse
    wrapper = cst.MetadataWrapper(cst.parse_module(source))
    module = wrapper.module
    clump = _clump_at(module, line, wrapper.resolve(PositionProvider))
    if clump is None:
        req.decline = (
            f"no parameter pair shared by 3+ module functions is anchored at {req.rel}:{line} "
            "— the anchor is stale, or the clump is gone"
        )
        return None
    pair_a, pair_b = clump.pair
    index: dict[str, tuple[int, int]] = {}
    for fn in clump.fns:
        if fn.params.posonly_params or isinstance(fn.params.star_arg, cst.Param) or fn.params.star_kwarg:
            req.decline = (
                f"'{fn.name.value}' has positional-only or variadic parameters — the pair "
                "cannot be threaded mechanically; do it by hand"
            )
            return None
        if any(p.default is not None for p in fn.params.params if p.name.value in clump.pair):
            req.decline = (
                f"'{fn.name.value}' defaults '{pair_a}'/'{pair_b}' — threading would change the "
                "call contract; do it by hand"
            )
            return None
        args = _plain_args(fn)
        if pair_a not in args or pair_b not in args:
            req.decline = f"'{fn.name.value}' does not carry the pair in its plain parameters — thread it by hand"
            return None
        index[fn.name.value] = (args.index(pair_a), args.index(pair_b))
    class_name = req.opts.name or _pascal_case(f"{pair_a}_{pair_b}")
    if not class_name.isidentifier() or keyword.iskeyword(class_name):
        req.decline = f"'{class_name}' is not a usable class name — pass --name <Name>"
        return None
    parameter = _snake_case(class_name)
    if parameter in clump.pair:
        req.decline = f"the threaded parameter '{parameter}' collides with the pair — pass another --name"
        return None
    if _binds_name(module, class_name):
        req.decline = f"'{class_name}' is already bound in {req.rel} — pass another --name"
        return None
    culprit = _imported_elsewhere(req.repo, req.rel, {fn.name.value for fn in clump.fns})
    if culprit:
        req.decline = (
            f"'{culprit}' imports a clump function — its call site is outside this module; "
            "thread the pair by hand"
        )
        return None
    stores = _CollectStores()
    wrapper.visit(stores)
    transformer = _DataClumpToRecord(clump.pair, class_name, parameter, stores.per_fn, index)
    transformed = wrapper.visit(transformer)
    if transformer.decline:
        req.decline = transformer.decline
        return None
    body: list = [_record_class_def(class_name, [pair_a, pair_b]), cst.EmptyLine(), *transformed.body]
    return cst.Module(body=body).code


# --------------------------------------------------------------------------- partition


class _PartitionGroup(NamedTuple):
    """One field-disjoint method group: its methods in source order and the
    fields only they touch."""

    methods: tuple[cst.FunctionDef, ...]
    fields: tuple[str, ...]


class _PartitionPlan(NamedTuple):
    groups: tuple[_PartitionGroup, ...]
    init_fields: tuple[tuple[str, cst.BaseExpression], ...]
    init_docstring: cst.BaseStatement | None


class _PartitionAttempt(NamedTuple):
    """The plan, or the specific reason it cannot be computed."""

    plan: _PartitionPlan | None
    decline: str


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


def _reads_self(expr: cst.BaseExpression) -> bool:
    """Does the expression read `self.<attr>`? An initializer that does would
    have to move between classes in order — not mechanical."""
    return bool(_self_field_reads(expr))


# the __init__ plan: the field initializers in order + the docstring statement
_InitPlan = tuple[list[tuple[str, cst.BaseExpression]], cst.BaseStatement | None]


# lucidlint: ignore complexity the shape analyzers dispatch over libcst node kinds — splitting scatters one table
def _init_plan(init: cst.FunctionDef | None) -> _InitPlan | None:
    """__init__'s `self.x = <expr>` statements (plus its docstring) in order;
    None when the body does anything else — a split would have to reorder or
    retarget that statement."""
    if init is None:
        return [], None
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
        if (
            not isinstance(target, cst.Attribute)
            or not isinstance(target.value, cst.Name)
            or target.value.value != "self"
            or not isinstance(small, cst.Assign)
            or _reads_self(small.value)
        ):
            return None
        assigns.append((target.attr.value, small.value))
    return assigns, docstring


# lucidlint: ignore complexity the shape analyzers dispatch over libcst node kinds — splitting scatters one table
def _partition_plan(cls: cst.ClassDef) -> _PartitionAttempt:
    """The field-disjoint split of `cls`, or the reason it cannot be split —
    connector methods merge the groups and produce the refusal."""
    methods = [stmt for stmt in cls.body.body if isinstance(stmt, cst.FunctionDef)]
    extra = [
        stmt
        for stmt in cls.body.body
        if not isinstance(stmt, cst.FunctionDef) and not _is_docstring_stmt(stmt)
    ]
    if extra:
        return _PartitionAttempt(
            None,
            "the class carries statements besides a docstring — they cannot be attributed to "
            "one group; split by hand",
        )
    if len(methods) < 6:
        return _PartitionAttempt(
            None,
            f"the class has only {len(methods)} methods — not the latent-partition shape (6+); "
            "the anchor is stale",
        )
    init = next((method for method in methods if method.name.value == "__init__"), None)
    parsed = _init_plan(init)
    if parsed is None:
        return _PartitionAttempt(
            None,
            "the class's __init__ does more than assign self fields — a split would reorder or "
            "retarget a statement; split by hand",
        )
    init_fields, init_docstring = parsed
    fielded = [(method, _self_field_reads(method)) for method in methods if method is not init]
    fieldless = [method.name.value for method, fields in fielded if not fields]
    if fieldless:
        return _PartitionAttempt(
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
        return _PartitionAttempt(
            None,
            "the methods share fields across the whole class (a connector method ties the "
            "groups together) — the split would not be field-disjoint; split by hand",
        )
    group_fields = [tuple(sorted({f for name in group for f in fields_of[name]})) for group in groups]
    if any(len(group) < 2 or len(fields) < 2 for group, fields in zip(groups, group_fields, strict=True)):
        return _PartitionAttempt(
            None,
            "a method group is too small (needs 2+ methods and 2+ fields) — the split would not "
            "produce classes; split by hand",
        )
    all_fields = {field for _, fields in fielded for field in fields} | {field for field, _ in init_fields}
    attributed = {field for fields in group_fields for field in fields}
    orphan = sorted(all_fields - attributed)
    if orphan:
        return _PartitionAttempt(
            None,
            f"'{orphan[0]}' is initialized but touched by no method — it belongs to no group; "
            "split by hand",
        )
    plan = _PartitionPlan(
        groups=tuple(
            _PartitionGroup(tuple(fielded[i][0] for i in sorted(order[name] for name in group)), fields)
            for group, fields in zip(groups, group_fields, strict=True)
        ),
        init_fields=tuple(init_fields),
        init_docstring=init_docstring,
    )
    return _PartitionAttempt(plan, "")


def _init_stmt(field: str, value: cst.BaseExpression) -> cst.BaseStatement:
    """One `self.<field> = <value>` line for a group's synthesized __init__."""
    return cst.SimpleStatementLine(
        body=[
            cst.Assign(
                targets=[cst.AssignTarget(cst.Attribute(cst.Name("self"), cst.Name(field)))],
                value=value,
            )
        ]
    )


def _group_class(
    name: str,
    template: cst.ClassDef,
    methods: list[cst.FunctionDef],
    init_stmts: list[cst.BaseStatement],
    head: list[cst.BaseStatement],
) -> cst.ClassDef:
    """One group's class: the template's decorators, bases, and keywords, a
    synthesized __init__ over its own fields, then its methods."""
    body: list[cst.BaseStatement] = list(head)
    if init_stmts:
        body.append(
            cst.FunctionDef(
                name=cst.Name("__init__"),
                params=cst.Parameters(params=[cst.Param(cst.Name("self"))]),
                body=cst.IndentedBlock(body=init_stmts),
            )
        )
    body.extend(methods)
    if isinstance(body[0], (cst.FunctionDef, cst.ClassDef)) and body[0].leading_lines:
        body[0] = body[0].with_changes(leading_lines=[])
    return template.with_changes(name=cst.Name(name), body=cst.IndentedBlock(body=body))


# lucidlint: ignore complexity the shape analyzers dispatch over libcst node kinds — splitting scatters one table
def _partition_fix(req: _FixRequest) -> str | None:
    """Split the field-disjoint class into one class per method group: the
    group's fields move into its synthesized __init__, the first group keeps
    the class's name, and every group keeps the shared bases. Declines when a
    connector method (or unattributable __init__ logic) ties the groups
    together."""
    source, line = req._loaded_source(), req.line
    # the wrapper's own tree: the visitor's nodes must be the ones spliced
    # back into `module.body`, and metadata resolves by NODE IDENTITY
    wrapper = cst.MetadataWrapper(cst.parse_module(source))
    module = wrapper.module
    finder = _EnclosingClass(line)
    wrapper.visit(finder)
    cls = finder.found
    if cls is None or not any(stmt is cls for stmt in module.body):
        req.decline = f"no module-level class anchored at {req.rel}:{line} — the anchor is stale"
        return None
    attempt = _partition_plan(cls)
    if attempt.plan is None:
        req.decline = attempt.decline
        return None
    plan = attempt.plan
    base = req.opts.name or cls.name.value
    if not base.isidentifier() or keyword.iskeyword(base):
        req.decline = f"'{base}' is not a usable class-name prefix — pass --name <Prefix>"
        return None
    class_names = [base]
    for group in plan.groups[1:]:
        class_names.append(base + _pascal_case(group.fields[0]))
    if len(set(class_names)) != len(class_names):
        req.decline = "two groups derive the same class name — pass --name <Prefix> to disambiguate"
        return None
    for name in class_names:
        if name != cls.name.value and _binds_name(module, name):
            req.decline = f"'{name}' is already bound in {req.rel} — pass another --name"
            return None
    class_docstring = next(
        (stmt for stmt in cls.body.body if isinstance(stmt, cst.SimpleStatementLine) and _is_docstring_stmt(stmt)),
        None,
    )
    replacements: list[cst.BaseStatement] = []
    for i, group in enumerate(plan.groups):
        init_stmts: list[cst.BaseStatement] = []
        if i == 0 and plan.init_docstring is not None:
            init_stmts.append(plan.init_docstring)
        init_stmts.extend(
            _init_stmt(field, value) for field, value in plan.init_fields if field in group.fields
        )
        head: list[cst.BaseStatement] = []
        if i == 0 and class_docstring is not None:
            head.append(class_docstring)
        group_cls = _group_class(class_names[i], cls, list(group.methods), init_stmts, head)
        if i:
            group_cls = group_cls.with_changes(leading_lines=[cst.EmptyLine()])
        replacements.append(group_cls)
    body: list[cst.BaseStatement] = list(module.body)
    index = next(i for i, stmt in enumerate(body) if stmt is cls)
    body[index : index + 1] = replacements
    return module.with_changes(body=body).code

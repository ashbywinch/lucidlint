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

import keyword
import re
from collections.abc import Sequence
from dataclasses import dataclass, field
from pathlib import Path
from typing import NamedTuple, cast, override

import libcst as cst
from libcst.metadata import ExpressionContextProvider, ParentNodeProvider, PositionProvider

from fix_engine import records as _records
from fix_engine.cst_mechanics import CstMechanics
from fix_engine.fix_options import FixOptions
from fix_engine.fixers.feature_envy import FeatureEnvy
from fix_engine.fixers.magic_literal import MagicLiteral
from fix_engine.fixers.mechanical_fixes import MechanicalFixes
from fix_engine.fixers.parameter_objects import ParameterObjects
from fix_engine.fixers.record_fixes import RecordFixes
from fix_engine.fixers.renaming import Renaming
from fix_engine.method_extraction import MethodExtraction
from fix_engine.strewing_group import StrewingGroup

_ChainHead = _records._ChainHead
_ChainMiddle = _records._ChainMiddle
_ChainTerminal = _records._ChainTerminal
_Clump = _records._Clump
_ForwardTarget = _records._ForwardTarget
_GroupInit = _records._GroupInit
_ModuleFn = _records._ModuleFn
_ModuleProposal = _records._ModuleProposal
_ModuleRef = _records._ModuleRef
_PartitionAttempt = _records._PartitionAttempt
_PartitionGroup = _records._PartitionGroup
_PartitionPlan = _records._PartitionPlan
_SplitCtx = _records._SplitCtx
_SplitLayout = _records._SplitLayout
_SplitOutcome = _records._SplitOutcome
_WideTupleSite = _records._WideTupleSite
# the shared mechanics stay reachable from the package root: consumers and the
# tests import them from fix_engine
_as_range = CstMechanics._as_range
_as_node = CstMechanics._as_node
_iter_nodes = CstMechanics._iter_nodes
_iter_nodes_pruned = CstMechanics._iter_nodes_pruned
_expr_names = CstMechanics._expr_names
_collect_alias_names = CstMechanics._collect_alias_names
_is_keyword_name = CstMechanics._is_keyword_name
_is_self_attribute = CstMechanics._is_self_attribute
_import_base_name = CstMechanics._import_base_name
_is_docstring_stmt = CstMechanics._is_docstring_stmt
_py_files = CstMechanics._py_files
_record_class_def = CstMechanics._record_class_def
_self_attr_target = CstMechanics._self_attr_target
_hoist_target_names = CstMechanics._hoist_target_names
_extraction_is_private = CstMechanics._extraction_is_private
_RemoveNodes = CstMechanics._RemoveNodes
_EnclosingClass = CstMechanics._EnclosingClass
_EnclosingFn = CstMechanics._EnclosingFn

# the method-extraction mechanics stay reachable from the package root (the tests
# and the scanner-side helpers import them there)
_Analyse = MethodExtraction._Analyse
_DecisionCount = MethodExtraction._DecisionCount
_ExtractMethodRewrite = MethodExtraction._ExtractMethodRewrite
_FnBodyState = MethodExtraction._FnBodyState
_InsertExtractedFn = MethodExtraction._InsertExtractedFn
_chain_operands = MethodExtraction._chain_operands
_for_target_names = MethodExtraction._for_target_names
_is_nested_target = MethodExtraction._is_nested_target
_is_rebound_read = MethodExtraction._is_rebound_read
_min_seam_decisions = MethodExtraction._min_seam_decisions
_module_level_names = MethodExtraction._module_level_names
_stmt_decision_count = MethodExtraction._stmt_decision_count
_target_bound_names = MethodExtraction._target_bound_names
_wildcard = MethodExtraction._wildcard
collect_stmt_names = MethodExtraction.collect_stmt_names


def _non_receiver_param_names(params: Sequence[cst.Param]) -> list[str]:
    """The parameter names a moved method carries, without `self`/`cls`."""
    return [p.name.value for p in params if p.name.value not in ("self", "cls")]


def _module_parts(rel: str) -> list[str]:
    """A module's import path: its directory parts plus the stem."""
    return rel.split("/")[:-1] + [rel.rsplit("/", 1)[-1][:-3]]


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
        "Replace Loop Sequence with Pipeline: the shared-accumulator loop chain becomes concatenated comprehensions"
    ),
    "loop-hoist": (
        "Replace Loop with Hoist + Pipeline: the loop body becomes a per-item helper, the loop becomes a comprehension"
    ),
}


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


# lucidlint: ignore-file global-state the module constants are this tool's configuration tables —
# lucidlint: ignore-file global-state a config class is the eventual home
# lucidlint: ignore-file misplaced-method the repo helpers serve module-level callers too — moving them
# lucidlint: ignore-file misplaced-method onto _FixRequest would strand half
# lucidlint: ignore-file record-shape the libcst layer's tuple/dict shorthands ARE its wire records —
# lucidlint: ignore-file record-shape a class per helper hop is ceremony
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
    wrote: bool = False  # set by fix_finding: the file was actually rewritten
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

    # lucidlint: ignore closures the nested visitors are one-purpose probes; hoisting names nothing owned
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
                    and CstMechanics._as_range(self.get_metadata(PositionProvider, node)).start.line == self.target_line
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
        files = CstMechanics._py_files(self.repo)
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

    # lucidlint: ignore latent-class the chain facts thread through module helpers — _FixRequest is the boundary
    def fix_collapse_chain(self) -> str | None:
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
        module, wrapper = CstMechanics._wrapped_module(self._loaded_source())
        head = _chain_head(wrapper, self.line)
        if head is None:
            return None  # stale, or the method is not a pure forwarder (R28)
        mid = _chain_middle(self.repo, head)
        if mid is None:
            return None  # F is missing or does not forward its own params
        refusal = self._chain_refusal(head, mid)
        if refusal is not None:
            self.decline = refusal
            return None
        if mid.callee.is_name:
            return self._collapse_to_module_fn(module, head, mid)
        return self._collapse_to_method(module, head, mid)

    def _chain_refusal(self, head: _ChainHead, mid: _ChainMiddle) -> str | None:
        """The refusals every chain shape shares: F's only caller is the head,
        no *args/**kwargs passthrough, and no `from mod import F` re-export to
        retarget."""
        f_name = head.callee.name
        if _call_sites(_ModuleRef(self.repo, f_name)) != 1:
            return f"'{f_name}' has more than one caller — collapse by hand"
        if any("*" in p for p in head.params) or any("*" in p for p in mid.params):
            return "the chain passes *args/**kwargs — collapse by hand"
        if _reexported(_ModuleRef(self.repo, f_name)):
            return f"'from ... import {f_name}' re-exports the forwarder — collapse by hand"
        return None

    def _collapse_to_module_fn(self, module: cst.Module, head: _ChainHead, mid: _ChainMiddle) -> str | None:
        """M -> F -> G (a module function): M calls G with its own params and F
        is deleted. None when the two modules are otherwise independent — the
        chain is then the deliberate boundary between them."""
        if not _depends_on(self.repo, self.rel, mid.rel):
            self.decline = "the chain is the only coupling between the modules — a boundary; leave it"
            return None
        body = cst.SimpleStatementLine(
            body=[
                cst.Return(
                    cst.Call(
                        func=cst.Name(mid.callee.name),
                        args=[cst.Arg(cst.Name(p)) for p in head.params],
                    )
                )
            ]
        )
        origin_new = _replace_method_body(module, head.method, body, self.line).code
        # F may live in the origin itself — deleting it from a FRESH parse
        # would overwrite the rewire just applied above
        return self._drop_middle(origin_new, head, mid)

    def _method_terminal(self, head: _ChainHead, mid: _ChainMiddle) -> _ChainTerminal | None:
        """The unique D.N the chain reaches through F. None when the method is
        not unique in the repo, or when the modules are independent."""
        attr_method = mid.callee.attr_method
        if attr_method is None:
            return None
        recv, n_name = attr_method
        arg_params = [p for p in mid.params if p != recv]
        matches = [
            (d_rel, d_cls, d_method)
            for d_rel, d_cls, d_method in _methods_named(_ModuleRef(self.repo, n_name))
            if d_cls.name.value != head.class_name and _non_receiver_param_names(d_method.params.params) == arg_params
        ]
        if len(matches) != 1:
            return None  # ambiguous — no structural certainty
        d_rel, d_cls, d_method = matches[0]
        if not _depends_on(self.repo, self.rel, d_rel):
            self.decline = "the chain is the only coupling between the modules — a boundary; leave it"
            return None
        if _call_sites(_ModuleRef(self.repo, n_name)) > 1:
            self.decline = f"'{n_name}' has other callers beyond the chain — collapse by hand"
            return None
        return _ChainTerminal(recv, n_name, d_rel, d_cls, d_method)

    def _collapse_to_method(self, module: cst.Module, head: _ChainHead, mid: _ChainMiddle) -> str | None:
        """M -> F -> D.N: rewire when D has identity, promote N onto C when D
        dissolves. D's identity decides, not the caller's preference."""
        terminal = self._method_terminal(head, mid)
        if terminal is None:
            return None
        if _class_has_identity(terminal.cls):
            return self._rewire_method_terminal(module, head, mid, terminal)
        return self._promote_method_terminal(module, head, mid, terminal)

    def _rewire_method_terminal(
        self, module: cst.Module, head: _ChainHead, mid: _ChainMiddle, terminal: _ChainTerminal
    ) -> str:
        """D keeps its identity: M reaches D.N directly and F is deleted."""
        args = [p for p in head.params if p != terminal.recv]
        body = cst.SimpleStatementLine(
            body=[
                cst.Return(
                    cst.Call(
                        func=cst.Attribute(value=cst.Name(terminal.recv), attr=cst.Name(terminal.name)),
                        args=[cst.Arg(cst.Name(p)) for p in args],
                    )
                )
            ]
        )
        origin_new = _replace_method_body(module, head.method, body, self.line).code
        return self._drop_middle(origin_new, head, mid)

    def _promote_method_terminal(
        self, module: cst.Module, head: _ChainHead, mid: _ChainMiddle, terminal: _ChainTerminal
    ) -> str | None:
        """D has no identity: N becomes a sibling method of M on C, D dissolves."""
        d_name = terminal.cls.name.value
        if not _zero_external_refs(self.repo, d_name):
            self.decline = f"'{d_name}' still has external references — dissolve D by hand"
            return None
        if _method_uses_self(terminal.method):
            self.decline = f"'{terminal.name}' touches its own instance state — promote by hand"
            return None
        promoted = terminal.method.with_changes(name=cst.Name(terminal.name))
        new_class = head.class_node.with_changes(
            body=head.class_node.body.with_changes(body=[*head.class_node.body.body, promoted])
        )
        origin_new = _replace_class_body(module, self.line, new_class).code
        args = [p for p in head.params if p != terminal.recv]
        m_body = cst.SimpleStatementLine(
            body=[
                cst.Return(
                    cst.Call(
                        func=cst.Attribute(value=cst.Name("self"), attr=cst.Name(terminal.name)),
                        args=[cst.Arg(cst.Name(p)) for p in args],
                    )
                )
            ]
        )
        origin_new = _replace_method_body(cst.parse_module(origin_new), head.method, m_body, self.line).code
        origin_new = self._drop_middle(origin_new, head, mid)
        if terminal.rel != self.rel:
            d_new = _remove_class(_module_of(self.repo, terminal.rel), d_name)
            if _module_empty(d_new):
                self.deletes.append(terminal.rel)
            else:
                self.extra_writes.append((terminal.rel, d_new))
        else:
            # D dissolves in the origin too (zero external refs by the gate)
            origin_new = cst.parse_module(origin_new).visit(_RemoveClass(d_name)).code
        return origin_new

    def _drop_middle(self, origin_new: str, head: _ChainHead, mid: _ChainMiddle) -> str:
        """Delete F once the head no longer calls it: in the origin when F
        lives there, otherwise as a separate write."""
        if mid.rel == self.rel:
            return cst.parse_module(origin_new).visit(_RemoveFn(head.callee.name)).code
        self.extra_writes.append((mid.rel, _remove_fn(mid.module, head.callee.name)))
        return origin_new

    # lucidlint: ignore latent-class the nested visitors are one-purpose probe walks — hoisting names nothing owned
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
        if not self.rel.endswith(".py") or self.rel.endswith("__init__.py"):
            return None  # the scanner never flags __init__ — nothing to split
        module, wrapper = CstMechanics._wrapped_module(self._loaded_source())
        origin = _split_origin(self.rel)
        subjects = _split_subjects(module.body, origin.stem)
        if not subjects:
            return None  # stale — no misplaced public class remains
        refusal = _refuse_split_reordering(module.body, subjects)
        if refusal is not None:
            self.decline = refusal
            return None
        outcome = _ModuleSplit(self.repo, origin, module, subjects).execute(wrapper, self.opts.name)
        if outcome.decline is not None:
            self.decline = outcome.decline
            return None
        self.extra_writes = outcome.writes
        self.deletes.extend(outcome.deletes)
        return outcome.origin

    def _fn_seam_analysis(self):
        """Analyze the function at `line`: its body statements with per-statement
        spans, first-use contexts, writes, and control-flow flags (the method
        extraction's seam analysis)."""
        return MethodExtraction(self)._fn_seam_analysis()

    def extract_method_proposal(self):
        """Compute the best extraction seam and the resulting source, WITHOUT
        writing — the method extraction's preview."""
        return MethodExtraction(self).extract_method_proposal()

    def fix_extract_method(self) -> str | None:
        """Extract Function (applied): the best self-contained seam of the
        function at `line` becomes a new function named `name`."""
        return MethodExtraction(self).fix_extract_method()

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
            free = CstMechanics._FreeNames()
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
            body=[*CstMechanics._moved_imports(module, referenced), *(top_defs[n] for n in opts.params)]
        )
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
        finder = CstMechanics._FindFnLine(line)
        wrapper.visit(finder)
        fn = finder.found
        if fn is None or not CstMechanics._module_contains(wrapper.module, fn):
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
        finder = CstMechanics._FindFnLine(line)
        wrapper.visit(finder)
        fn = finder.found
        if fn is None or not CstMechanics._module_contains(wrapper.module, fn):
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

    def _fix_structural(self) -> str | None:
        """The name-driven transforms — the agent supplies the semantic bit.
        A registry of kind -> fixer method (the conditional-polymorphism
        shape: a dispatch table, not branching)."""
        kind = self.kind
        if kind in _NAME_REQUIRED_KINDS and self.opts.name is None:
            return None
        fixer = _STRUCTURAL_FIXERS.get(kind)
        return getattr(self, fixer)() if fixer else None

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

    def fix_tuple_record(self) -> str | None:
        """The anonymous tuple record becomes a named class."""
        return RecordFixes(self).tuple_record()

    def fix_extract_record_class(self) -> str | None:
        """The constant-key dict literal becomes a named class."""
        return RecordFixes(self).extract_record_class()

    def fix_parameter_object(self) -> str | None:
        return ParameterObjects(self).run()

    def fix_magic_literal(self) -> str | None:
        return MagicLiteral(self).run()

    def fix_rename(self) -> str | None:
        return Renaming(self).run()

    def fix_feature_envy(self) -> str | None:
        return FeatureEnvy(self).run()

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
            new_source = MechanicalFixes(self).run()
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
    "dissolve-husk": ("dissolve the delegating-husk: rewire its callers to the module functions, delete the husk"),
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


def _is_husk_instantiation(stmt: cst.BaseSmallStatement, husk: str) -> bool:
    """`x = Husk()` with no arguments — one instance the static-husk rewrite
    carries onto its members."""
    return (
        isinstance(stmt, cst.Assign)
        and len(stmt.targets) == 1
        and isinstance(stmt.targets[0].target, cst.Name)
        and isinstance(stmt.value, cst.Call)
        and isinstance(stmt.value.func, cst.Name)
        and stmt.value.func.value == husk
        and not stmt.value.args
    )


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
            pos = CstMechanics._as_range(self.get_metadata(PositionProvider, node))
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

    def _is_body_store(self, ctx) -> bool:
        """A name stored by the function body — not a parameter (the signature)
        and not outside any function."""
        return ctx == cst.metadata.ExpressionContext.STORE and self._current is not None and not self._in_param

    @override
    def visit_Name(self, node) -> None:
        try:
            ctx = self.get_metadata(ExpressionContextProvider, node)
        except KeyError:
            return  # attribute names (obj.state) are not refs
        if self._is_body_store(ctx):
            assert self._current is not None
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
        if CstMechanics._is_keyword_name(original_node, parent):
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


# --------------------------------------------------------------------------- the fix surface


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
                if _is_husk_instantiation(stmt, husk):
                    assert isinstance(stmt, cst.Assign) and isinstance(stmt.targets[0].target, cst.Name)
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
                if _is_husk_method_call(inner, func, husk, method_fns):
                    return updated_node.with_changes(func=cst.Name(method_fns[func.attr.value]))
            # h.m(<args>) -> Fm(<args>) for a proven instance variable
            if _is_husk_attr_call(func, instances, method_fns):
                assert isinstance(func, cst.Attribute)
                return updated_node.with_changes(func=cst.Name(method_fns[func.attr.value]))
            return updated_node

    rewired = module.visit(_HuskRewire())

    class _Leftover(cst.CSTVisitor):
        """Any remaining reference to the husk or an instance variable —
        EXCEPT inside the husk class's own subtree (deleted afterwards)."""

        def __init__(self):
            self.names: list[str] = []
            self.in_husk: int = 0

        def _is_husk_name(self, node) -> bool:
            """A reference to the husk class or one of its instances, outside
            the husk's own subtree."""
            return self.in_husk == 0 and (node.value == husk or node.value in instances)

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
            if self._is_husk_name(node):
                self.names.append(node.value)

    # drop the `h = Husk()` assignments: identity removal needs nodes OF the
    # rewired tree (the rewire produced new objects), so re-probe it
    drop_probe = _HuskProbe()
    rewired.visit(drop_probe)
    if drop_probe.statements:
        rewired = rewired.visit(CstMechanics._RemoveNodes(drop_probe.statements))
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
        if _is_import_line(s):
            insert_at = i + 1
    remaining.insert(insert_at, reexport)
    return remaining


class _MethodAtLine(cst.CSTVisitor):
    """The class + method whose def sits on one line — a chain's head probe."""

    METADATA_DEPENDENCIES = (PositionProvider,)

    def __init__(self, target_line: int) -> None:
        self.target_line = target_line
        self.owner: tuple[str, cst.ClassDef, cst.FunctionDef] | None = None

    @override
    def visit_ClassDef(self, node: cst.ClassDef) -> None:
        for stmt in node.body.body:
            if not isinstance(stmt, cst.FunctionDef):
                continue
            pos = CstMechanics._as_range(self.get_metadata(PositionProvider, stmt))
            if pos.start.line == self.target_line:
                self.owner = (node.name.value, node, stmt)
                return


def _chain_head(wrapper: cst.MetadataWrapper, line: int) -> _ChainHead | None:
    """The forwarding chain's head at `line`: a method whose body is exactly
    one call into a module function, passing its own parameters through. None
    when the line is not that shape (stale, or not a forwarder)."""
    probe = _MethodAtLine(line)
    wrapper.visit(probe)
    if probe.owner is None:
        return None
    class_name, class_node, method = probe.owner
    params = _non_receiver_param_names(method.params.params)
    callee, args = _pure_forward_of(method)
    if _breaks_forwarding_chain(callee, args, params):
        return None
    assert callee is not None
    return _ChainHead(class_name, class_node, method, params, callee)


def _chain_middle(repo: Path, head: _ChainHead) -> _ChainMiddle | None:
    """The module function F the head calls, resolved repo-wide; None when F is
    missing or does not forward its own parameters on."""
    found = _find_module_fn(_ModuleRef(repo, head.callee.name))
    if found is None:
        return None
    params = [p.name.value for p in found.node.params.params]
    callee, args = _pure_forward_of(found.node)
    if callee is None:
        return None
    # a method callee absorbs the receiver param (`store.load(key)` passes only
    # `key`); a module-fn callee takes all params
    expected = [p for p in params if p != callee.attr_method[0]] if callee.attr_method is not None else params
    if args != expected:
        return None
    return _ChainMiddle(found.rel, found.module, params, callee)


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


def _find_module_fn(ref: _ModuleRef) -> _ModuleFn | None:
    """One module-level function by name, resolved across the repo — the
    lookup returns the file, the module, and the node together."""
    for path in CstMechanics._py_files(ref.repo):
        module = cst.parse_module(path.read_text(encoding="utf-8"))
        node = _module_fn_node(module, ref.name)
        if node is not None:
            return _ModuleFn(path.relative_to(ref.repo).as_posix(), module, node)
    return None


def _call_sites(ref: _ModuleRef) -> int:
    total = 0
    for path in CstMechanics._py_files(ref.repo):
        mod = cst.parse_module(path.read_text(encoding="utf-8"))

        class _Count(cst.CSTVisitor):
            def __init__(self):
                self.n = 0

            @override
            def visit_Call(self, node):
                if isinstance(node.func, cst.Name) and node.func.value == ref.name:
                    self.n += 1

        c = _Count()
        mod.visit(c)
        total += c.n
    return total


def _reexported(ref: _ModuleRef) -> bool:
    for path in CstMechanics._py_files(ref.repo):
        mod = cst.parse_module(path.read_text(encoding="utf-8"))
        for stmt in mod.body:
            if isinstance(stmt, cst.SimpleStatementLine):
                for inner in stmt.body:
                    if not isinstance(inner, cst.ImportFrom) or isinstance(inner.names, cst.ImportStar):
                        continue
                    for alias in inner.names:
                        if isinstance(alias.name, cst.Name) and alias.name.value == ref.name:
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
                mod_name = (
                    inner.module
                    if isinstance(inner.module, str)
                    else (inner.module.value if hasattr(inner.module, "value") else "")
                )
                if str(mod_name).split(".")[-1] == target_stem:
                    return True
    return False


def _methods_named(ref: _ModuleRef) -> list[tuple[str, cst.ClassDef, cst.FunctionDef]]:
    out: list[tuple[str, cst.ClassDef, cst.FunctionDef]] = []
    for path in CstMechanics._py_files(ref.repo):
        mod = cst.parse_module(path.read_text(encoding="utf-8"))
        for stmt in mod.body:
            if not isinstance(stmt, cst.ClassDef):
                continue
            for mem in stmt.body.body:
                if isinstance(mem, cst.FunctionDef) and mem.name.value == ref.name:
                    out.append((path.relative_to(ref.repo).as_posix(), stmt, mem))
    return out


def _class_has_identity(cls: cst.ClassDef) -> bool:
    writes_self = False

    class _Scan(cst.CSTVisitor):
        @override
        def visit_Assign(self, node):
            nonlocal writes_self
            for t in node.targets:
                if CstMechanics._is_self_attribute(t.target):
                    writes_self = True

        @override
        def visit_AnnAssign(self, node):
            nonlocal writes_self
            if CstMechanics._is_self_attribute(node.target):
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
    for path in CstMechanics._py_files(repo):
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
            pos = CstMechanics._as_range(self.get_metadata(PositionProvider, original_node))
            if pos.start.line == self.line:
                return updated_node.with_changes(body=updated_node.body.with_changes(body=self.body))
        return updated_node


# lucidlint: ignore data-clump the (module, positions, line) triple is the analyzer's input record
def _replace_method_body(
    module: cst.Module, method: cst.FunctionDef, body: cst.SimpleStatementLine, line: int | None = None
) -> cst.Module:
    if line is None:
        line = CstMechanics._as_range(cst.MetadataWrapper(module).resolve(PositionProvider)[method]).start.line
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
        pos = CstMechanics._as_range(self.get_metadata(PositionProvider, original_node))
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
    probe = CstMechanics._FreeNames()
    cls.visit(probe)
    return probe.names - probe.bound - {cls.name.value}


def _free_names_of(stmt) -> set[str]:
    """The names one module-level statement reads from the module scope
    (bindings inside it excluded) — the residual's references to moved
    classes and constants."""
    probe = CstMechanics._FreeNames()
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
        if isinstance(inner, cst.Assign) and all(isinstance(t.target, cst.Name) for t in inner.targets):
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
        if _is_single_name_assign(inner):
            assert isinstance(inner, cst.Assign) and isinstance(inner.targets[0].target, cst.Name)
            out.append((inner.targets[0].target.value, stmt))
        elif isinstance(inner, cst.AnnAssign) and inner.value is not None and isinstance(inner.target, cst.Name):
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
        isinstance(s, cst.SimpleStatementLine) and any(isinstance(i, cst.ImportFrom) and i.relative for i in s.body)
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
                    name in CstMechanics._hoist_target_names(t.target) for t in inner.targets
                ):
                    return True
                if _binds_hoisted_name(inner, name):
                    return True
                if isinstance(inner, (cst.Import, cst.ImportFrom)):
                    bound: set[str] = set()
                    CstMechanics._collect_alias_names(bound, inner)
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
    value_refs_of = {n: CstMechanics._expr_names(v) - CstMechanics.BUILTINS for n, v in value_of.items()}
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
    insert = 1 if body and CstMechanics._is_docstring_stmt(body[0]) else 0
    new_body = body[:insert] + attrs + body[insert:]
    cls2 = cls.with_changes(body=cls.body.with_changes(body=new_body))
    return cast(cst.ClassDef, cls2.visit(_RewriteMovedConstRefs(wrapper, moved, cls_name)))


class _SplitOrigin(NamedTuple):
    """Where the module being split lives: its repo-relative path, the
    directory segments that decide flat-vs-package, its stem (the file name
    without `.py`), and the dotted path the split's imports resolve through."""

    rel: str
    directory: str
    segments: list[str]
    stem: str

    @property
    def in_package(self) -> bool:
        return bool(self.segments)

    @property
    def origin_path(self) -> list[str]:
        return self.segments + [self.stem]


def _split_origin(rel: str) -> _SplitOrigin:
    """The module's location facts, derived once from its repo-relative path."""
    directory = rel.rsplit("/", 1)[0] if "/" in rel else ""
    return _SplitOrigin(
        rel=rel,
        directory=directory,
        segments=directory.split("/") if directory else [],
        stem=rel.rsplit("/", 1)[-1].removesuffix(".py"),
    )


def _split_subjects(top: Sequence, stem: str) -> list[cst.ClassDef]:
    """The misplaced public classes: not private, not the stem-matching class
    (that one belongs to the module's own name)."""
    return [
        stmt
        for stmt in top
        if isinstance(stmt, cst.ClassDef)
        and not stmt.name.value.startswith("_")
        and not _class_matches_stem(stmt.name.value, stem)
    ]


def _refuse_split_reordering(top: Sequence, subjects: list[cst.ClassDef]) -> str | None:
    """Module-level executable statements BETWEEN the first and last subject
    class would be reordered by the split — decline, never reorder."""
    subject_ids = {id(s) for s in subjects}
    bounds = [i for i, stmt in enumerate(top) if id(stmt) in subject_ids]
    for stmt in top[bounds[0] : bounds[-1] + 1]:
        if id(stmt) in subject_ids or isinstance(stmt, cst.EmptyLine):
            continue
        if isinstance(stmt, (cst.ClassDef, cst.FunctionDef)):
            continue
        if _stmt_is_plain_assign(stmt):
            continue
        return "module-level executable statements between classes would be reordered by the split — split by hand"
    return None


def _union_find(parent: dict[int, int], node: int) -> int:
    """One union-find root lookup — the clustering's only shared state."""
    while parent[node] != node:
        parent[node] = parent[parent[node]]
        node = parent[node]
    return node


class _ModuleSplit:
    """One module split (#31), as the operation it is: the module's misplaced
    classes, the constants that move with them, the residual that stays, and
    the layout the split produces. Each step is named after what it decides —
    plan the constant moves, cluster, name the layout, refuse the unsafe
    shapes, build the sources — so the phases read in order."""

    def __init__(self, repo: Path, origin: _SplitOrigin, module: cst.Module, subjects: list[cst.ClassDef]) -> None:
        self.repo = repo
        self.origin = origin
        self.module = module
        self.subjects = subjects
        self.subject_names = {subject.name.value for subject in subjects}
        self.const_stmts = _module_constants(module.body)
        self.const_names = {name for name, _stmt in self.const_stmts}
        self.moved_consts = self._moved_constants()
        self.moved_all = set().union(*self.moved_consts.values()) if self.moved_consts else set()
        self.residual = [
            stmt
            for stmt in module.body
            if id(stmt) not in {id(s) for s in subjects} and id(stmt) not in {id(s) for _name, s in self.const_stmts}
        ]
        self.residual_def_names = {
            stmt.name.value for stmt in self.residual if isinstance(stmt, (cst.FunctionDef, cst.ClassDef))
        }
        self.residual_def_names |= self.const_names - self.moved_all
        self.residual_subject_refs = self._residual_subject_refs()
        self.free_of = {id(c): _class_free_names(c) for c in subjects}
        self.attrs_of = {id(c): _class_attr_names(c) for c in subjects}

    def _moved_constants(self) -> dict[int, set[str]]:
        """Which module constants each subject class reads, and which of those
        no residual statement and no other module needs — only those move."""
        const_def_ids = {id(stmt) for _name, stmt in self.const_stmts}
        subject_ids = {id(subject) for subject in self.subjects}
        class_refs: dict[int, set[str]] = {}
        residual_refs: set[str] = set()
        for stmt in self.module.body:
            if id(stmt) in const_def_ids:
                continue
            names = _free_names_of(stmt) & self.const_names
            if isinstance(stmt, cst.ClassDef) and id(stmt) in subject_ids:
                class_refs[id(stmt)] = names
            else:
                residual_refs |= names
        external = {
            name
            for name in self.const_names
            if _const_imported_elsewhere(_ModuleRef(self.repo, name), self.origin.rel, self.module)
        }
        return {
            id(subject): _consts_moving_into(self.const_stmts, class_refs, residual_refs, external, subject)
            for subject in self.subjects
        }

    def _residual_subject_refs(self) -> set[str]:
        """The moved classes the residual statements need — the origin imports
        them back. A constant's VALUE may reference one (`CACHE = {User: []}`):
        the residual keeps the constant, so the origin imports the class too."""
        refs: set[str] = set()
        for stmt in self.residual:
            if isinstance(stmt, cst.SimpleStatementLine) and any(
                isinstance(item, (cst.Import, cst.ImportFrom)) for item in stmt.body
            ):
                continue  # imports bind their own names — not class references
            refs |= _free_names_of(stmt) & self.subject_names
        for _name, stmt in self.const_stmts:
            refs |= CstMechanics._expr_names(_const_value(stmt)) & self.subject_names
        return refs

    def components(self) -> list[list[cst.ClassDef]]:
        """The subjects clustered by cohesion edges: cross-class member
        references OR shared class-level attributes. Components keep source
        order."""
        parent = {id(subject): id(subject) for subject in self.subjects}
        for i, first in enumerate(self.subjects):
            for second in self.subjects[i + 1 :]:
                if _functions_collide(first, second, self.free_of, self.attrs_of):
                    parent[_union_find(parent, id(first))] = _union_find(parent, id(second))
        class_index = {id(stmt): i for i, stmt in enumerate(self.module.body)}
        components: dict[int, list[cst.ClassDef]] = {}
        for subject in self.subjects:
            components.setdefault(_union_find(parent, id(subject)), []).append(subject)
        return sorted(components.values(), key=lambda group: min(class_index[id(c)] for c in group))

    def layout(self, package_name: str | None) -> _SplitLayout:
        """Name each component's target: a lone class goes flat, a cohesive
        whole takes the stem (or --name), a sub-cluster needs --name. Relative
        imports in the origin refuse the package layout and fall back to flat."""
        total = len(self.subjects)
        members: list[tuple[list[cst.ClassDef], str | None]] = []
        for component in self.components():
            if len(component) == 1:
                members.append((component, None))
            elif len(component) == total:
                members.append((component, package_name or self.origin.stem))
            elif package_name:
                members.append((component, package_name))
            else:
                return _SplitLayout(
                    members=[],
                    any_package=False,
                    snake={},
                    decline=(
                        "the cohesive sub-cluster needs a package name — name the package with a domain noun (--name)"
                    ),
                )
        any_package = any(name is not None for _component, name in members)
        if any_package and _module_has_relative_imports(self.module):
            members = [(component, None) for component, _name in members]
            any_package = False
        snake = {subject.name.value: _snake_name(subject.name.value) for subject in self.subjects}
        return _SplitLayout(members=members, any_package=any_package, snake=snake)

    def is_whole_package(self, layout: _SplitLayout) -> bool:
        """One package holding every subject class — the origin is replaced."""
        return (
            len(layout.members) == 1
            and layout.members[0][1] is not None
            and len(layout.members[0][0]) == len(self.subjects)
        )

    def refuse_package(self, layout: _SplitLayout, wrapper: cst.MetadataWrapper, whole_pkg: bool) -> str | None:
        """The package-layout refusals: no module-level constant may survive
        (BQ6), and no residual code may evaluate a moved class at import time
        (the __init__ evaluates before its re-exports)."""
        if layout.any_package and (self.const_names - self.moved_all):
            names = ", ".join(sorted(self.const_names - self.moved_all))
            return (
                f"the constant(s) {names} cannot move into a class; a package "
                "__init__ may hold no constants — split by hand"
            )
        if whole_pkg:
            for stmt in self.residual:
                refs = _exec_time_names(wrapper, stmt) & self.subject_names
                if refs:
                    return (
                        "module-level code evaluates "
                        + ", ".join(sorted(refs))
                        + " at import time — the package __init__ runs before the "
                        "re-exports — split by hand"
                    )
        return None

    def refuse_collisions(self, layout: _SplitLayout, package_name: str | None) -> str | None:
        """The target-collision refusals: an existing sibling file or an
        existing package dir blocks the split (the stem package needs --name
        when taken)."""
        directory = self.origin.directory
        seen_files: set[str] = set()
        for component, name in layout.members:
            if name is None:
                for subject in component:
                    target = self._flat_target(layout, subject)
                    if target in seen_files:
                        return f"the split target '{target}' would collide — split by hand"
                    seen_files.add(target)
                    if (self.repo / target).exists():
                        return f"'{target}' already exists — split by hand"
            else:
                package_dir = f"{directory}/{name}" if directory else name
                # the origin file itself is being replaced — only a DIFFERENT
                # `name.py` or an existing `name/` directory blocks the package
                if (self.repo / package_dir).exists() or (
                    (self.repo / f"{package_dir}.py").exists() and f"{package_dir}.py" != self.origin.rel
                ):
                    if name == self.origin.stem and package_name is None:
                        return f"a package named '{name}' already exists — pass --name <name>"
                    return f"the package name '{name}' is taken — pick another --name"
        return None

    def refuse_cycles(self, layout: _SplitLayout) -> str | None:
        """Import cycles between the produced class files (the __init__
        re-export edge is exempt by construction)."""
        if not layout.any_package:
            return None
        for component, name in layout.members:
            if name is None:
                continue
            nodes = [subject.name.value for subject in component]
            edges = {node: set() for node in nodes}
            for subject in component:
                for other in component:
                    if other is not subject and other.name.value in self.free_of[id(subject)]:
                        edges[subject.name.value].add(other.name.value)
            cycle = _graph_cycle(nodes, edges)
            if cycle:
                return "the split would create an import cycle (" + " -> ".join(cycle) + ") — split by hand"
        return None

    def refuse_residual_cycles(self) -> str | None:
        """Import cycles between a moved class file and the residual origin
        module, and the `from ... import *` origin the split cannot retarget."""
        bad = [
            subject.name.value
            for subject in self.subjects
            if (self.free_of[id(subject)] & self.residual_def_names)
            and subject.name.value in self.residual_subject_refs
        ]
        if bad:
            return (
                "the split would create an import cycle between '" + bad[0] + "' and the origin module — split by hand"
            )
        if _module_star_imported(self.repo, self.origin.rel, self.module):
            return "a file does 'from ... import *' on the origin — the split cannot retarget it — split by hand"
        return None

    @staticmethod
    def targets(layout: _SplitLayout) -> dict[str, list[str]]:
        """Each subject class's import path after the split: the package name,
        or the snake-cased flat sibling."""
        return {
            subject.name.value: ([name] if name is not None else [layout.snake[subject.name.value]])
            for component, name in layout.members
            for subject in component
        }

    def _flat_target(self, layout: _SplitLayout, subject: cst.ClassDef) -> str:
        filename = f"{layout.snake[subject.name.value]}.py"
        return f"{self.origin.directory}/{filename}" if self.origin.directory else filename

    def member_sources(self, layout: _SplitLayout, wrapper: cst.MetadataWrapper) -> list[tuple[str, str]]:
        """The produced files for the moved classes: flat siblings, or a
        package's __init__ (residual + re-exports) and one file per class."""
        writes: list[tuple[str, str]] = []
        for component, name in layout.members:
            if name is None:
                for subject in component:
                    writes.append(
                        (
                            self._flat_target(layout, subject),
                            _split_class_source(
                                self.module,
                                wrapper,
                                subject,
                                self._split_ctx(subject, None),
                            ),
                        )
                    )
            else:
                package_dir = f"{self.origin.directory}/{name}" if self.origin.directory else name
                init_body = list(self.residual) + [
                    cst.SimpleStatementLine(body=[_from_import_node([c.name.value], [c.name.value], 1)])
                    for c in component
                ]
                writes.append((f"{package_dir}/__init__.py", cst.Module(body=init_body).code))
                for subject in component:
                    writes.append(
                        (
                            f"{package_dir}/{subject.name.value}.py",
                            _split_class_source(
                                self.module,
                                wrapper,
                                subject,
                                self._split_ctx(subject, name),
                            ),
                        )
                    )
        return writes

    def _split_ctx(self, subject: cst.ClassDef, package: str | None) -> _SplitCtx:
        """The per-class rewrite context: which names move with this class,
        and the import depth its new file sees — a package member is one hop
        deeper than the package's own __init__."""
        in_package = self.origin.in_package
        return _SplitCtx(
            stem=self.origin.stem,
            subjects=self.subject_names,
            residual_def_names=self.residual_def_names,
            moved=self.moved_consts.get(id(subject), set()),
            package=package,
            origin_dots=(2 if in_package else 0) if package is not None else (1 if in_package else 0),
            member_dots=1 if package is not None else 0,
        )

    def origin_source(self, targets: dict[str, list[str]]) -> str:
        """The origin residual: the kept statements in their original order
        plus the from-imports the residual needs for the moved classes."""
        subject_ids = {id(subject) for subject in self.subjects}
        moved_def_ids = {id(stmt) for name, stmt in self.const_stmts if name in self.moved_all}
        kept = [stmt for stmt in self.module.body if id(stmt) not in subject_ids and id(stmt) not in moved_def_ids]
        additions = [
            cst.SimpleStatementLine(body=[_from_import_node(targets[name], [name], 1 if self.origin.in_package else 0)])
            for name in sorted(self.residual_subject_refs)
        ]
        return cst.Module(body=_insert_header_imports(kept, additions)).code

    def retarget(
        self, origin_source: str, targets: dict[str, list[str]], whole_pkg: bool
    ) -> tuple[str, dict[str, str]]:
        """Repo-wide rewrites for the layouts that move classes out of the
        reachable origin (FLAT + sub-package): from-imports and `old.Cls`
        attribute chains retarget to the class's new module."""
        if whole_pkg:
            return origin_source, {}
        rewritten: dict[str, str] = {}
        for path in CstMechanics._py_files(self.repo):
            rel = path.relative_to(self.repo).as_posix()
            text = origin_source if rel == self.origin.rel else path.read_text(encoding="utf-8")
            new = _split_rewrite_source(text, rel, self.origin.origin_path, targets)
            if new is not None:
                rewritten[rel] = new
        if self.origin.rel in rewritten:
            origin_source = rewritten[self.origin.rel]
        return origin_source, rewritten

    def finalize(
        self,
        origin_source: str,
        writes: list[tuple[str, str]],
        rewritten: dict[str, str],
        whole_pkg: bool,
    ) -> _SplitOutcome:
        """Verify the produced files parse and no new target collides, then
        decide whether an empty origin is deleted (only when no import names
        it anywhere in the produced repo, BQ6(f))."""
        rel = self.origin.rel
        for target, src in [(rel, origin_source)] + writes:
            try:
                cst.parse_module(src)
            except Exception:  # lucidlint: ignore broad-except the split is cst output; a parse failure means refuse
                return _SplitOutcome(
                    None,
                    [],
                    [],
                    f"the computed split for '{target}' does not parse — split by hand",
                )
            if _split_target_exists(self.repo, target, rel, rewritten):
                return _SplitOutcome(
                    None,
                    [],
                    [],
                    f"the split target '{target}' already exists — split by hand",
                )
        if whole_pkg:
            return _SplitOutcome("", writes, [rel])
        final_sources = {path: source for path, source in rewritten.items()}
        final_sources.update({target: src for target, src in writes})
        if _module_empty(origin_source) and not _origin_imported_anywhere(
            self.repo, final_sources, rel, self.origin.origin_path
        ):
            return _SplitOutcome("", writes, [rel])
        return _SplitOutcome(origin_source, writes, [])

    def execute(self, wrapper: cst.MetadataWrapper, package_name: str | None) -> _SplitOutcome:
        """Run the split: name the layout, refuse the unsafe shapes, build and
        verify the sources. The refusals run in the order their reasons are
        cheapest for the reader to check."""
        layout = self.layout(package_name)
        if layout.decline is not None:
            return _SplitOutcome(None, [], [], layout.decline)
        whole_pkg = self.is_whole_package(layout)
        refusals = [
            self.refuse_package(layout, wrapper, whole_pkg),
            self.refuse_collisions(layout, package_name),
            self.refuse_cycles(layout),
        ]
        if not whole_pkg:
            refusals.append(self.refuse_residual_cycles())
        for refusal in refusals:
            if refusal is not None:
                return _SplitOutcome(None, [], [], refusal)
        targets = self.targets(layout)
        writes = self.member_sources(layout, wrapper)
        origin_source = self.origin_source(targets)
        origin_source, rewritten = self.retarget(origin_source, targets, whole_pkg)
        writes.extend((rel, src) for rel, src in rewritten.items() if rel != self.origin.rel)
        return self.finalize(origin_source, writes, rewritten, whole_pkg)


def _from_import_node(module_comps: list[str], names: list[str] | list[cst.ImportAlias], dots: int) -> cst.ImportFrom:
    """`from <module> import <names>` with `dots` relative dots; module
    components are a dotted Name/Attribute chain when present."""
    module: cst.BaseExpression | None = None
    if module_comps:
        module = cst.Name(module_comps[0])
        for part in module_comps[1:]:
            module = cst.Attribute(value=module, attr=cst.Name(part))
    aliases = [a if isinstance(a, cst.ImportAlias) else cst.ImportAlias(name=cst.Name(a)) for a in names]
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
    free = _class_free_names(cls) - ctx.moved - CstMechanics.BUILTINS
    imports = CstMechanics._moved_imports(module, free)  # verbatim copies for import-bound names
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


def _alias_points_to_origin(parts: list[str], origin_path: list[str], alias_map: dict[str, list[str]]) -> bool:
    """A one-component import path that names the origin through an alias."""
    return len(parts) == 1 and parts[0] in alias_map and alias_map[parts[0]] == origin_path


def _import_refers_to_origin(ifrom: cst.ImportFrom, importer_dir: list[str], origin_path: list[str]) -> bool:
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
        return _alias_points_to_origin(parts, self.origin_path, self.alias_map)

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
    i = 1 if body and CstMechanics._is_docstring_stmt(body[0]) else 0
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
        cst.SimpleStatementLine(body=[cst.Import(names=[cst.ImportAlias(name=_dotted_expr(p.split(".")))])])
        for p in sorted(dotted_paths)
    ]
    return module.with_changes(body=_insert_header_imports(list(module.body), lines))


def _const_imported_elsewhere(ref: _ModuleRef, rel: str, module: cst.Module) -> bool:
    """Is the constant imported or attribute-read from another file? A moved
    constant must be sole-owned by its class (BQ6) — an external reader would
    break."""
    origin_path = _module_parts(rel)
    for path in CstMechanics._py_files(ref.repo):
        if path.relative_to(ref.repo).as_posix() == rel:
            continue
        try:
            mod = cst.parse_module(path.read_text(encoding="utf-8"))
        except Exception:
            continue
        importer_dir = path.relative_to(ref.repo).as_posix().split("/")[:-1]
        alias_map = _module_alias_map(mod, origin_path)

        probe = _ConstImportScan(importer_dir, alias_map, origin_path, ref.name)
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

    def _refers_to_origin(self, parts: list[str]) -> bool:
        """The import path is the origin module itself, or an alias of it."""
        if parts == self.origin_path:
            return True
        return _alias_points_to_origin(parts, self.origin_path, self.alias_map)

    @override
    def visit_ImportFrom(self, node) -> None:
        if not _import_refers_to_origin(node, self.importer_dir, self.origin_path):
            return
        if isinstance(node.names, cst.ImportStar) or any(
            isinstance(a.name, cst.Name) and a.name.value == self.name for a in node.names
        ):
            self.hit = True

    @override
    def visit_Attribute(self, node) -> None:
        if node.attr.value != self.name:
            return
        parts = _resolve_dotted(node.value)
        if parts is None:
            return
        if self._refers_to_origin(parts):
            self.hit = True


def _module_star_imported(repo: Path, rel: str, module: cst.Module) -> bool:
    """Does any other file `from <origin> import *`? The flat rewrite cannot
    retarget a star import — the moved classes would silently vanish from it."""
    origin_path = _module_parts(rel)
    for path in CstMechanics._py_files(repo):
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
                if _is_star_import_from_origin(inner, importer_dir, origin_path):
                    return True
    return False


def _origin_imported_anywhere(repo: Path, sources: dict[str, str], rel: str, origin_path: list[str]) -> bool:
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
                if isinstance(inner, cst.ImportFrom) and not isinstance(inner.names, cst.ImportStar):
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
    return all(not isinstance(s, (cst.FunctionDef, cst.ClassDef, cst.SimpleStatementLine)) for s in mod.body)


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
                bound |= CstMechanics._hoist_target_names(t.target)
        elif isinstance(inner, cst.AnnAssign) and inner.target is not None:
            bound |= CstMechanics._hoist_target_names(inner.target)
    return bound


# --------------------------------------------------------------------------- dispatch-registry


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
    if not _is_single_return_tail(tail):
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
        if _is_empty_list_assign(a):
            assert isinstance(a, cst.Assign) and isinstance(a.targets[0].target, cst.Name)
            return a.targets[0].target.value, i
        if _is_empty_list_annassign(a):
            assert isinstance(a, cst.AnnAssign) and isinstance(a.target, cst.Name)
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
    if not _is_accumulator_append(call, acc) or len(call.args) != 1:
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


class _FindWideTuples(cst.CSTVisitor):
    """Every wide-tuple annotation anchored on the finding's line — the fixer
    declines when the line carries more than one (it cannot pick)."""

    METADATA_DEPENDENCIES = (PositionProvider,)

    def __init__(self, line: int) -> None:
        self.line: int = line
        self.sites: list[_WideTupleSite] = []

    @override
    def visit_AnnAssign(self, node) -> None:
        if CstMechanics._as_range(self.get_metadata(PositionProvider, node)).start.line != self.line:
            return
        if not isinstance(node.target, cst.Name):
            return
        arity = _fixed_tuple_arity(node.annotation.annotation)
        if arity is None or arity < 3:
            return
        elements = None
        if _is_arity_tuple(node.value, arity):
            assert isinstance(node.value, cst.Tuple)
            elements = tuple(element.value for element in node.value.elements)
        self.sites.append(_WideTupleSite("assign", node.target.value, arity, node.annotation.annotation, elements))

    @override
    def visit_FunctionDef(self, node) -> None:
        if CstMechanics._as_range(self.get_metadata(PositionProvider, node)).start.line != self.line:
            return
        for param in [*node.params.posonly_params, *node.params.params, *node.params.kwonly_params]:
            if param.annotation is None:
                continue
            arity = _fixed_tuple_arity(param.annotation.annotation)
            if arity is not None and arity >= 3:
                self.sites.append(_WideTupleSite("param", param.name.value, arity, param.annotation.annotation, None))
        if node.returns is not None:
            arity = _fixed_tuple_arity(node.returns.annotation)
            if arity is not None and arity >= 3:
                self.sites.append(_WideTupleSite("return", node.name.value, arity, node.returns.annotation, None))


def _fixed_tuple_arity(node: cst.BaseExpression | None) -> int | None:
    """The arity of a fixed-arity `tuple[...]`/`Tuple[...]` annotation; None
    for anything else (including the variadic `tuple[int, ...]`)."""
    if not _is_tuple_subscript(node):
        return None
    assert isinstance(node, cst.Subscript)
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
        if _field_name_taken_by_python(name, names):
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
    body: list = [CstMechanics._record_class_def(class_name, fields), cst.EmptyLine(), *transformed.body]
    return cst.Module(body=body).code


# --------------------------------------------------------------------------- data-clump


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


def _type_alias_value(node: cst.BaseExpression) -> bool:
    """A type expression, not a runtime value: the scanner's alias table
    accepts names, attributes, subscripts, and `|` unions."""
    if isinstance(node, (cst.Name, cst.Attribute, cst.Subscript)):
        return True
    return isinstance(node, cst.BinaryOperation) and isinstance(node.operator, cst.BitOr)


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
                if isinstance(target, cst.Name) and _type_alias_value(small.value):
                    aliases[target.value] = small.value
            elif isinstance(small, cst.AnnAssign) and isinstance(small.target, cst.Name):
                if small.value is not None and _type_alias_value(small.value):
                    aliases[small.target.value] = small.value
    return aliases


def _annotation_key(
    annotation: cst.BaseExpression | None,
    module: cst.Module,
    aliases: dict[str, cst.BaseExpression],
) -> str:
    """The parameter's grouping key: annotation source text without
    whitespace, resolved through module aliases. `""` is unannotated."""
    node = annotation
    seen: set[str] = set()
    while _is_unresolved_alias(node, aliases, seen):
        assert isinstance(node, cst.Name)
        seen.add(node.value)
        node = aliases[node.value]
    if node is None:
        return ""
    return "".join(module.code_for_node(node).split())


def _typed_args(
    fn: cst.FunctionDef,
    module: cst.Module,
    aliases: dict[str, cst.BaseExpression],
) -> list[tuple[str, str]]:
    """The regular positional parameters with their grouping keys."""
    return [
        (
            param.name.value,
            _annotation_key(
                param.annotation.annotation if param.annotation else None,
                module,
                aliases,
            ),
        )
        for param in fn.params.params
    ]


# lucidlint: ignore complexity,assembly-class libcst shape dispatch; module and aliases are parse inputs
def _clump_at(module: cst.Module, line: int, positions) -> _Clump | None:
    """The clump anchored at `line`: the module-level defs sharing a regular-
    parameter pair (>= 3 of them, the scanner's rule), the pair in the
    anchor's signature order. None when no pair qualifies there (a stale
    anchor). Pairs that share names but not annotations stay separate."""
    fns = [stmt for stmt in module.body if isinstance(stmt, cst.FunctionDef)]
    aliases = _module_aliases(module)
    pairs: dict[tuple[tuple[str, str], tuple[str, str]], list[cst.FunctionDef]] = {}
    for fn in fns:
        params = _typed_args(fn, module, aliases)
        for i in range(len(params)):
            for j in range(i + 1, len(params)):
                left, right = params[i], params[j]
                key = (left, right) if left[0] <= right[0] else (right, left)
                pairs.setdefault(key, []).append(fn)
    anchor = next((f for f in fns if CstMechanics._as_range(positions[f]).start.line == line), None)
    if anchor is None:
        return None
    qualifying = sorted(pair for pair, group in pairs.items() if len(group) >= 3 and anchor in group)
    if not qualifying:
        return None
    pair = qualifying[0]
    order = _plain_args(anchor)
    kept = {id(f) for f in pairs[pair]}
    ordered = tuple(sorted((pair[0][0], pair[1][0]), key=order.index))
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

    def _own_params(self, node: cst.FunctionDef) -> set[str]:
        return {p.name.value for p in [*node.params.posonly_params, *node.params.params, *node.params.kwonly_params]}

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
        if CstMechanics._is_keyword_name(original_node, parent):
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
                f"the call to '{fn}' does not pass both '{self.pair[0]}' and '{self.pair[1]}' — thread the pair by hand"
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
        if _has_positional_only_or_variadic(fn.params):
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
            f"'{culprit}' imports a clump function — its call site is outside this module; thread the pair by hand"
        )
        return None
    stores = _CollectStores()
    wrapper.visit(stores)
    transformer = _DataClumpToRecord(clump.pair, class_name, parameter, stores.per_fn, index)
    transformed = wrapper.visit(transformer)
    if transformer.decline:
        req.decline = transformer.decline
        return None
    body: list = [CstMechanics._record_class_def(class_name, [pair_a, pair_b]), cst.EmptyLine(), *transformed.body]
    return cst.Module(body=body).code


# --------------------------------------------------------------------------- partition


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
_InitPlan = tuple[
    list[tuple[str, cst.BaseExpression]],
    cst.BaseStatement | None,
    list[tuple[str, cst.Annotation | None, cst.BaseExpression | None]],
]


# lucidlint: ignore complexity the shape analyzers dispatch over libcst node kinds — splitting scatters one table
def _init_plan(init: cst.FunctionDef | None) -> _InitPlan | None:
    """__init__'s `self.x = <expr>` statements (plus its docstring) in order;
    None when the body does anything else — a split would have to reorder or
    retarget that statement."""
    if init is None:
        return [], None, []
    params = init.params
    if _has_irregular_params(params):
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
        if not _is_plain_self_assign(target, small):
            return None
        assert isinstance(small, cst.Assign) and isinstance(target, cst.Attribute)
        assigns.append((target.attr.value, small.value))
    return assigns, docstring, init_params


# lucidlint: ignore complexity the shape analyzers dispatch over libcst node kinds — splitting scatters one table
def _partition_plan(cls: cst.ClassDef) -> _PartitionAttempt:
    """The field-disjoint split of `cls`, or the reason it cannot be split —
    connector methods merge the groups and produce the refusal."""
    methods = [stmt for stmt in cls.body.body if isinstance(stmt, cst.FunctionDef)]
    extra = [
        stmt
        for stmt in cls.body.body
        if not isinstance(stmt, cst.FunctionDef) and not CstMechanics._is_docstring_stmt(stmt)
    ]
    if extra:
        return _PartitionAttempt(
            None,
            "the class carries statements besides a docstring — they cannot be attributed to one group; split by hand",
        )
    if len(methods) < 6:
        return _PartitionAttempt(
            None,
            f"the class has only {len(methods)} methods — not the latent-partition shape (6+); the anchor is stale",
        )
    init = next((method for method in methods if method.name.value == "__init__"), None)
    parsed = _init_plan(init)
    if parsed is None:
        return _PartitionAttempt(
            None,
            "the class's __init__ does more than assign self fields — a split would reorder or "
            "retarget a statement; split by hand",
        )
    init_fields, init_docstring, init_params = parsed
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
            f"'{orphan[0]}' is initialized but touched by no method — it belongs to no group; split by hand",
        )
    plan = _PartitionPlan(
        groups=tuple(
            _PartitionGroup(tuple(fielded[i][0] for i in sorted(order[name] for name in group)), fields)
            for group, fields in zip(groups, group_fields, strict=True)
        ),
        init_fields=tuple(init_fields),
        init_docstring=init_docstring,
        init_params=tuple(init_params),
    )
    return _PartitionAttempt(plan, "")


def _init_stmt(field: str, value: cst.BaseExpression) -> cst.BaseStatement:
    """One `self.<field> = <value>` line for a group's synthesized __init__."""
    return cst.SimpleStatementLine(
        body=[
            cst.Assign(
                targets=CstMechanics._self_attr_target(field),
                value=value,
            )
        ]
    )


def _group_class(
    name: str,
    template: cst.ClassDef,
    methods: list[cst.FunctionDef],
    init: _GroupInit,
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
    finder = CstMechanics._EnclosingClass(line)
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
        (
            stmt
            for stmt in cls.body.body
            if isinstance(stmt, cst.SimpleStatementLine) and CstMechanics._is_docstring_stmt(stmt)
        ),
        None,
    )
    replacements: list[cst.BaseStatement] = []
    for i, group in enumerate(plan.groups):
        init_stmts: list[cst.BaseStatement] = []
        if i == 0 and plan.init_docstring is not None:
            init_stmts.append(plan.init_docstring)
        retained = [(field, value) for field, value in plan.init_fields if field in group.fields]
        init_stmts.extend(_init_stmt(field, value) for field, value in retained)
        needed = set(group.fields)
        for _field, value in retained:
            needed |= set(CstMechanics._expr_names(value))
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
        group_cls = _group_class(class_names[i], cls, list(group.methods), _GroupInit(init_stmts, init_params), head)
        if i:
            group_cls = group_cls.with_changes(leading_lines=[cst.EmptyLine()])
        replacements.append(group_cls)
    body: list[cst.BaseStatement] = list(module.body)
    index = next(i for i, stmt in enumerate(body) if stmt is cls)
    body[index : index + 1] = replacements
    return module.with_changes(body=body).code


# ------------------------------------------------- unnamed-condition: named decisions
# Each names one decision a fixer makes inline, so the guard reads as the
# question it asks rather than a list of operators.


def _is_husk_method_call(
    inner: cst.BaseExpression, func: cst.BaseExpression, husk: str, method_fns: dict[str, str]
) -> bool:
    """`h.m(...)` on the husk instance, where `m` is one of its methods."""
    return (
        isinstance(inner, cst.Call)
        and isinstance(inner.func, cst.Name)
        and inner.func.value == husk
        and not inner.args
        and isinstance(func, cst.Attribute)
        and func.attr.value in method_fns
    )


def _is_husk_attr_call(func: cst.BaseExpression, instances: set[str], method_fns: dict[str, str]) -> bool:
    """`<instance>.m(...)` where the instance is a dissolved husk."""
    return (
        isinstance(func, cst.Attribute)
        and isinstance(func.value, cst.Name)
        and func.value.value in instances
        and func.attr.value in method_fns
    )


def _is_import_line(stmt: cst.BaseStatement) -> bool:
    """A line holding exactly one import."""
    return (
        isinstance(stmt, cst.SimpleStatementLine)
        and len(stmt.body) == 1
        and isinstance(stmt.body[0], (cst.Import, cst.ImportFrom))
    )


def _breaks_forwarding_chain(callee, args: list, params: list) -> bool:
    """The call does not forward every parameter to a plain name — the chain's
    middle edge is broken."""
    return callee is None or args != params or not callee.is_name


def _is_single_name_assign(stmt: cst.BaseSmallStatement) -> bool:
    """`name = value` with exactly one plain-name target."""
    return isinstance(stmt, cst.Assign) and len(stmt.targets) == 1 and isinstance(stmt.targets[0].target, cst.Name)


def _binds_hoisted_name(stmt: cst.BaseSmallStatement, name: str) -> bool:
    """The annotated assignment binds `name` (directly or through a target
    tuple)."""
    return (
        isinstance(stmt, cst.AnnAssign)
        and stmt.target is not None
        and name in CstMechanics._hoist_target_names(stmt.target)
    )


def _split_target_exists(repo: Path, target: str, rel: str, rewritten) -> bool:
    """The split's target path already exists on disk and is not the origin or
    a file this fix already rewrote."""
    return target != rel and target not in rewritten and (repo / target).exists()


def _is_star_import_from_origin(imp: cst.BaseSmallStatement, importer_dir: list[str], origin_path: list[str]) -> bool:
    """A `from ... import *` that pulls in the module being split."""
    return (
        isinstance(imp, cst.ImportFrom)
        and isinstance(imp.names, cst.ImportStar)
        and _import_refers_to_origin(imp, importer_dir, origin_path)
    )


def _is_single_return_tail(tail: list) -> bool:
    """The function ends with exactly one `return <value>` statement."""
    return (
        len(tail) == 1
        and isinstance(tail[0], cst.SimpleStatementLine)
        and len(tail[0].body) == 1
        and isinstance(tail[0].body[0], cst.Return)
    )


def _is_empty_list_assign(stmt: cst.BaseSmallStatement) -> bool:
    """`name = []` — the empty-list accumulator's declaration."""
    return (
        isinstance(stmt, cst.Assign)
        and isinstance(stmt.value, cst.List)
        and len(stmt.value.elements) == 0
        and isinstance(stmt.targets[0].target, cst.Name)
    )


def _is_empty_list_annassign(stmt: cst.BaseSmallStatement) -> bool:
    """`name: list = []` — the annotated form of the accumulator."""
    return (
        isinstance(stmt, cst.AnnAssign)
        and isinstance(stmt.value, cst.List)
        and len(stmt.value.elements) == 0
        and isinstance(stmt.target, cst.Name)
    )


def _is_accumulator_append(call: cst.Call, acc: str) -> bool:
    """`<acc>.append(...)` — the loop's per-item write."""
    return (
        isinstance(call.func, cst.Attribute)
        and call.func.attr.value == "append"
        and isinstance(call.func.value, cst.Name)
        and call.func.value.value == acc
    )


def _is_arity_tuple(value: cst.BaseExpression | None, arity: int) -> bool:
    """A tuple literal with exactly `arity` plain elements."""
    return (
        isinstance(value, cst.Tuple)
        and len(value.elements) == arity
        and all(isinstance(element, cst.Element) for element in value.elements)
    )


def _is_tuple_subscript(node: cst.BaseExpression | None) -> bool:
    """`tuple[...]` / `Tuple[...]` — the annotation that hides a record."""
    return (
        isinstance(node, cst.Subscript) and isinstance(node.value, cst.Name) and node.value.value in ("tuple", "Tuple")
    )


def _field_name_taken_by_python(name: str, names: list[str]) -> bool:
    """The field name is not a usable Python identifier, or is already used."""
    return not name.isidentifier() or keyword.iskeyword(name) or name in names


def _is_unresolved_alias(node: cst.BaseExpression | None, aliases: dict, seen: set[str]) -> bool:
    """The name is an import alias that has not been walked yet."""
    return isinstance(node, cst.Name) and node.value in aliases and node.value not in seen


def _has_positional_only_or_variadic(params: cst.Parameters) -> bool:
    """The signature has positional-only or variadic parameters — the fix
    cannot rewrite it faithfully."""
    return bool(params.posonly_params) or isinstance(params.star_arg, cst.Param) or params.star_kwarg is not None


def _has_irregular_params(params: cst.Parameters) -> bool:
    """The signature has positional-only, keyword-only, or variadic
    parameters."""
    return (
        bool(params.posonly_params)
        or bool(params.kwonly_params)
        or isinstance(params.star_arg, cst.Param)
        or params.star_kwarg is not None
    )


def _is_plain_self_assign(target, stmt) -> bool:
    """The statement is `self.<attr> = <value not read from self>` — one field
    initialised from outside."""
    if target is None or not CstMechanics._is_self_attribute(target):
        return False
    return isinstance(stmt, cst.Assign) and not _reads_self(stmt.value)


def _functions_collide(first, second, free_of: dict, attrs_of: dict) -> bool:
    """Two functions cannot share one moved cluster: one calls the other, or
    both touch the same state."""
    return (
        second.name.value in free_of[id(first)]
        or first.name.value in free_of[id(second)]
        or bool(attrs_of[id(first)] & attrs_of[id(second)])
    )

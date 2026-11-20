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
from fix_engine.bindings import Bindings, StoredNames
from fix_engine.fix_options import FixOptions
from fix_engine.fixers.chain_fixes import ChainFixes
from fix_engine.fixers.class_extraction import ClassExtraction
from fix_engine.fixers.data_clump import DataClump
from fix_engine.fixers.dispatch_registry import DispatchRegistry
from fix_engine.fixers.feature_envy import FeatureEnvy
from fix_engine.fixers.husk_fixes import HuskFixes
from fix_engine.fixers.magic_literal import MagicLiteral
from fix_engine.fixers.mechanical_fixes import MechanicalFixes
from fix_engine.fixers.parameter_objects import ParameterObjects
from fix_engine.fixers.partition import Partition
from fix_engine.fixers.record_fixes import RecordFixes
from fix_engine.fixers.renaming import Renaming
from fix_engine.fixers.rule_table import RuleTable
from fix_engine.fixers.wide_tuple import WideTuple
from fix_engine.imports import Imports
from fix_engine.method_extraction import MethodExtraction
from fix_engine.module_outline import ModuleOutline
from fix_engine.module_source import ModuleSource
from fix_engine.rewrite import Rewrite
from fix_engine.source_files import SourceFiles
from fix_engine.strewing_group import StrewingGroup

_ModuleProposal = _records._ModuleProposal
_SplitCtx = _records._SplitCtx
_SplitLayout = _records._SplitLayout
_SplitOutcome = _records._SplitOutcome
# the shared mechanics stay reachable from the package root: consumers and the
# tests import them from fix_engine
_as_range = ModuleOutline.span
_as_node = ModuleOutline.resolve_node
_iter_nodes = ModuleOutline.iter_nodes
_iter_nodes_pruned = ModuleOutline.iter_nodes_pruned
_class_body_binds = Bindings.class_body_binds
_expr_names = Bindings.expr_names
_collect_alias_names = Bindings.collect_alias_names
_is_keyword_name = ModuleOutline.is_keyword_name
_is_self_attribute = ModuleOutline.is_self_attribute
_import_base_name = Bindings.import_base_name
_is_docstring_stmt = ModuleOutline.is_docstring_stmt

_py_files = SourceFiles.py_files
_record_class_def = Rewrite.record_class_def
_self_attr_target = Rewrite.self_attr_target
_hoist_target_names = Bindings.hoist_target_names
_extraction_is_private = Rewrite.extraction_is_private
_RemoveNodes = Rewrite._RemoveNodes

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
    "wide-tuple": "fix_wide_tuple",
    "data-clump": "fix_data_clump",
    "partition": "fix_partition",
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

    def fix_wide_tuple(self) -> str | None:
        """The wide-tuple shape arm: the annotation becomes a record class."""
        return WideTuple(self).run()

    def fix_data_clump(self) -> str | None:
        """The data-clump shape arm: the shared pair becomes one object."""
        return DataClump(self).run()

    def fix_partition(self) -> str | None:
        """The partition shape arm: the field-disjoint class splits."""
        return Partition(self).run()

    def fix_extract_class(self) -> str | None:
        """Move the strewing group into a class named after its shared receiver.

        An `extract-class` directive on a shape anchor tries each shape arm in
        turn: the wire record carries only (kind, file, line), and nothing but
        the anchor's shape can tell a tuple from a clump from a partition."""
        fixed = ClassExtraction(self).run()
        if fixed is not None or self.kind != "extract-class":
            return fixed
        for arm in (self.fix_wide_tuple, self.fix_data_clump, self.fix_partition):
            self.decline = None
            shaped = arm()
            if shaped is not None:
                return shaped
        self.decline = None  # a probe's reason is not this finding's
        return None

    def fix_dissolve_husk(self) -> str | None:
        """Dissolve the delegating husk: rewire its callers, delete the husk."""
        return HuskFixes(self).run()

    def fix_collapse_chain(self) -> str | None:
        """Collapse the forwarding chain: rewire or promote the terminal."""
        return ChainFixes(self).run()

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
        module, wrapper = ModuleSource.parse(self._loaded_source())
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
            referenced |= Bindings(top_defs[name]).free_names()
        if referenced & (set(top_defs) - move):
            return None  # the moved code needs an origin def — cycle risk
        # module-level assignments/constants the moved code reads would be
        # missing from the new module (a runtime NameError) — refuse; moving
        # them would break the origin's remaining code (review finding)
        if referenced & Bindings.module_bindings(module):
            return None
        new_module = cst.Module(
            body=[*Imports(module).needed(referenced), *(top_defs[n] for n in opts.params)]
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
        """Rewrite the dispatch chain as a registry of selector -> handler."""
        return DispatchRegistry(self).fix_dispatch_registry()

    def fix_rule_table(self) -> str | None:
        """Rewrite the rule battery as a table of (condition, violation)."""
        return RuleTable(self).fix_rule_table()

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


# the orchestrator
# lucidlint: ignore data-clump the shared params are the scan layer's operations on one file — the class boundary is
# the orchestrator
# lucidlint: ignore data-clump the shared params are the scan layer's operations on one file — the class boundary is
# lucidlint: ignore data-clump the (module, positions, line) triple is the analyzer's input record
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
    return Bindings(cls).free_names() - {cls.name.value}


def _free_names_of(stmt) -> set[str]:
    """The names one module-level statement reads from the module scope
    (bindings inside it excluded) — the residual's references to moved
    classes and constants."""
    return Bindings(stmt).free_names()


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
    value_refs_of = {n: Bindings.expr_names(v) - Bindings.BUILTINS for n, v in value_of.items()}
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
        and not Bindings.class_body_binds(cls, n)
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
    insert = 1 if body and ModuleOutline.is_docstring_stmt(body[0]) else 0
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
        external = {name for name in self.const_names if self._const_imported_elsewhere(name)}
        return {
            id(subject): _consts_moving_into(self.const_stmts, class_refs, residual_refs, external, subject)
            for subject in self.subjects
        }

    def _const_imported_elsewhere(self, name: str) -> bool:
        """Is the constant imported or attribute-read from another file? A moved
        constant must be sole-owned by its class (BQ6) — an external reader would
        break."""
        origin_path = _module_parts(self.origin.rel)
        for path in SourceFiles(self.repo).py_files():
            if path.relative_to(self.repo).as_posix() == self.origin.rel:
                continue
            try:
                mod = cst.parse_module(path.read_text(encoding="utf-8"))
            except Exception:
                continue
            importer_dir = path.relative_to(self.repo).as_posix().split("/")[:-1]
            alias_map = _module_alias_map(mod, origin_path)

            probe = _ConstImportScan(importer_dir, alias_map, origin_path, name)
            mod.visit(probe)
            if probe.hit:
                return True
        return False

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
            refs |= Bindings.expr_names(_const_value(stmt)) & self.subject_names
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
        for path in SourceFiles(self.repo).py_files():
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
        if ModuleOutline.is_empty(origin_source) and not _origin_imported_anywhere(
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
    free = _class_free_names(cls) - ctx.moved - Bindings.BUILTINS
    imports = Imports(module).needed(free)  # verbatim copies for import-bound names
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
    i = 1 if body and ModuleOutline.is_docstring_stmt(body[0]) else 0
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
    for path in SourceFiles(repo).py_files():
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


# --------------------------------------------------------------------------- dispatch-registry


# ambient names a dispatch arm may read without a handler parameter: the
# builtins + underscore dunders (the module's imports are a hand-apply case)


# per-arm bound + read name analysis result — the fix's scope check
# (refused when sibling-bound reads are detected)


# --------------------------------------------------------------------------- rule-checks


# ------------------------------------------------- unnamed-condition: named decisions
# Each names one decision a fixer makes inline, so the guard reads as the
# question it asks rather than a list of operators.


def _is_import_line(stmt: cst.BaseStatement) -> bool:
    """A line holding exactly one import."""
    return (
        isinstance(stmt, cst.SimpleStatementLine)
        and len(stmt.body) == 1
        and isinstance(stmt.body[0], (cst.Import, cst.ImportFrom))
    )


def _is_single_name_assign(stmt: cst.BaseSmallStatement) -> bool:
    """`name = value` with exactly one plain-name target."""
    return isinstance(stmt, cst.Assign) and len(stmt.targets) == 1 and isinstance(stmt.targets[0].target, cst.Name)


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


def _functions_collide(first, second, free_of: dict, attrs_of: dict) -> bool:
    """Two functions cannot share one moved cluster: one calls the other, or
    both touch the same state."""
    return (
        second.name.value in free_of[id(first)]
        or first.name.value in free_of[id(second)]
        or bool(attrs_of[id(first)] & attrs_of[id(second)])
    )

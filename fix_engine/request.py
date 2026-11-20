"""One fix request, and the dispatch that answers it.

The request carries what a fix needs — which file, which kind, where, with what
options — and every structural kind is a one-line delegate to the fixer that
owns it. The kind tables live here because they are the dispatch's vocabulary.
"""

from __future__ import annotations

from dataclasses import dataclass, field
from pathlib import Path

from fix_engine.fix_options import FixOptions
from fix_engine.fixers.chain_fixes import ChainFixes
from fix_engine.fixers.class_extraction import ClassExtraction
from fix_engine.fixers.data_clump import DataClump
from fix_engine.fixers.dispatch_registry import DispatchRegistry
from fix_engine.fixers.feature_envy import FeatureEnvy
from fix_engine.fixers.husk_fixes import HuskFixes
from fix_engine.fixers.magic_literal import MagicLiteral
from fix_engine.fixers.mechanical_fixes import MechanicalFixes
from fix_engine.fixers.module_extraction import ModuleExtraction
from fix_engine.fixers.module_splitting import ModuleSplitting
from fix_engine.fixers.parameter_objects import ParameterObjects
from fix_engine.fixers.partition import Partition
from fix_engine.fixers.record_fixes import RecordFixes
from fix_engine.fixers.renaming import Renaming
from fix_engine.fixers.rule_table import RuleTable
from fix_engine.fixers.wide_tuple import WideTuple
from fix_engine.method_extraction import MethodExtraction

# lucidlint: ignore-file global-state the module constants are this tool's configuration tables —
# lucidlint: ignore-file global-state a config class is the eventual home

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



KIND_ALIASES = {
    "latent-class": "extract-class",
    "complexity": "extract-method",
    "large-function": "extract-method",
}



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

    def fix_split_module(self) -> str | None:
        """Split the module so every public class lives in a file named after it."""
        return ModuleSplitting(self).run()

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

    def _extract_module_proposal(self):
        """Compute the extract-module split without writing it — the preview."""
        return ModuleExtraction(self).proposal()

    def fix_extract_module(self) -> str | None:
        """The apply side of extract-module — writes the new module."""
        return ModuleExtraction(self).run()

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

# Plan — the fix-engine split: 246 definitions → one class per fixer

The last ladder finding in this repo is `fix_engine/__init__.py`:
`many-definitions` at 246 definitions (174 functions, 72 classes, 22 of them
records). The orchestrator half is done (`docs/plans/plan-issue-107-module-shape.md`,
Round 3); this plan is the fix engine's turn. It is a behaviour-preserving
refactor: no rule, threshold, message or fixer behaviour changes.

## Where the file stands

`fix_engine.py` moved to `fix_engine/__init__.py` (so `import fix_engine` is
unchanged), and the shared libcst layer is out: `fix_engine/cst_mechanics.py`
holds `CstMechanics` — node positions, class-body bindings, module-level
names, def removal, the compiler-shorthand tests. Eighteen names left the
root there; 246 remain.

The definitions are one **hub plus seven families**. `_FixRequest` is the hub
every fixer entry hangs off; the measured closure of each family (references
followed through every helper and transformer, the hub excluded because
everything names it) is:

| family | definitions | entry kinds |
|---|---|---|
| mechanical | 45 | stale-suppression, noop-statement, unreachable, positional-literals, duplicate-def, restating-docstring, duplicate-block, undeclared-attribute, loop-pipeline, loop-sequence, loop-hoist |
| method extraction | 25 | extract-method |
| record | 17 | tuple-record, extract-record-class |
| class extraction (strewing) | 58 | extract-class |
| shape arms | (~40, inside the 58) | wide-tuple, data-clump, partition |
| parameter object | 10 | long-param-list |
| magic literal + rename | 9 | magic-number, vague-name |
| feature envy | 15 | feature-envy |
| composite | 112 | dissolve-husk, collapse-chain, split-module, extract-module, dispatch-registry, rule-table |
| shared (already out) | 16 | — |

Two numbers need untangling before the split is one-class-per-concern:

- **class extraction's 58 includes the three shape arms.** `_STRUCTURAL_FIXERS`
  routes `wide-tuple`/`data-clump`/`partition` through `fix_extract_class`,
  which tries each arm in turn. Map those kinds straight to their own fixers
  and the router disappears; strewing then keeps ~20 definitions.
- **composite's 112 is six concerns**: husk, chain, module split, module
  extract, dispatch registry, rule table. Each is its own class.

## Target layout

Infrastructure at the package root (6 files), fixers in a subpackage (17
files). Both folders stay under the `flat-folder` bar.

| module | class | owns |
|---|---|---|
| `fix_engine/__init__.py` | — | the public surface: the kind tables (`MECHANICAL_KINDS`, `STRUCTURAL_KINDS`, the fixer registry, `FIXABLE_KINDS`, `PREVIEW_KINDS`, `KIND_ALIASES`), the facade re-exports, `FixOptions` |
| `cst_mechanics.py` | `CstMechanics` | done |
| `records.py` | — | the private records (`_ModuleProposal`, `_ChainHead`, `_SplitLayout`, the partition/clump plans, `_ModuleRef`, …) — a module of records is exempt; a record its fixer alone uses may live in that fixer (`MechanicalFixes._LoopPipelinePlan`) |
| `fix_options.py` | `FixOptions` | the agent-supplied bits a fix may need |
| `strewing_group.py` | `StrewingGroup` | the strewing seam record |
| `request.py` | `_FixRequest` (name kept: the tests import it) | the request, `propose_finding`/`fix_finding`, and the dispatch table |
| `fixers/mechanical_fixes.py` | `MechanicalFixes` | the mechanical kinds + the loop rewrites |
| `fixers/method_extraction.py` | `MethodExtraction` | the seam analysis, `_FnBodyState`, `_Analyse`, the extract/insert rewrites |
| `fixers/record_fixes.py` | `RecordFixes` | tuple-record, extract-record-class |
| `fixers/class_extraction.py` | `ClassExtraction` | strewing only |
| `fixers/wide_tuple.py`, `data_clump.py`, `partition.py` | `WideTuple`, `DataClump`, `Partition` | the three shape arms |
| `fixers/parameter_objects.py` | `ParameterObjects` | parameter object |
| `fixers/magic_literal.py`, `renaming.py` | `MagicLiteral`, `Renaming` | magic number, rename |
| `fixers/feature_envy.py` | `FeatureEnvy` | the envied-receiver move |
| `fixers/husk_fixes.py`, `chain_fixes.py` | `HuskFixes`, `ChainFixes` | dissolve-husk, collapse-chain |
| `fixers/module_splitting.py`, `module_extraction.py` | `ModuleSplitting`, `ModuleExtraction` | split-module, extract-module |
| `fixers/dispatch_registry.py`, `rule_table.py` | `DispatchRegistry`, `RuleTable` | the two table rewrites |

Naming is part of the work: `class-module` accepts a class whose name equals
the file stem with underscores removed (`MechanicalFixes` in
`mechanical_fixes.py`), so every file above is named after its one class.

Every fixer class holds the request as state (`self.request`) and its helpers
as instance or static members; its CST transformers are **nested classes** —
a nested type is the fixer's implementation, not a second concern. That also
keeps `static-husk` quiet (the class has state) and keeps each module at one
top-level definition.

## The move, per family

The support layer is already named, so each family is the same mechanical
step:

1. **Compute the family's closure** (the script in this round's notes; the hub
   excluded) and assign every name exactly one owner. A name referenced by two
   families belongs in `CstMechanics` (shared mechanism) — never duplicated.
2. **Qualify references** before cutting: every `ast.Name` in *Load* context
   that matches a moved name becomes `<Owner>.<name>` (definition lines are not
   `Name` nodes, so `def x(` is untouched; recursive calls are qualified like
   any other use).
3. **Cut the definitions into the class**: functions become members,
   transformer classes become nested classes, records go to `records.py`.
4. **Turn the request's fixer method into a delegate**:
   `return MechanicalFixes(self).run()`; the fixer class's methods read the
   request's fields as `self.request.kind`, `self.request.repo`, … The
   request keeps only the pieces a preview/apply needs.
5. **Move the file-level whys with their code.** `fix_engine/__init__.py`
   carries reviewed `ignore-file` whys for `global-state` (the kind tables),
   `misplaced-method` (the repo helpers), and `record-shape` (the libcst
   wire shorthands). Each moves to the module whose code it covers — a why
   left behind reads `stale-suppression`; a why copied to a module that does
   not fire reads the same. No *new* why is added.

## Order (one commit per family)

Small and leaf-first, so each step is provable on its own:

| step | move | why this order |
|---|---|---|
| 1 | `records.py`, `fix_options.py`, `strewing_group.py` | pure data; no logic risk |
| 2 | `request.py` | the hub moves whole — nothing else changes yet |
| 3 | `mechanical_fixes.py` | the largest independent family; no other fixer calls it |
| 4 | `method_extraction.py` | self-contained seam analysis |
| 5 | `record_fixes.py` | small, uses `CstMechanics` |
| 6 | `wide_tuple.py`, `data_clump.py`, `partition.py` + retarget the fixer registry | removes the router from strewing |
| 7 | `class_extraction.py` | now strewing only |
| 8 | `parameter_objects.py`, `magic_literal.py`, `renaming.py`, `feature_envy.py` | small, independent |
| 9 | `husk_fixes.py`, `chain_fixes.py` | share the rewiring helpers — move together or with a shared owner |
| 10 | `module_extraction.py`, `module_splitting.py` | the import/constant rewriting; depends on 9's removers |
| 11 | `dispatch_registry.py`, `rule_table.py` | leaf rewrites |
| 12 | the entry: keep only imports, the kind tables and the facade | the ladder finding is gone here |

**Progress (2026-11-19).** Steps 1 and 4 are landed: `records.py` /
`fix_options.py` / `strewing_group.py` (22 records and the options out) and
`method_extraction.py` (`MethodExtraction`, 18 definitions, the request keeps
three delegates). The package root is at 209 definitions; behaviour is
unchanged (houses 353 / album 431 identical, 318 tests, `GATE: PASS`). The
remaining order is unchanged; the mechanical family is next.

**Update (2026-11-20).** The host shim is gone (constructor injection at the
composition root; `lucidlint.py` publishes `main` only). The engine is at 120
definitions: mechanical, record, the four small fixers, the shared
cross-family mechanics (parse+wrap, function-line/free-name visitors,
membership probe, moved-imports builder — now in `CstMechanics`), the dispatch
registry and the rule table are out. Remaining: class extraction/strewing, the
three shape arms, husk, chain, module split, module extract, the request hub,
and the few helpers those still share. `_STRUCTURAL_FIXERS` is the next
untangling: map `wide-tuple` / `data-clump` / `partition` straight to their own
fixers so strewing's closure stops including them.

**Mechanical family — tried and reverted (2026-11-19).** The move itself works
(38 definitions, tests green, root 209 → 178), but it produced six findings
that are design decisions, not mechanical fixes. The next pass starts here:

1. `private-import`: `records._LoopPipelinePlan` must be reached the way the
   package root does (`from fix_engine import records` + attribute), not by
   importing an underscore symbol.
2. `global-state` ×2: `_HOIST_MUTATORS` / `_OTHER_MUTATORS` become class
   attributes of `MechanicalFixes` (the `CstMechanics.BUILTINS` pattern).
3. `stale-suppression` ×2: the inline `ignore complexity` / `ignore
   data-clump` whys travelled with the loop code but no longer fire at the
   new site — they are dropped, not copied.
4. `latent-class` in `__init__.py`: the split-module helpers' `(name, repo)`
   parameter pair surfaces once the code around it moves; it is the same data
   clump the reviewed `ignore` in the old file covered, and belongs with the
   split-module family when that family moves (a named record or its why).

None of the six is a behaviour change; they are the per-family cleanup the
"Rules this split will meet" table predicted.

**Mechanical family — landed (2026-11-19).** `fix_engine/fixers/mechanical_fixes.py`
now holds `MechanicalFixes`: the four request methods and 31 top-level
definitions (39 in all), with the loop transformers and the delete/keyword/
annotation visitors as nested classes. The package root is at 178 definitions.
The six findings were resolved without a suppression:

- the two `global-state` constants are class attributes;
- the stale `complexity`/`data-clump` whys were deleted with their code (the
  file-level whys stay: their code is still at the root);
- `latent-class` was the real seam: the split-module lookups now take the
  `_ModuleRef(repo, name)` record from `records.py` instead of restating the
  pair in five signatures;
- `private-import` and the import cycle it would have caused are gone by
  moving `_LoopPipelinePlan` into `MechanicalFixes` itself. The plan's
  "`records.py` holds every record" rule gave way to the import graph: a fixer
  module may not import the package root (`from fix_engine import records`),
  and the only user of that plan is this fixer.

Verification: 318 tests, `GATE: PASS` (592 actions, 0 fails), houses 353 /
album 431 with identical `(file, kind, message)` sets, `make bundle-check` and
`make wheel-check` green. The moved findings (duplicate blocks, loop-pipeline,
magic numbers, the duplicate visitors) moved with their code; the total did
not grow.

The remaining families and their order are unchanged.

**Step 8 — the small independent fixers — landed (2026-11-19).**
`fixers/parameter_objects.py` (`ParameterObjects`), `fixers/magic_literal.py`
(`MagicLiteral`), `fixers/renaming.py` (`Renaming`) and
`fixers/feature_envy.py` (`FeatureEnvy`) now own their request method and its
closed helpers; each `_FixRequest.fix_*` is a one-line delegate. The package
root is at 147 definitions. `_Collaborator` and `_StmtReads` moved into
`FeatureEnvy` (its only user) for the same import-graph reason as
`_LoopPipelinePlan`. Verification: 318 tests, `GATE: PASS` (592 actions, 0
fails), houses 353 / album 431 identical.

## Landed: the split is done (2026-11-20)

The ladder report is empty. `lucidlint.py --repo .` reports **587** actions,
none of them `several-definitions`, `many-definitions`, `static-husk`,
`private-import`, `record-shape`, `latent-class` or `stale-suppression`.
`fix_engine/__init__.py` holds no definition at all: it is the public surface
(the kind tables, the request, the shared mechanics and the method-extraction
helpers re-exported for consumers and the tests).

| step | class | module |
|---|---|---|
| shared | `Imports`, `Rewrite` | `imports.py`, `rewrite.py` (`cst_mechanics.py` deleted) |
| 6 | `WideTuple`, `DataClump`, `Partition` | `fixers/wide_tuple.py`, `data_clump.py`, `partition.py` |
| 7 | `ClassExtraction` | `fixers/class_extraction.py` |
| 9 | `HuskFixes`, `ChainFixes` | `fixers/husk_fixes.py`, `chain_fixes.py` |
| 10 | `ModuleSplitting`, `ModuleExtraction` | `fixers/module_splitting.py`, `module_extraction.py` |
| 12 | `_FixRequest` | `request.py` |

Decisions the move forced, each a finding the code raised once its neighbours
left:

- **`CstMechanics` was two nouns.** The imports a moved name needs became
  `Imports(module).needed(names)`; the edits every fixer makes (remove the
  flagged nodes, synthesize the record class, name an extraction) became
  `Rewrite(module).without(...)` plus its statics. Both hold the module they
  work on, so neither is a static husk.
- **`records.py` is gone.** A fixer module may not import a private record
  (that is `private-import`) and may not import the package root (that is the
  import cycle). Every record a fixer alone used therefore lives beside it —
  nested in its class (`ChainFixes._ChainHead`, `ModuleSplitting._SplitLayout`,
  `WideTuple._Site`, `DataClump._Clump`, `Partition._PartitionPlan`, …) — and
  the one shared record, `_ModuleRef`, became `ChainFixes`'s own when the
  split's constant probe turned into a method.
- **The shared layer grew three members**, each because two families asked the
  same question: `Bindings.binds_name` (the three shape arms' "is this class
  name taken?"), `Bindings.stores_by_function` with `StoredNames` (strewing and
  data-clump's shadow test), `Rewrite.pascal_case` (the three arms' derived
  class name), and `ModuleOutline.is_empty` (the chain's and the split's
  "did the removal leave a file worth keeping?").
- **`_InitPlan` is a named record**, not a bare three-tuple: partition's only
  `record-shape` finding, fixed rather than suppressed — that also kept the
  record-shape census under its bulk-suppression threshold.
- **The re-export whys moved with their code.** `record-shape` now sits on
  `bindings.py`, `data_clump.py`, `chain_fixes.py` and `module_splitting.py`
  (the libcst wire shorthands those modules carry), `global-state` on
  `request.py` (the kind tables) and on `__init__.py` (the re-export aliases).
  No why was added whose code does not fire; the four that stopped firing
  (root's `data-clump`, `complexity`, `misplaced-method`, `closures` leftovers)
  were deleted.
- **`extract-class` keeps its shape fallback.** The kind table now routes
  `wide-tuple` / `data-clump` / `partition` to their own fixers, but an
  `extract-class` directive on a shape anchor still tries the three arms in
  turn (the wire record carries only kind/file/line) — that dispatch stayed in
  the request, where dispatch belongs.

**One artefact worth knowing.** `ModuleSplitting._Split.member_sources` is no
longer reported by the loop family: its two build loops were two
`loop-pipeline` findings at the root and are gone at the new site (the file's
other three loops — `_consts_moving_into`, `components`, `refuse_cycles` —
still fire). The cause is the nesting the move gave it: the same method inside
a *top-level* class reports both loops, inside a class nested one level deeper
it reports neither. No code changed; the count drops by two (402 → 400 in the
self-scan).

Verification of the final tree: `make check` (ruff + pyrefly, baseline lock
clean), `make test` (318), `make self-check` (`GATE: PASS`), `make wheel-check`,
`make bundle-check`, and houses **353** / album **431** with identical sorted
`(file, kind, line)` sets.

## Verification, after every step

1. `make check` (ruff + pyrefly; run `make typecheck-update-baseline` only
   when a *moved* line changes a baselined error's position — the lock is
   both-direction).
2. `make test` (318 tests; the fix-engine suite exercises every fixer).
3. `lucidlint.py --repo . --json`: no new finding anywhere. Line shifts are
   fine; a new `(file, kind, message)` is not. Expected transient: the
   `many-definitions` message's count drops as the root shrinks.
4. Houses **353** / album **431**: identical sorted `(file, kind, line)` sets
   (the snapshot files from this round).
5. `make self-check` (the gate must stay `GATE: PASS`).
6. `make wheel-check` and `make bundle-check` at the packaging-sensitive
   steps (the package already ships; only the entry's imports change).

## Rules this split will meet

| rule | what it says here | the answer |
|---|---|---|
| `several`/`many-definitions` | the target: ≤ 2 definitions per module | one class per module, records in `records.py` |
| `class-module` | a public class's file is named after it | the naming table above |
| `static-husk` | a class of only staticmethods is a namespace | every fixer holds `self.request` |
| `flat-folder` | 20+ files in one folder | fixers in `fixers/`, infrastructure at the root |
| `record-shape` | a constant-key dict is an unnamed record | wire payloads stay named records; the reviewed whys travel |
| `duplicate-block` / `standard` duplicates | pre-existing copies inside transformers | they move with their code; the count must not grow |
| `layer-mix` (graph) | a file whose functions call into two modules | all infrastructure stays at `fix_engine`'s root, so a fixer's calls are one layer |
| `misplaced-method` / `latent-class` / `closures` | pre-existing reviewed whys | carried, per step 5 |

## Risks and the stop rule

- **Class-body evaluation.** A nested class's base, decorator or default
  argument is evaluated while the owner class body runs, before the owner is
  bound. Before a move, list every reference to a moving name that sits in
  class-body position (not a method body); those few stay module-level in the
  owner's module or are re-ordered.
- **`self` changes meaning** only for the request's fixer methods; each is
  verified by its own tests.
- **Circular imports**: `request.py` imports the fixers; a fixer imports the
  request only under `TYPE_CHECKING` (annotations are strings under
  `from __future__ import annotations`).
- **Stop rule.** If a family cannot move without changing behaviour or
  packaging, leave it where it is, record the seam and the evidence in this
  document, and ship the rest. A family that is still on the hub is a smaller
  finding than a broken fixer.

## Acceptance

- [x] `lucidlint.py --repo .` reports zero `several-definitions` and
      `many-definitions` findings, and `GATE: PASS` with 0 fails.
- [x] `make check`, `make test`, `make self-check`, `make wheel-check`,
      `make bundle-check` green.
- [x] Houses 353 / album 431, identical finding sets.
- [x] No suppression added; every moved why still covers its code.
- [x] One commit per family, each naming the class it created.

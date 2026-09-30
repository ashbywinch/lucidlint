# Plan — resolve GitHub issues #26–#34 (branch `fix/github-issues-26-34`)

All nine issues were raised by @app/omp-harness (enhancements). Each was
verified against the actual code before acceptance; findings that failed
verification are called out below. Judgment calls pending user ruling live in the
"Open decisions" section; rulings already made are recorded there as decided.

WRITING RULE (2026-09-30): every Phase bullet is a task, not a concept — state
the concrete action (which file/function to change, what to add/emit/read, what
the acceptance check is), then the constraints. A bullet that only names data or
intent without the action is a finding; rewrite it. (docs/writing-documentation.md
task-shaped sections.)

TEST-FIRST RULE (docs/testing-standards.md "Write the test before the code"): every
change ships with its failing test written FIRST — run it red, then implement, then
green. Phase 5 is the final GATE, not the testing phase: the per-change tests are
written inside Phases 2-4, one red->green per rule/fixer/render change. No
project-wide test runs mid-round (they cannot be green until the round's changes
land); the gate runs them once at the end.

## Verified facts (grounding)

- `duplicate` (checks.rs:5597) pairs **functions** only — modules/classes invisible (#26 real).
- `class_module_findings` (checks.rs:2675) returns when `classes.len() != 1` — a
  multi-class module never checks whether its classes live in files named after them
  (#31 real; fixture `class_module_matching_name_and_multi_class_pass` pins it).
- extract-method refuses nested targets (`fix_engine.py:260`, `_is_nested_target`) —
  nested-closure shape is invisible to the fix engine (#32 detection gap real).
- `god_class_findings` (checks.rs:2524) is pure size (warn): >=20 methods or >=12 over >=250
  lines. No partition check in it.
- `partition_findings` (checks.rs:6232) EXISTS and is **fail** — partitions a class's
  **methods** over its fields, but guards at >=6 methods AND >=150-line span.
- #29(b) premise is wrong: the 25-method/53-line probe class (span 53 < 150) was never
  evaluated by the partition rule — it was rejected at the span gate. Escalation to fail
  means building a NEW small-span partition check, not "acting on the partition verdict".
- #32 evidence is internally contradictory: "tools/server.py, 37 methods, GATE: PASS with
  zero actions" — 37 >= 20 ⇒ god-class MUST fire warn. Either suppressed in that repo or
  inaccurate. Rule shape itself remains real.
- #29(a) "guard undocumented" is FALSE — RULES.md documents `>=20 methods, or >=12 methods
  over >=250 lines`. "Not configurable" is true; no threshold in the tool is configurable
  (deliberate, documented model: suppress-with-a-why).
- Catalog description of `partition` is stale: documented as "free functions partition a
  struct's fields"; implementation partitions a class's METHODS. RULES.md inherits the lie
  (generated from the catalog) — this is why #29 misreads the rule.
- No new rule may break `make self-check` — the tool scans its own repo; new families
  firing on lucidlint itself get refactored or baselined-with-a-why.
- House registration discipline: every family = one `Rule` row in `rule_metadata.py`
  CATALOG; `make rules` regenerates `rules_gen.rs`, RULES.md, RULE_GROUPS. Drift gate
  (`make rules --check` + pytest) fails on catalog/emission disagreement.

## Phase 0 — Baseline

Point: the branch must start from a proven-clean state, so any failure later in the
round is attributable to THIS work, not to pre-existing breakage.

1. Branch off main (`9219dc5`) as `fix/github-issues-26-34`. (Done — exists at
   `c8ccaec`, plan commits only; working tree clean.)
2. Run `make self-check` — acceptance: `GATE: PASS` with 0 fails (the repo's ~557
   pre-existing warnings never gate; leave them alone).
3. Run `make test` — acceptance: lint + typecheck + 334 Rust + 185 pytest green.

## Phase 1 — Catalog (`rule_metadata.py` → `make rules`)

Point: `rule_metadata.py` is the tool's single registration point — every family's
kind, severity, section, display group and name-flag lives here ONCE, and `make
rules` derives `scanner/src/rules_gen.rs`, RULES.md and the Python RULE_GROUPS from
it. Getting the catalog right first keeps the derived artifacts (and the drift gate
that pins them) coherent for everything that follows.

1. **Field renames** (naming ruling 2026-09-29): in the `Rule` dataclass —
   `Rule.display_group` (the RULES.md SECTION: architecture/style/...) becomes
   `Rule.section`; `Rule.display` (the report BUCKET) becomes `Rule.display_group`,
   with `None` = belongs to no group, stands alone under its own kind; `"standard"`
   = the catch-all group; any other value = a family group (`latent-class`, `docs`,
   `loop-pipeline`). Update: the catalog rows' kwarg names, `gen-rules.py` (`bucket
   = display_group or kind`; group indexing by `section`), the `_CONFIG_GROUP` map,
   RULES.md static text. Then `make rules`.
2. **Add the new families** per the table below — kind, severity, section
   (`architecture` unless noted), `display_group` as listed; **add the fix registry
   (Fifth-pass)**: `FixParam`/`Fix` models, and each row's `fix=` link — the new
   rules carry `fix=None` (their fixes ship in Phase 3 with the fixer work:
   dissolve-husk, collapse-chain, split-module). Replace the boolean flags with the
   fix links on the existing name-required rows (large-function, partition,
   strewing, wide-tuple join the registry via their `fix=` values). Fix the
   stale `partition` description — it says "free functions partition a struct's
   fields"; the implementation partitions a class's METHODS over its fields.
3. `make rules` — regenerates `rules_gen.rs` + RULES.md + RULE_GROUPS.

New families (severities per Open decisions 1 and 5):

| Issue | Kind | display_group | Fix kind |
|---|---|---|---|
| #26 | `duplicate-module` | `None` (standalone) | none (reconciliation is judgment) |
| #27 | `static-husk` | `None` | none |
| #27 | `delegating-husk` | `None` | dissolve-husk (rewire callers) |
| #28 | `process-class` (supersedes the dropped single-callsite; dead-class arm folds into `unused`) | `None` | none (apply the domain test: fold/rename/delete) |
| #30 | `forwarding-chain` | `None` | collapse-chain (multi-file, identity-gated) |
| #31 | extend `class-module` (no new kind) | existing (`"standard"`) | split-module (new multi-file fixer) |
| #32 | `closure-cluster` | `"latent-class"` | none (extraction refused by design) |

Constraint: judge-status is a property of the linked fix's PARAMS (Fifth-pass) —
there are no boolean name flags and no maintained kind list.

Acceptance: `make rules` is idempotent; the catalog rows match the table exactly;
Phase 5's drift check is run AFTER Phase 2 lands (every family in the table needs a
Phase 2 emission before `make rules --check` passes — expected to fail mid-round).

## Phase 2 — Scanner (Rust)

Each emission lives in `scanner/src/checks.rs` (or `rustscan.rs` where it is
Rust-shaped), is registered per Phase 1, and ships TEST-FIRST: for each rule write
the failing test first — the fixture in `tests/fixtures/rust/` (the include_str!
convention; NOT `scanner/tests/fixtures/`, which does not exist), the scanner unit
test asserting the emission, the suppression test (`lucidlint: ignore` + config
`ignore`), and the orchestrator test where gate behavior changes — run it RED, then
implement the emitter, then GREEN. The same red->green order applies to the Phase 3
fixers and the Phase 4 render changes.

- **#26 duplicate-module** — Point: a fork — two modules or classes with the same
  structural skeleton — is drift waiting to happen: a fix lands in one copy and the
  other silently keeps the old behavior. The function-level `duplicate` rule cannot
  see a whole-module fork (its unit is a function); this rule names the fork as ONE
  decision.
  Actions: (1) extend the skeleton machinery (`BigramInterner` ~checks.rs:5620,
  `dice_from_bigrams` ~5655) to modules and classes — skeleton = `NAME = literal`
  constants (token `NAME:norm(type)`) + statement bigrams, bigrams never crossing
  member boundaries; pair identities module-module, class-class, module-class (a
  module whose non-import top-levels are exactly one class def). (2) Gates: skeleton
  >=2 statements AND >=12 tokens (mirror the function rule); file gate >=3
  statements. (3) Exclusions: `__init__.py`, tests, vendored dirs — add a
  `VENDOR_DIRS` path-component list (`vendor`, `third_party`, `thirdparty`,
  `node_modules`, `site-packages`); `SCAN_SKIP_DIRS` has no vendor entries. (4)
  Track per-skeleton member-name sets and emit their INTERSECTION as "matched
  members:" in the message — the bigram Dice counts cannot produce the list. (5)
  Emit `duplicate-module`, `fail`, threshold 0.9 hardcoded.
  Constraints: no configurable threshold (ruling, Open decisions 3);
  `# lucidlint: ignore-file duplicate-module <why>` works per-file.
  Acceptance: a fixture pair with >=0.9 skeletons and identical constants emits one
  fail finding naming the intersecting members; a <3-statement file never fires;
  `ignore-file` silences it.
- **#27 static-husk** — Point: a class with no state whose members are all
  staticmethods is a namespace wearing a domain noun — it presents an object's shape
  while owning nothing, and the reader expects state that isn't there (the
  GedcomDocument shape: the records it should hold live outside it).
  Actions: new emitter — class where (no `self.X =` anywhere, no dataclass fields,
  no properties) AND every member is a `@staticmethod`, with >=1 method -> kind
  `static-husk`, `warn`.
  MESSAGE makes the fix direction explicit (2026-09-30): "Class X has no state of
  its own and only staticmethods — a namespace wearing a domain noun. The fix:
  find the state these operations work on and put it IN the class (fields + method
  bodies reading them); if there is no such state, the operations still belong
  with SOME abstraction or another — possibly more than one: the class(es) that
  own the state they work on. Finding it is the task; reconsider whether THIS
  class is the correct abstraction."
  Exclusions (none of these fire): inherited base (state may live in the base), ABC/
  Protocol (pure interfaces), `pass`-only subclass (re-export marker),
  `*Error`/`*Exception` (exception namespace).
  Acceptance: stateless static-only fixture fires warn; each exclusion shape passes.
- **#27 delegating-husk** — Point: a class where every method forwards to a module
  function adds an indirection layer and no behavior — callers could reach the
  functions directly, and the class name is a namespace for others' work (the
  Memory shape).
  Actions: new emitter — class where every instance method's body is exactly
  `return F(<all params>)` with F a module-level function -> kind `delegating-husk`,
  `warn`. Depth >=2 chains are #30's shape (hand them off). Fix: dissolve — rewire
  the husk's callers to the module functions, delete the husk (Phase 3).
  Constraints: facade opt-out is a plain `lucidlint: ignore delegating-husk <why>`;
  NEVER inline the functions' bodies into the husk.
  Acceptance: husk fixture fires warn; a depth-2 M->F->G shape produces a
  `forwarding-chain` finding instead.
- **#28 process-class** — Point: a process-named class (Builder, Validator,
  Importer...) usually labels work that belongs ON the domain objects it operates
  on — the name is a verb wearing a noun; the reader cannot tell the thing from the
  doing. (single-callsite is DROPPED — 2026-09-30: construction count is irrelevant
  to lucidity.)
  Actions: new emitter — top-level class name in the ENUMERATED process set
  (Builder, Importer, Exporter, Validator, Converter, Parser, Reader, Writer,
  Fetcher, Collector, Handler, Manager, Provider, Renderer, Serializer, Dispatcher,
  Processor, Analyzer, Scheduler, Runner, Executor, Authenticator, ... a general
  `-er` test would fire on real nouns) MINUS the GoF lexicon (Visitor, Iterator,
  Command, Factory, Strategy, Proxy, Adapter — the domain's own pattern names) ->
  kind `process-class`, `fail`. Message = the four-part honesty test (Fourth-pass
  4). No fix directive: fold/rename/delete is judgment, so not name-required and
  unstamped.
  Dead-class arm: extend `unused` to CLASSES — defs list gains class definitions;
  "never referenced" via the existing prod_refs/test_refs machinery; test-used is
  not dead. `warn`.
  Acceptance: a RefValidator-shaped fixture fires fail with the honesty-test message
  (parts 1-4 present); GoF names pass; a never-referenced class fires `unused`;
  test-only-referenced passes.
- **#31 class-module multi** — Point: the file system is a name index — a class
  belongs in a file named after it so a reader can find the concept by name. A
  multi-class module in which any public class's name does not match the stem
  leaves that class unfindable; a matching sibling excuses nothing (ruling
  2026-09-30).
  Actions: in `class_module_findings` (checks.rs:2675) — (1) remove the
  `classes.len() != 1` returns; (2) remove the DUPLICATED `__init__.py`/len block at
  checks.rs:2687-2693 (merge artifact); (3) add the condition: module (non-test,
  not `__init__`) with >=2 public classes where AT LEAST ONE public class's name
  does not match the stem (case-insensitive, plural-underscore form ok) -> `fail`;
  (4) the message names the MISPLACED classes; (5) keep the tool-script
  (`has_module_fns`) and `__init__` exemptions; (6) the message ends
  `— fix: split-module`.
  Acceptance: fixture migration — INVERT
  `class_module_matching_name_and_multi_class_pass` (main.rs:3485: User/Team,
  neither matches) into the new arm's FINDING test; add a PASS fixture where EVERY
  class matches the stem; add a mixed fixture (one matches, one does not -> finding
  names only the misplaced).
- **#32 closure-cluster** — Point: a method whose nested closures partition its
  locals is a class-in-a-method — the split is structurally visible, but extract-
  method refuses nested targets, so no rule sees the shape today. `warn`: the shape
  is often a deliberate cohesive serving method (the message can be wrong; forced
  extraction can hurt).
  Actions: new emitter — per method, build the local-var x nested-fn bipartite
  graph; >=2 connected components, each with >=2 nested fns and >=1 distinct local
  -> kind `closure-cluster`, `warn`, display `latent-class`. Degenerate arm:
  empty-param method >=150 lines with nested defs. Fix-side: the extract-method
  refusal (fix_engine.py:260-261) currently returns `(None, None)` with NO decline
  text — ADD decline-text plumbing on the Python side using the >=2-nested-def
  proxy ("nested target with >=2 closures (closure-cluster shape) — extract by
  hand"): the cluster graph lives in the scanner; the fix engine sees one file and
  must NOT reimplement the detection.
  Constraints: co-reporting with `closures`/`large-function` is ACCEPTED (dedupe
  across families is out of scope); the proxy being weaker than the rule condition
  is accepted (the citation is a hint, not a claim the rule fired).
  Acceptance: a boxjig.main-shaped fixture fires warn; a nested closure-cluster
  extract-method request returns the decline text.
- **#30 forwarding-chain detection** — Point: method -> module function -> method at
  depth >=2 is dead indirection when the layers already know each other — each just
  re-expresses the next. BUT when the chain is the ONLY coupling between two
  otherwise-independent modules, it is a deliberate boundary: the modules meet at a
  function so neither side knows the other's classes. Then there is NO finding and
  NO message.
  Actions: structural same-repo resolution — method M body exactly `return F(<all M
  params>)` (or F as the sole expression); module fn F body exactly `return N(<all F
  params>)`; N a method on ANOTHER class D; depth >=2 edges. Emit `forwarding-chain`,
  `fail`, ONLY when C's module already depends on D's module (import/reference
  present, or same module).
  Constraints: no graph tool required; boundary case = silent.
  Acceptance: coupled fixture fires fail; a server/CLI-style independent-modules
  chain emits nothing.
- **#34 structured seam** — Point: the report must be able to show "these N
  findings are ONE design decision" (issue #34). That needs the clump member sets as
  structured data — never scraped from message prose.
  Actions: (1) add `seam_members: Vec<String>` to the `Finding` struct
  (scanner/src/common.rs); populate in `data_clump_findings` (~2150 — the function
  names currently joined into the message's names list, plus each pair's member
  names), `partition_findings` (~6232 — the method names in the disjoint groups),
  `strewing_findings` (~2790 — the functions sharing the leading parameter); all
  other emitters leave it empty. (2) Serialize: `main.rs`'s per-finding `json!`
  block adds `"seam_members": <field>`; nothing else changes. (3) Orchestrator:
  bump the contract check 3 -> 4 (lucidlint.py:825), update the pinning test
  (test_lucidlint.py pins schema_version 3 inline), fix the stale "schema 2"
  mentions in docs/TECHSPEC.md and docs/PLAN.md; the reader maps the field into the
  Action, required on the three carriers, absent elsewhere — branch on the kind, no
  `.get` default (no-backwards-compat ruling).
  Acceptance: scanning a repo with a data-clump shows non-empty `seam_members` in
  `--json`; the Phase 4 grouping keys on it; old-schema output is rejected loudly.


## Phase 3 — Fix engine (Python, libcst; fix.rs for Rust where shape applies)

- **#30 collapse-chain — IN SCOPE (ruling 2026-09-29: full multi-file fix)**.
  Point: the chains Phase 2 flags are dead indirection inside already-coupled
  modules; the fix removes the redundant layer. The transform must NEVER blindly
  inline — the right fix depends on whose class is real, and a boundary chain must
  be refused, not collapsed.
  Actions (fix_engine.py; fix.rs for Rust shapes):
  1. Chain facts are re-derived repo-wide in libcst — the fixer already reads
     repo-wide (`_py_files`, `_name_occurrences`, `_repo_params`); the libcst
     re-derivation is pinned by the same rule tests as the Rust detector
     (documented duplication, NOT a silent second implementation).
  2. Per chain C.M -> F -> D.N (M, F pure forwarders, F single-caller):
     - DEPENDENCY GATE (2026-09-30): refuse when C's module does not already depend
       on D's module — collapsing would marry two independent modules (the chain is
       the deliberate boundary).
     - IDENTITY GATE: D has data members (`self.X` assigns, declared/dataclass
       fields) or any non-forwarding member -> D is real: **REWIRE, not inline** —
       M calls `D.N(<args>)` directly, F deleted (inlining would duplicate D's
       behavior into C and erase D's role).
     - D has NO identity -> N is a method in exile: **PROMOTE N to a sibling method
       of M on C** (`self.N(...)`); M's body becomes the call or M merges into N; F
       deleted; D dissolves — delete D only when ZERO external references to D
       (constructions, from-imports, isinstance, annotations, subclassing,
       attribute access), else D remains as an empty husk and #27 reports it.
       Promotion refused when N touches `self.<attr>` or self-methods. N's other
       callers: rewired only where a C instance provably exists; otherwise
       **INLINE** N's body into M as the fallback (preserves N). M merge: after
       rewiring, if M is unreferenced, fold M into N and delete M; else keep M as
       `return self.N(...)`.
     - REFUSALS (all shapes): >1 caller of F; `*args`/`**kwargs` passthrough;
       callee N in third-party code (no source); keyword-name collisions after
       substitution; any `from mod import F` re-export reference (deleting F breaks
       the importer).
  3. Multi-file transaction: `_FixRequest` gains `extra_writes: list[(rel,
     source)]` and `deletes: list[rel]`; the orchestrator applies origin + extras +
     deletes, then verifies via the REPO-WIDE scan (NOT `scan_single_file`) that no
     forwarding-chain remains. Iterate per edge until no chain remains (R9); params
     pass through positionally-in-order or all by identical keyword names with
     defaults re-supplied.
  Acceptance: the server/CLI boundary chain refuses with the dependency reason; a
  coupled chain collapses (rewire or promote, never blind inline) and the repo-wide
  re-scan shows zero `forwarding-chain` findings.
- **#31 split-module — IN SCOPE (ruling 2026-09-29)**.
  Point: misplaced classes belong in files named after them, but the layout must be
  the RIGHT one for the classes — a cohesive set is a package, independent classes
  are flat files; the choice is correctness, never safety convenience (both layouts
  are deterministic and safe).
  Actions (fix_engine.py, libcst):
  1. The split's SUBJECT = the public classes whose names don't match the stem; a
     stem-matching class STAYS in the origin.
  2. Layouts: PACKAGE = `mod.py` -> `mod/` with `<class>.py` per subject class +
     `__init__.py` re-exporting (`from mod import Cls` / `mod.Cls` keep resolving,
     ZERO caller rewrites; NO constants in `__init__.py` — third-pass 5: they move
     into the owning class). FLAT = `<class>.py` siblings; rewrite `from old
     import Cls` -> `from new import Cls` and `old.Cls` -> `new.Cls` repo-wide to a
     fixed point; `mod.py` REMAINS as the residual module (functions, constants,
     private classes, stem-matching class), deleted iff it becomes empty.
  3. DECISION RULE (cohesion only, no container-word test, 2026-09-29): cluster the
     subject by edges = cross-class member references OR shared class-level
     attributes. Per cluster: all misplaced classes one cluster -> PACKAGE, named by
     the stem when free, else via `--name <package>`; cohesive sub-cluster (>=2
     misplaced, edges among them) -> PACKAGE via `--name <package>`; singleton ->
     FLAT file. The "closely related models" carve-out stays in the message.
  4. REFUSALS (transform-safety only — decline with a reason, the agent applies by
     hand): an existing `mod/` directory when flat is also impossible; module-level
     executable statements BETWEEN classes (splitting would reorder them); an import
     cycle in the candidate layout — cycles among the produced class files; the
     `__init__` re-export/constant edge is exempt by construction (constants-first
     ordering); a target file/dir collision; relative imports in `mod.py`
     (`from .x import ...`) refuse PACKAGE (fall to flat — the path root changes).
     `--name` is required when the package name isn't derivable: DECLINE when
     absent (never flat-for-convenience).
  5. Safety: per-file py_compile + the rule's own re-scan showing every remaining
     class's stem matches.
  Acceptance: fixtures cover flat-singleton split, whole-cluster package by stem,
  sub-cluster package via `--name`, mixed module (stem class stays, misplaced move),
  and each refusal case; the re-scan shows zero `class-module` findings.

## Phase 4 — Orchestrator (lucidlint.py; Action model)

- **#33(a) stamps — render-time only, no stored flag (2026-09-30)**. Point: agents
  must treat name-committing fixes as judgment calls (the reader supplies the name)
  and mechanical rewrites as applicable; the report marks which is which. The
  name-requirement is NOT per-finding data — it is a property of the finding's
  kind's linked fix (Fifth-pass: `fix_of(signal).params` declares the required
  `name`). NO
  `Action.judgement` field, NO JSON `judgement` field, no marker appended to any
  message: the finding whose fix needs a name already SHOWS it — its own fix
  directive reads `--name <Name>`. That IS the per-finding signal.
  Actions (lucidlint.py render, one lookup — on the STRUCTURED `signal` field,
  NEVER message text):
  1. Judge-true iff the rule's linked fix declares a required `name` param
     (`fix_of(signal).params` — Fifth-pass). A flagged data-clump matches via its
     row's `fix="extract-class"`; a partition finding matches on its raw signal,
     not its display kind `latent-class`. The directive's `--name` text is NOT
     parsed; the fix's params are the source of truth.
  2. TEXT: the kind header renders `JUDGEMENT` for judge-true findings,
     `MECHANICAL` for fixable kinds outside it (loop-pipeline/loop-sequence,
     positional-literals, stale-suppression, noop-statement, unreachable,
     duplicate-def, restating-docstring, duplicate-block, undeclared-attribute);
     fix-less findings get neither.
  3. Stamps are computed at render from the signal — nothing stored per action.
  Acceptance: a complexity finding renders `[JUDGEMENT]`; a loop-pipeline finding
  renders `[MECHANICAL]`; scanner message text is byte-identical to today's (the
  stamp is renderer-side).
- **#33(b) NAMING notice** — Point: the naming lesson is issued ONCE per report, not
  copied onto every message, so it exists without inflating the report (ruling
  2026-09-29). The lesson teaches what `--name <Name>` is asking for.
  Actions (lucidlint.py render): print the notice once at the top of the findings
  list (after the header) iff ANY finding is judge-true (`fix_kind_of(signal)` in
  NAME_REQUIRED_KINDS — the same structured lookup as the stamp, never message
  text). No per-finding reference beyond the directive itself. Text: the four-point
  NAMING
  notice (2026-09-29 draft, research-sourced): names must be what the domain calls
  the THING; verb names (-er/-or) name a process — stateful process = name the
  state; stateless = the operations belong to the abstraction that owns their state
  — find it; an -er/-or is honest only when the domain calls a stateful component
  that and no existing type already is it; generic containers
  (Options/Context/Parameters/Config) and tool jargon (Seam/Clump/Accumulator) name
  the means, not the thing. JSON: `meta.naming_notice` present iff the trigger
  fires. Constraint: scanner messages stay clean; the LSP's lack of the notice is a
  recorded decision, no doc edit this round.
  Acceptance: a report with one name-required finding prints exactly one notice;
  `--json` meta carries it; no name-required finding -> no notice.
- **#33(c) name-suffix nudge — HELD (ruling 2026-09-29). Nothing to build**: no
  post-hoc note; naming is steered first-time by the notice + stamp; decision D
  (no second-guessing names) governs.
- **#33(d) existence check — folded into the notice's point 1. Nothing to build.**
- **#34 clustering** — Point: N findings sharing one seam are ONE design decision;
  the report must show that so the agent designs the target type once instead of
  minting N half-baked types (the issue's 62-findings case).
  Actions (lucidlint.py render layer, per rule R5):
  1. Seam modes: (a) complexity/large-function -> (file, function); (b) the
     carriers (data-clump/partition/strewing) -> their `seam_members` sets (Phase
     2 field).
  2. Group by union-find transitive closure over overlapping member sets (pairwise
     overlap merges; cross-kind groups allowed — a data-clump and a partition over
     the same class share a seam).
  3. A group of >=3 findings prints the heading: "these N findings share ONE seam —
     design the target type once, for all of them (the seams: ...)".
  4. JSON: `groups` array ALWAYS present (empty when no cluster); item =
     `{heading, seam, finding indices into actions}`. Ordering: groups and members
     by (file, line) of first/inner findings, deterministic across runs. No
     "non-breaking" language (moot under the no-backwards-compat ruling).
  Acceptance: the issue's 6-clump scenario renders one heading; `--json` `groups`
  contains the cluster; two runs produce identical output.

## Phase 5 — The final gate

Point: everything the round touched is proven green TOGETHER for the first time —
the per-change red->green already happened inside Phases 2-4 (TEST-FIRST RULE). This
phase runs the full suite once, applies the self-check hygiene, and opens the PR.

1. `cargo test` green — includes every per-rule scanner unit test written first in
   Phase 2.
2. `make rules --check` (drift — run AFTER every Phase 2 emission and the Phase 1
   catalog land together; expected red mid-round); `make check` (ruff + pyrefly);
   `pytest` green — includes the per-rule suppression + orchestrator tests.
3. `make self-check` — the new families scan lucidlint's own repo; expect hits
   (process-named classes in lucidlint.py/fix_engine.py; closure-cluster/static-husk
   on fix_engine.py). Hygiene: suppress each hit with a
   `lucidlint: ignore <signal> <why>` comment at the site — baselines CANNOT carry
   whys (bare kind:file:function keys, lucidlint.py:1661) and new WARN findings
   never gate; the round ends with no unsuppressed hit (the house standard, not the
   gate).
4. `make coverage` refresh; commit; PR to main (house pattern:
   `fix/github-issues-21-22-23` -> PR #24).

Acceptance: 1-4 all green; coverage refreshed; PR opened against main.

## Open decisions (pending rulings marked; decided rulings recorded)

1. **Severity policy — DECIDED (2026-09-29)**. Governing principle, user-stated:
   "Everything should be an error unless it's really possible that the message is wrong
   and the code might be more maintainable and understandable without the change."
   Findings default to `fail`; downgrade to `warn` only when BOTH hold — the message
   could plausibly be wrong, AND the code may be genuinely better without the change.

   Applied:
   - `duplicate-module`: **fail** — Dice>=0.9 + identical constants is a fork,
     near-unmistakable; forking is drift risk.
   - `forwarding-chain`: **fail** — exact forwarder bodies are structurally verifiable;
     collapsing removes dead indirection (a deliberate facade opts out via
     suppression-with-why). GATED (2026-09-30): no finding when the chain is the only
     coupling path between two otherwise-independent modules — the message can be
     wrong and the code is better with the boundary (severity principle).
   - `closure-cluster`: **warn** — nested handlers over disjoint locals are often a
     deliberate cohesive serving method (the issue's own server.py is this shape);
     forced extraction can make handler wiring less readable.
   - god-class small-span escalation (#29(b)): **warn / no escalation** — the 150-line
     partition guard exists because small-span splits are noisy; new machinery would
     gate on a signal that can be wrong.

   #29 therefore closes with: the stale `partition` catalog description fix + the
   closure-carried-state gap covered by `closure-cluster` (#32). No (a)/(b) code.

2. **#31 automated split fix — DECIDED (2026-09-29): full automated split in scope** —
   the `split-module` fixer ships this round (see Phase 3). The issue marks the fix
   "optional"; user chose to include it.
3. **Configurable thresholds — DECIDED (2026-09-29): NOT supported.** Both proposals
   die: #29(a) `god-class-min-*` and #26 `duplicate-module-similarity`. Thresholds stay
   in the binary; the escapes remain per-finding suppression with a why, `ignore-file`,
   config `ignore` (census-visible), and `[lucidlint.guidance]`. Rationale, recorded:
   the agent's "undocumented" claim was false; the underlying pains (noise on cohesive
   classes, blindness to closure state) are rule-quality problems a knob cannot fix; a
   knob would be the tool's first policy-hidden-in-TOML escape hatch, defeating the
   suppression census that makes mass-hiding visible.
4. **#33(c) name-suffix nudge — DECIDED (2026-09-29): HELD (option A)**. No post-hoc
   note. Naming is steered FIRST TIME by the once-per-report NAMING notice + the
   `JUDGEMENT` stamp (see Phase 4 #33(b)); the recorded decision D ("naming stays a
   judgment call the tool must not second-guess") governs. The nudge's ropey-name
   triage is instead the notice's content, issued before the name exists.
5. **Judgement set — DECIDED (2026-09-29): anything where the user must provide a
   name.** Implemented via the fix registry (Fifth-pass): judge-true iff the rule's
   linked fix declares a required `name` param. Includes feature-envy, magic-number,
   loop-hoist (the "name-required but mechanical" carve-out does not exist);
   mechanical := fixable without a user-supplied name; unstamped := fix-less. See
   Phase 4 #33(a).
6. **Backward compatibility — DECIDED (2026-09-29): NOT supported.** The scan
   contract is strictly versioned (existing pattern: `lucidlint.py:825` rejects a
   mismatched `schema_version`); contract changes bump the schema and old output is
   rejected loudly. No additive-with-default compat seams — the #34 seam field rides
   the 3->4 bump (see Phase 2 #34), and the reader does not `.get`-default new
   contract fields.
## Second critique — resolutions (2026-09-29)

Second cold critique (agent://PlanCritique2) verified 11 first-critique items resolved;
the 12 below remained mechanical gaps. These resolutions SUPERSEDE the corresponding
Phase text where they conflict.

1. **#28 process-class (R1) — supersedes the single-callsite spec** (dropped
   2026-09-30: construction count is irrelevant to lucidity). Detection: top-level
   class name in the enumerated process set (Builder, Importer, Exporter, Validator,
   Converter, Parser, Reader, Writer, Fetcher, Collector, Handler, Manager, Provider,
   Renderer, Serializer, Dispatcher, Processor, Analyzer, Scheduler, Runner, Executor,
   Authenticator, ...) MINUS the GoF pattern lexicon (Visitor, Iterator, Command,
   Factory, Strategy, Proxy, Adapter). Severity fail. No automated fix; the message
   carries the four-part honesty test. `fix=None` (fold/rename/delete is judgment),
   so unstamped.
2. **#28 dead-class arm (R2)** — extend `unused` (warn, Python) from functions to
   classes: "class X is never referenced anywhere in the repo" — same reference
   machinery (prod_refs/test_refs), same test-seam treatment (test-used is not dead;
   annotations count as references). No construction counting exists or is added.
3. **#31 public + exemptions (R3)** — "public" = name not starting with `_`. The
   tool-script exemption (`has_module_fns`) applies to BOTH arms. split-module moves
   the MISPLACED public classes only (those whose names don't match the stem); the
   stem-matching public class and private classes stay in the origin (in FLAT:
   `mod.py` keeps them; in PACKAGE: they live in `__init__.py`).
4. **#26 skeletons + matched members (R4)** — Constant token = `NAME:norm(type)`
   (numeric literals normalised, strings stripped, bools canonical). Bigrams never
   cross member boundaries. Module-class pair: a module whose non-import top-level
   statements are exactly one class def qualifies (skeleton = class skeleton + module
   constants). Min skeleton gate: >=2 statements AND >=12 tokens (mirror the fn rule;
   the file >=3-statement gate stays as an additional floor). Vendored exclusion: new
   `VENDOR_DIRS = {vendor, third_party, thirdparty, node_modules, site-packages}`
   (path-component match; independent of SCAN_SKIP_DIRS which has no vendor entries).
   Matched members: maintain per-skeleton member-name sets and emit the INTERSECTION —
   the bigram Dice counts cannot produce the issue's "matched members:" list, so this
   is new bookkeeping beside `BigramInterner`.
5. **#34 clustering + field (R5/R6)** — Field NAMED `seam_members: Vec<String>`:
   present on data-clump/partition/strewing findings, absent elsewhere; the schema-4
   reader branches by kind and indexes WITHOUT a default on carriers (no `.get`).
   Seam modes: (a) complexity/large-function (extract-method fix) -> (file, function);
   (b) latent-class variants with `seam_members` -> member-set overlap. Grouping:
   union-find transitive closure over overlapping sets (pairwise overlap merges);
   cross-kind groups ALLOWED (a data-clump and a partition over the same class share a
   seam — one design decision, the issue's core case). Ordering: groups and members by
   (file, line) of first/inner findings. `groups` array ALWAYS present (empty when no
   cluster); item = {heading, seam, finding indices into `actions`}. The word
   "non-breaking" is dropped (moot under no-backwards-compat).
6. **#33 judgement mechanism (R6)** — Extend `fix_kind_of` (gen-rules.py) to map
   partition/strewing/wide-tuple -> extract-class (data-clump already mapped); add the
   missing `— fix: extract-class` directives to partition, wide-tuple and data-clump
   messages (strewing already advertises it). Judgement := fix_kind_of(signal) is in
   NAME_REQUIRED_KINDS, computed at render from the message directive (the R27 rewrite
   already parses it). No new Rule flags — respects the no-new-metadata ruling; the
   latent-class variants become judgement exactly as enumerated.
7. **#30 no-identity branch (R7)** — Delete D only when ZERO external references to D
   (constructions, from-imports, isinstance, annotations, subclassing, attribute
   access); otherwise D remains as an empty husk and #27 reports it. Promotion refused
   when N's body reads/writes `self.<attr>` or calls self-methods (D has no state by
   gate, so only self-method calls can appear -> refuse). N's other callers: rewired
   only where a C instance provably exists at the call site; otherwise inline fallback
   (preserves N). M merge: after rewiring, if M is unreferenced, fold M into N and
   delete M; else keep M as `return self.N(...)`.
8. **#30/#31/#27 multi-file transaction (R8)** — The fixer already performs repo-wide
   READS (`_py_files`, `_name_occurrences`, `_repo_params`); the fix-side detection is
   the libcst re-derivation of the chain/identity facts (pinned by the same rule tests
   as the Rust detector — documented duplication, not a silent second implementation).
   Result surface: `_FixRequest` gains `extra_writes: list[(rel, source)]` and
   `deletes: list[rel]`; the orchestrator applies origin + extras + deletes, then
   verifies via the REPO-WIDE scan (not `scan_single_file`) that the chain is gone.
9. **#30 chain grammar (R9)** — Terminal N is a method on another class OR a
   module-level function (a module-fn terminal at depth >=2 is the #27 handoff shape:
   M->F->G is a forwarding chain, flagged by #30). The fixer iterates per edge until no
   chain remains; the acceptance re-scan confirms all edges. Param pass-through: every
   param passed positionally in order, or all by identical keyword names; defaults must
   be RE-SUPPLIED in the forwarder's call (per the issue's own condition).
10. **#27 dissolve-husk (R10)** — REFUSE when: any reference to the husk name outside
    itself (imports, isinstance, annotations, subclassing, constructions — the instance
    escapes), or any call site that constructs the husk or uses the instance beyond one
    call (store/pass). Rewire stateless sites: `Husk().m(x)` / `h.m(x)` -> `m(x)`
    (hub has no state by definition, so the collapse is sound). Message advertises
    `— fix: dissolve-husk`; wire into _FIX_ALIASES/STRUCTURAL_KINDS + fix-command
    acceptance. Callers in test files are rewired too (repo-wide).
11. **#31 layout fate + constants + relative imports (R11)** — FLAT: `mod.py` REMAINS
    as the residual module (module-level functions, constants, private classes, and
    any stem-matching class); deleted iff it becomes empty; `from mod import CONST`
    from class files still resolves; `from mod import Cls` sites rewritten. PACKAGE:
    `__init__.py` holds constants + private classes + re-exports, constants FIRST so
    class-file `from . import CONST` binds; the import-cycle REFUSAL covers cycles
    among the produced class files ONLY — the __init__ re-export/constant edge is
    exempt by construction. REFUSE package layout (fall to flat) when `mod.py`
    contains relative imports (`from .x import ...`) — path root changes under a
    package.
12. **#32 refusal + LSP note (R12)** — Decline text: "nested target with >=2 closures
    (closure-cluster shape) — extract by hand"; the proxy being weaker than the rule
    condition is ACCEPTED and stated (the citation is a hint, not a claim the rule
    fired). The LSP gap (no NAMING notice/stamps in per-buffer diagnostics) is a
    recorded DECISION in this plan, not a dangling "documented gap" — no doc edit this
    round.
## Third-pass rulings (2026-09-29) — amend the sections above

1. **Judgement mechanism — the linked fix's params (Fifth-pass).** The boolean flag
   is replaced because parallel facts drifted (data-clump: flag without a fix;
   large-function: fix without a flag); the registry makes the mismatch impossible.
   The judgement SET (above) is unchanged. Consequence of the registry's drift gate:
   the directives must now match `Rule.fix` — the flag-without-directive state is
   replaced by the gate requiring `— fix: extract-class` on data-clump, partition,
   wide-tuple.
2. **Package layout never flattens for convenience.** A cohesive sub-cluster (subset
   holds cross-class member references or shared class-level attributes) also gets a
   package; when
   the package name is not derivable (sub-cluster), the split fixer takes it via
   `--name <package>` — the "name is the commitment" mechanism — and DECLINES when
   the name is absent. Flat layout ONLY for non-cohesive modules (independent
   classes, no shared internals).
3. **NEW RULE — module-level variables are a hard error.** Extend the `global-state`
   family (fail): fires on ANY module-level variable assignment — mutable or
   constant, mutated or not. Message: values are a class's private internals; the
   variable belongs as that class's attribute/member. Exception (carved out):
   global services containers and framework-required globals (`app = Flask(...)`,
   DI registries). The existing narrow condition (mutable container mutated inside a
   function) merges into the expanded one; one family, one message.
4. **magic-number fixer must stop prescribing module-top constants.** Its current fix
   inserts `MAX_RETRIES = 10` at module top (fix_engine.py:310-311) — a self-
   contradiction under rule 3. Change: insert the constant as a CLASS ATTRIBUTE of
   the enclosing class; when the literal is inside a module-level function (no class
   to own it), REFUSE with "the literal has no class to own its constant — move the
   function into a class first". This change ships in this round.
5. **#31/#34 interaction.** Package `__init__.py` holds NO constants (rule 3). During
   a split, each module-level constant moves into the class that uses it; shared
   module-level constants cannot exist post-rule, so the package/cluster cohesion
   signal is: cross-class member references + shared CLASS-LEVEL attributes.
   Class-attribute sharing across classes is a consolidation signal (those classes
   belong together), surfaced via the cluster seam, not a "copy to __init__" path.
6. **Self-check consequence.** The expanded global-state rule fires on this repo's own
   module-level constants (lucidlint.py, fix_engine.py, scanner emits) — handled per
   Phase 5 hygiene: refactor into class attributes or suppress-with-why. The round is
   not green until zero unsuppressed hits.
## Fourth-pass — the lucidity principle (2026-09-30)

The tool's entire purpose is code lucidity — code that is maintainable,
lucid, and obviously correct. Every rule, message, and fix serves that aim;
findings are POINTERS for the reader's judgment, never verdicts or orders.
Derived rule-design rule (dev agents): NEVER justify a rule, threshold, or
message from mechanics (deduplication, size, call counts) — a class with one
construction site can be exactly right. The judgment asked of the reader is
always: "what is the best way to make this code more maintainable, lucid,
and obviously correct?"

Encode it at the four surfaces (per docs/writing-documentation.md: one
canonical statement in RULES.md; AGENTS.md links, never copies; the header
is runtime output):

1. **Report header** — replace `common::REPORT_HEADER` (common.rs:959):
   "lucidlint — the aim is code that is maintainable, lucid, and obviously
   correct. Findings and suggested fixes are pointers, not orders: for each,
   judge the best way to make this code more maintainable, lucid, and
   obviously correct. Fix findings instead of suppressing them; give every
   suppression a why a reviewer can check."
   JSON carriage VERIFIED: scan emits `"header"` (main.rs:2073) ->
   orchestrator read (lucidlint.py:1386) -> `--json` meta "header"
   (lucidlint.py:171); text prints it (:218); LSP excluded by pinned test.
   The const's pinning test (common.rs:966-970, asserts "readable") updates
   to the new triplet.
2. **RULES.md preamble** — canonical statement, right after the title:
   "Every rule below exists for one reason: code that is maintainable,
   lucid, and obviously correct. A finding is a pointer, not a verdict —
   its message and suggested fix state the reasoning and exist to make the
   reader reflect. Judge each with common sense: what is the best way to
   make this code more maintainable, lucid, and obviously correct? When
   the suggested fix isn't the optimal class design for maintainability,
   draw on your knowledge of good class design to devise a better one.
   Suppress only where the finding itself
   does not apply — and give the why a reviewer can check."
3. **AGENTS.md Rules** — one rule, link, no copy: the tool exists for code
   lucidity; never design from mechanics (dedup, size, call counts);
   findings are pointers for the reader's judgment; the question is always
   "what is the best way to make this code more maintainable, lucid, and
   obviously correct?" (stated in RULES.md's preamble).
4. **process-class message (supersedes the #28 reframe — single-callsite dropped
   2026-09-30)**:
   "class X is a process (-er/-or name), not a thing: the work belongs on the
   domain objects it operates on. A process class is the best idea only when ALL
   hold — (1) the domain itself names it (the parser, the scheduler); (2) its
   state is its own and substantial, no actual noun can carry it; (3) no existing
   type already carries this work (extend or fold, never mint a twin); (4) it is
   not one owner's private device — a single consumer with no own state means the
   operations belong to the domain abstraction that owns their work, not to a
   class of their own. Mostly one fails: fold, rename, or delete."
## Fifth-pass — the fix registry (2026-09-30) — supersedes the flag mechanics

The boolean "name-required" list is a fragile parallel fact: data-clump shipped
flag-without-fix and large-function fix-without-flag — exactly what parallel truth
does. Replace it with a class structure: a FIX declares its parameters once; a RULE
links to its fix; "requires a name" is a property of the fix's parameter list.

1. **Models** (rule_metadata.py):
   - `FixParam(name: str, required: bool)` — one fix parameter.
   - `Fix(kind: str, params: tuple[FixParam, ...])` — one declaration per fix kind.
   - `Rule.fix: str | None` — the fix kind the rule's findings advertise (replaces
     the `fix_name_required` bool AND gen-rules.py's `fix_kind_of` override map: the
     three overrides become plain `fix=` values on the rows — data-clump
     `fix="extract-class"`, record-shape `fix="extract-record-class"`,
     module-cohesion `fix="extract-module"`; complexity/large-function
     `fix="extract-method"`; vague-name `fix="rename"`; long-param-list
     `fix="long-param-list"`; tuple-record `fix="tuple-record"`; magic-number
     `fix="magic-number"`; loop-hoist `fix="loop-hoist"`; fix-less rules `fix=None`).
   - `FixParam("name", required=True)` on: extract-method, extract-class,
     extract-record-class, extract-module, rename, tuple-record, long-param-list,
     magic-number, loop-hoist. No name param on the deterministic rewrites
     (loop-pipeline/loop-sequence, positional-literals, stale-suppression, the
     mechanical set).
2. **Derivations all read the same source**: judge-true iff the rule's linked fix
   declares a required `name` param (`fix_of(signal).params`). CLI --name gate,
   LSP needsName, render stamps (JUDGEMENT/MECHANICAL) and the NAMING-notice
   trigger all use it. NAME_REQUIRED_KINDS stops being maintained — generated from
   the params if any artifact still wants the list.
3. **Drift gate (new)**: every finding's message directive must equal its `Rule.fix`
   — the R27 contract, pinned by the existing drift test. The data-clump/large-
   function classes of mismatch become hard failures. This supersedes the
   "no message-directive changes" note for the MACHINE TAIL only: the directive's
   `--name <Name>` is the surface the registry pins; message prose stays untouched.
   (The flag-without-directive state for data-clump/partition/wide-tuple dies with
   this gate: their rows declare `fix="extract-class"`, so their directives must
   carry `— fix: extract-class`.)
4. **Acceptance**: a rule's judge-status flips by editing its row or the fix's
   params — one place; a fix taking `--name` tomorrow makes its findings judge-true
   with no second registration; the drift test fails on any directive/fix
   mismatch.